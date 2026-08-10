//! Parsed module imports for the TypeScript proof layer. Shared-entry
//! loading proofs need real import statements rather than substring
//! matches: comments, dead strings, and type-only clauses must never count
//! as runtime loading, so the sources are parsed with oxc and only actual
//! module records are reported.

use oxc_allocator::Allocator;
use oxc_ast::ast::{Argument, Expression};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::SourceType;
use serde::Serialize;

use crate::at_rules::parse_css;
use oxc_css_parser::ast::Statement;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceImport {
    specifier: String,
    /// True for `import type` / `export type` clauses and clauses whose
    /// specifiers are all inline `type` entries; erased at runtime, they
    /// never load stylesheets.
    type_only: bool,
    /// True for `import()` expressions and `require()` calls, which may sit
    /// behind control flow; a loading proof needs an unconditional static
    /// import, while reachability edges may still follow them.
    dynamic: bool,
}

struct ImportCollector {
    imports: Vec<SourceImport>,
}

impl<'a> Visit<'a> for ImportCollector {
    fn visit_import_declaration(&mut self, decl: &oxc_ast::ast::ImportDeclaration<'a>) {
        let type_only = decl.import_kind.is_type()
            || decl.specifiers.as_ref().is_some_and(|specifiers| {
                !specifiers.is_empty()
                    && specifiers.iter().all(|specifier| match specifier {
                        oxc_ast::ast::ImportDeclarationSpecifier::ImportSpecifier(named) => {
                            named.import_kind.is_type()
                        }
                        _ => false,
                    })
            });
        self.imports.push(SourceImport {
            specifier: decl.source.value.to_string(),
            type_only,
            dynamic: false,
        });
        walk::walk_import_declaration(self, decl);
    }

    fn visit_export_from_declaration(&mut self, decl: &oxc_ast::ast::ExportFromDeclaration<'a>) {
        let type_only = decl.export_kind.is_type()
            || (!decl.specifiers.is_empty()
                && decl
                    .specifiers
                    .iter()
                    .all(|specifier| specifier.export_kind.is_type()));
        self.imports.push(SourceImport {
            specifier: decl.source.value.to_string(),
            type_only,
            dynamic: false,
        });
        walk::walk_export_from_declaration(self, decl);
    }

    fn visit_export_all_declaration(&mut self, decl: &oxc_ast::ast::ExportAllDeclaration<'a>) {
        self.imports.push(SourceImport {
            specifier: decl.source.value.to_string(),
            type_only: decl.export_kind.is_type(),
            dynamic: false,
        });
        walk::walk_export_all_declaration(self, decl);
    }

    fn visit_import_expression(&mut self, expression: &oxc_ast::ast::ImportExpression<'a>) {
        if let Expression::StringLiteral(literal) = &expression.source {
            self.imports.push(SourceImport {
                specifier: literal.value.to_string(),
                type_only: false,
                dynamic: true,
            });
        }
        walk::walk_import_expression(self, expression);
    }

    fn visit_call_expression(&mut self, call: &oxc_ast::ast::CallExpression<'a>) {
        if let Expression::Identifier(callee) = &call.callee
            && callee.name == "require"
            && let Some(Argument::StringLiteral(literal)) = call.arguments.first()
        {
            self.imports.push(SourceImport {
                specifier: literal.value.to_string(),
                type_only: false,
                dynamic: true,
            });
        }
        walk::walk_call_expression(self, call);
    }
}

/// Parse one JavaScript or TypeScript source and return its module records
/// as JSON. A file that does not parse is an error, which the caller treats
/// as having no provable imports.
pub fn collect_source_imports_json(source: &str, path: &str) -> Result<String, String> {
    let source_type = SourceType::from_path(path).unwrap_or_default();
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return Err(format!("Failed to parse {path}"));
    }
    let mut collector = ImportCollector {
        imports: Vec::new(),
    };
    collector.visit_program(&parsed.program);
    serde_json::to_string(&collector.imports).map_err(|error| error.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CssDirective {
    name: String,
    /// The complete directive text including the at-keyword and prelude.
    text: String,
}

/// Top-level at-rule directives of a stylesheet. Tailwind honors `@source`
/// and `@import` only at the top level, so directive-shaped text inside
/// rule blocks or string values never counts.
pub fn collect_css_directives_json(source: &str) -> Result<String, String> {
    let allocator = oxc_css_parser::Allocator::default();
    let parsed = parse_css(&allocator, source, oxc_css_parser::Syntax::Css)
        .map_err(|error| format!("Failed to parse stylesheet: {error}"))?;
    let mut directives = Vec::new();
    for statement in &parsed.statements {
        let Statement::AtRule(at_rule) = statement else {
            continue;
        };
        let prelude_end = at_rule
            .block
            .as_ref()
            .map_or(at_rule.span.end, |block| block.span.start);
        directives.push(CssDirective {
            name: at_rule.name.name.to_string(),
            text: source[at_rule.span.start..prelude_end].trim().to_string(),
        });
    }
    serde_json::to_string(&directives).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    fn imports(source: &str, path: &str) -> Vec<(String, bool, bool)> {
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&super::collect_source_imports_json(source, path).unwrap())
                .unwrap();
        parsed
            .iter()
            .map(|import| {
                (
                    import["specifier"].as_str().unwrap().to_string(),
                    import["typeOnly"].as_bool().unwrap(),
                    import["dynamic"].as_bool().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn collects_runtime_and_type_only_imports() {
        let collected = imports(
            "import '../globals.css';\n\
             import type { A } from './a.ts';\n\
             import { type B } from './b.ts';\n\
             import Mixed, { type C } from './c.ts';\n\
             export type { D } from './d.ts';\n\
             export { E } from './e.ts';\n\
             const f = await import('./f.ts');\n\
             const g = require('./g.cjs');\n\
             // import './commented.css';\n\
             const dead = '../dead.css';\n",
            "/project/main.ts",
        );
        assert_eq!(
            collected,
            vec![
                ("../globals.css".to_string(), false, false),
                ("./a.ts".to_string(), true, false),
                ("./b.ts".to_string(), true, false),
                ("./c.ts".to_string(), false, false),
                ("./d.ts".to_string(), true, false),
                ("./e.ts".to_string(), false, false),
                ("./f.ts".to_string(), false, true),
                ("./g.cjs".to_string(), false, true),
            ]
        );
    }

    #[test]
    fn rejects_unparseable_sources() {
        assert!(super::collect_source_imports_json("import from from;", "/p/x.ts").is_err());
    }

    #[test]
    fn collects_top_level_css_directives_only() {
        let parsed: Vec<serde_json::Value> = serde_json::from_str(
            &super::collect_css_directives_json(
                "@import \"tailwindcss\" source(none);\n\
                 @source \"./packages/app\";\n\
                 .rule { content: '@source \"./inside-string\"'; }\n\
                 @media screen { .x { color: red; } }\n",
            )
            .unwrap(),
        )
        .unwrap();
        let directives: Vec<(&str, &str)> = parsed
            .iter()
            .map(|directive| {
                (
                    directive["name"].as_str().unwrap(),
                    directive["text"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            directives,
            vec![
                ("import", "@import \"tailwindcss\" source(none)"),
                ("source", "@source \"./packages/app\""),
                ("media", "@media screen"),
            ]
        );
    }
}

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
use oxc_css_parser::ast::{
    AtRulePrelude, ComponentValue, Function, FunctionName, ImportPreludeHref, InterpolableIdent,
    InterpolableStr, MediaQuery, Statement, UrlValue,
};
use oxc_css_parser::token;

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
            // An import attribute clause such as `with { type: 'css' }`
            // constructs a stylesheet object without applying it, so the
            // record cannot prove unconditional loading.
            dynamic: decl.with_clause.is_some(),
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

    fn visit_ts_import_equals_declaration(
        &mut self,
        decl: &oxc_ast::ast::TSImportEqualsDeclaration<'a>,
    ) {
        if let oxc_ast::ast::TSModuleReference::ExternalModuleReference(reference) =
            &decl.module_reference
        {
            self.imports.push(SourceImport {
                specifier: reference.expression.value.to_string(),
                type_only: decl.import_kind.is_type(),
                dynamic: false,
            });
        }
        walk::walk_ts_import_equals_declaration(self, decl);
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
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
enum CssDirective {
    /// A top-level `@import`, read from the parser's structured prelude.
    Import {
        /// The import target, or None when it cannot be read statically.
        specifier: Option<String>,
        /// True when the target is the tailwindcss package or a subpath.
        tailwind: bool,
        /// The `source(...)` modifier: "none", a literal path, or None
        /// when absent.
        source: Option<String>,
        /// True when a `source(...)` modifier exists but cannot be read.
        source_unreadable: bool,
    },
    /// A top-level `@source` directive, using Tailwind's own grammar over
    /// the parser-proven prelude text.
    Source {
        not: bool,
        inline: bool,
        scope: Option<String>,
        /// True when the prelude does not match the supported grammar.
        unreadable: bool,
    },
    Other {
        name: String,
    },
}

fn literal_str(value: &InterpolableStr<'_>) -> Option<String> {
    match value {
        InterpolableStr::Literal(literal) => Some(literal.value.to_string()),
        _ => None,
    }
}

fn import_directive(source_text: &str, at_rule: &oxc_css_parser::ast::AtRule<'_>) -> CssDirective {
    let Some(AtRulePrelude::Import(prelude)) = &at_rule.prelude else {
        return CssDirective::Import {
            specifier: None,
            tailwind: false,
            source: None,
            source_unreadable: false,
        };
    };
    let specifier = match &prelude.href {
        ImportPreludeHref::Str(value) => literal_str(value),
        ImportPreludeHref::Url(url) => match &url.value {
            Some(UrlValue::Raw(raw)) => Some(raw.value.to_string()),
            Some(UrlValue::Str(value)) => literal_str(value),
            _ => None,
        },
        ImportPreludeHref::Function(_) => None,
    };
    let tailwind = specifier
        .as_deref()
        .is_some_and(|spec| spec == "tailwindcss" || spec.starts_with("tailwindcss/"));
    let mut source = None;
    let mut source_unreadable = false;
    // A lone `source(...)` parses into the media-query list as a structured
    // function; mixed with other modifiers such as `prefix(...)` the whole
    // tail becomes a raw token sequence in `modifiers`. Both positions are
    // read.
    if let Some(media) = &prelude.media {
        for query in &media.queries {
            if let MediaQuery::Function(function) = query {
                read_source_function(function, &mut source, &mut source_unreadable);
            }
        }
    }
    if let Some(modifiers) = &prelude.modifiers {
        let mut structured = false;
        for value in &modifiers.values {
            if let ComponentValue::Function(function) = value {
                structured = true;
                read_source_function(function, &mut source, &mut source_unreadable);
            }
        }
        if !structured {
            read_source_tokens(source_text, &modifiers.values, &mut source, &mut source_unreadable);
        }
    }
    CssDirective::Import {
        specifier,
        tailwind,
        source,
        source_unreadable,
    }
}

fn read_source_function(
    function: &Function<'_>,
    source: &mut Option<String>,
    source_unreadable: &mut bool,
) {
    let FunctionName::Ident(InterpolableIdent::Literal(name)) = &function.name else {
        return;
    };
    if name.name != "source" {
        return;
    }
    match function.args.as_slice() {
        [ComponentValue::InterpolableIdent(InterpolableIdent::Literal(ident))]
            if ident.name == "none" =>
        {
            *source = Some("none".to_string());
        }
        [ComponentValue::InterpolableStr(path)] => match literal_str(path) {
            Some(path) => *source = Some(path),
            None => *source_unreadable = true,
        },
        _ => *source_unreadable = true,
    }
}

/// Read `source(...)` from a raw modifier token sequence: an ident named
/// `source`, a parenthesis, then either the `none` ident or a quoted path.
fn read_source_tokens(
    source_text: &str,
    values: &[ComponentValue<'_>],
    source: &mut Option<String>,
    source_unreadable: &mut bool,
) {
    let tokens: Vec<(&token::TokenData, &str)> = values
        .iter()
        .filter_map(|value| match value {
            ComponentValue::TokenWithSpan(with_span) => Some((
                &with_span.token,
                &source_text[with_span.span.start..with_span.span.end],
            )),
            _ => None,
        })
        .collect();
    let mut index = 0;
    while index < tokens.len() {
        let (data, text) = tokens[index];
        let is_source = matches!(data, token::TokenData::Ident(_)) && text == "source";
        if !is_source {
            index += 1;
            continue;
        }
        let wrapped = matches!(tokens.get(index + 1), Some((token::TokenData::LParen(_), _)))
            && matches!(tokens.get(index + 3), Some((token::TokenData::RParen(_), _)));
        match tokens.get(index + 2) {
            Some((token::TokenData::Ident(_), text)) if wrapped && *text == "none" => {
                *source = Some("none".to_string());
            }
            Some((token::TokenData::Str(_), text)) if wrapped => {
                *source = Some(text.trim_matches(['"', '\'']).to_string());
            }
            _ => *source_unreadable = true,
        }
        index += 1;
    }
}

/// Parse a `@source` prelude with Tailwind's grammar: an optional `not`,
/// then a quoted scope or an `inline(...)` list.
fn source_directive(prelude: &str) -> CssDirective {
    let mut rest = prelude.trim();
    let mut not = false;
    if let Some(stripped) = rest.strip_prefix("not") {
        if stripped.starts_with(char::is_whitespace) {
            not = true;
            rest = stripped.trim_start();
        }
    }
    if rest.starts_with("inline(") {
        return CssDirective::Source {
            not,
            inline: true,
            scope: None,
            unreadable: false,
        };
    }
    let scope = rest
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            rest.strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        });
    match scope {
        Some(scope) if !scope.is_empty() => CssDirective::Source {
            not,
            inline: false,
            scope: Some(scope.to_string()),
            unreadable: false,
        },
        _ => CssDirective::Source {
            not,
            inline: false,
            scope: None,
            unreadable: !rest.is_empty(),
        },
    }
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
        match at_rule.name.name {
            "import" => directives.push(import_directive(source, at_rule)),
            "source" => {
                let prelude_end = at_rule
                    .block
                    .as_ref()
                    .map_or(at_rule.span.end, |block| block.span.start);
                let text = &source[at_rule.span.start..prelude_end];
                let prelude = text.trim_start().trim_start_matches("@source");
                directives.push(source_directive(prelude));
            }
            name => directives.push(CssDirective::Other {
                name: name.to_string(),
            }),
        }
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
             import H = require('./h.ts');\n\
             import sheet from './i.css' with { type: 'css' };\n\
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
                ("./h.ts".to_string(), false, false),
                ("./i.css".to_string(), false, true),
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
                 @import \"tailwindcss/utilities\" prefix(tw) source(\"./apps\");\n\
                 @import url(./theme.css);\n\
                 @source not \"./packages/app\";\n\
                 @source inline(\"width-lte-700px:hidden\");\n\
                 .rule { content: '@source \"./inside-string\"'; }\n\
                 @media screen { .x { color: red; } }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed,
            serde_json::json!([
                {
                    "kind": "import",
                    "specifier": "tailwindcss",
                    "tailwind": true,
                    "source": "none",
                    "sourceUnreadable": false
                },
                {
                    "kind": "import",
                    "specifier": "tailwindcss/utilities",
                    "tailwind": true,
                    "source": "./apps",
                    "sourceUnreadable": false
                },
                {
                    "kind": "import",
                    "specifier": "./theme.css",
                    "tailwind": false,
                    "source": null,
                    "sourceUnreadable": false
                },
                { "kind": "source", "not": true, "inline": false, "scope": "./packages/app", "unreadable": false },
                { "kind": "source", "not": false, "inline": true, "scope": null, "unreadable": false },
                { "kind": "other", "name": "media" }
            ])
            .as_array()
            .unwrap()
            .clone()
        );
    }
}

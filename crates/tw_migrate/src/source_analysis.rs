//! Parsed JavaScript and TypeScript facts for the TypeScript proof layer.
//! Shared-entry loading proofs and Vue analysis need real syntax rather
//! than substring matches: comments, dead strings, and type-only clauses
//! must never count as runtime loading, so the sources are parsed with oxc
//! and analyzed semantically before any fact is reported.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, BindingPattern, Expression, IdentifierReference,
    ImportDeclarationSpecifier, VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_syntax::symbol::SymbolId;
use serde::Serialize;

use crate::js_rewrite::source_type_for_path;

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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DefaultImportBinding {
    source: String,
    local: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionAnalysis {
    static_string: Option<String>,
    vue_module_member: Option<String>,
    uses_css_module: bool,
}

struct ExpressionCollector {
    uses_css_module: bool,
}

impl<'a> Visit<'a> for ExpressionCollector {
    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        if identifier.name == "$style" {
            self.uses_css_module = true;
        }
        walk::walk_identifier_reference(self, identifier);
    }
}

pub fn expression_analysis_json(path: &str, source: &str) -> Result<String, String> {
    let allocator = Allocator::default();
    let expression = Parser::new(&allocator, source, source_type_for_path(path)?)
        .parse_expression()
        .map_err(|diagnostics| format!("Failed to parse {path}: {diagnostics:?}"))?;
    let static_string = match &expression {
        Expression::StringLiteral(literal) => Some(literal.value.to_string()),
        Expression::TemplateLiteral(template) if template.expressions.is_empty() => template
            .quasis
            .first()
            .and_then(|quasi| quasi.value.cooked.as_ref())
            .map(ToString::to_string),
        _ => None,
    };
    let vue_module_member = match &expression {
        Expression::StaticMemberExpression(member)
            if matches!(&member.object, Expression::Identifier(object) if object.name == "$style") =>
        {
            Some(member.property.name.to_string())
        }
        _ => None,
    };
    let mut collector = ExpressionCollector {
        uses_css_module: false,
    };
    collector.visit_expression(&expression);
    serde_json::to_string(&ExpressionAnalysis {
        static_string,
        vue_module_member,
        uses_css_module: collector.uses_css_module,
    })
    .map_err(|error| error.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceAnalysis {
    imports: Vec<SourceImport>,
    static_imports: Vec<String>,
    default_imports: Vec<DefaultImportBinding>,
    vue_glob_patterns: Vec<String>,
    vue_glob_unverifiable: bool,
    has_dynamic_import: bool,
    has_vue_fallthrough_macro: bool,
    uses_css_module: bool,
}

struct SourceCollector<'s> {
    scoping: &'s Scoping,
    use_css_module_symbols: Vec<SymbolId>,
    vue_namespace_symbols: Vec<SymbolId>,
    this_alias_symbols: Vec<SymbolId>,
    analysis: SourceAnalysis,
}

impl SourceCollector<'_> {
    fn symbol(&self, identifier: &IdentifierReference<'_>) -> Option<SymbolId> {
        self.scoping
            .get_reference(identifier.reference_id.get()?)
            .symbol_id()
    }

    fn is_unbound(&self, identifier: &IdentifierReference<'_>) -> bool {
        self.symbol(identifier).is_none()
    }

    fn is_this_alias(&self, expression: &Expression<'_>) -> bool {
        match expression.get_inner_expression() {
            Expression::ThisExpression(_) => true,
            Expression::Identifier(identifier) => self
                .symbol(identifier)
                .is_some_and(|symbol| self.this_alias_symbols.contains(&symbol)),
            _ => false,
        }
    }

    fn collect_glob_argument(&mut self, argument: &Argument<'_>) -> bool {
        match argument {
            Argument::StringLiteral(literal) => {
                self.analysis.vue_glob_patterns.push(literal.value.to_string());
                true
            }
            Argument::TemplateLiteral(template) if template.expressions.is_empty() => {
                if let Some(value) = template
                    .quasis
                    .first()
                    .and_then(|quasi| quasi.value.cooked.as_ref())
                {
                    self.analysis.vue_glob_patterns.push(value.to_string());
                    true
                } else {
                    false
                }
            }
            Argument::ArrayExpression(array) => {
                let mut readable = true;
                for element in &array.elements {
                    match element {
                        ArrayExpressionElement::StringLiteral(literal) => self
                            .analysis
                            .vue_glob_patterns
                            .push(literal.value.to_string()),
                        ArrayExpressionElement::TemplateLiteral(template)
                            if template.expressions.is_empty() =>
                        {
                            if let Some(value) = template
                                .quasis
                                .first()
                                .and_then(|quasi| quasi.value.cooked.as_ref())
                            {
                                self.analysis.vue_glob_patterns.push(value.to_string());
                            } else {
                                readable = false;
                            }
                        }
                        _ => readable = false,
                    }
                }
                readable
            }
            _ => false,
        }
    }
}

impl<'a> Visit<'a> for SourceCollector<'_> {
    fn visit_import_declaration(&mut self, decl: &oxc_ast::ast::ImportDeclaration<'a>) {
        let type_only = decl.import_kind.is_type()
            || decl.specifiers.as_ref().is_some_and(|specifiers| {
                !specifiers.is_empty()
                    && specifiers.iter().all(|specifier| match specifier {
                        ImportDeclarationSpecifier::ImportSpecifier(named) => {
                            named.import_kind.is_type()
                        }
                        _ => false,
                    })
            });
        self.analysis.imports.push(SourceImport {
            specifier: decl.source.value.to_string(),
            type_only,
            // An import attribute clause such as `with { type: 'css' }`
            // constructs a stylesheet object without applying it, so the
            // record cannot prove unconditional loading.
            dynamic: decl.with_clause.is_some() || decl.phase.is_some(),
        });
        if !type_only && decl.phase.is_none() {
            self.analysis
                .static_imports
                .push(decl.source.value.to_string());
            for specifier in decl.specifiers.iter().flatten() {
                if let ImportDeclarationSpecifier::ImportDefaultSpecifier(specifier) = specifier {
                    self.analysis.default_imports.push(DefaultImportBinding {
                        source: decl.source.value.to_string(),
                        local: specifier.local.name.to_string(),
                    });
                }
            }
        }
        walk::walk_import_declaration(self, decl);
    }

    fn visit_export_from_declaration(&mut self, decl: &oxc_ast::ast::ExportFromDeclaration<'a>) {
        let type_only = decl.export_kind.is_type()
            || (!decl.specifiers.is_empty()
                && decl
                    .specifiers
                    .iter()
                    .all(|specifier| specifier.export_kind.is_type()));
        self.analysis.imports.push(SourceImport {
            specifier: decl.source.value.to_string(),
            type_only,
            dynamic: false,
        });
        walk::walk_export_from_declaration(self, decl);
    }

    fn visit_export_all_declaration(&mut self, decl: &oxc_ast::ast::ExportAllDeclaration<'a>) {
        self.analysis.imports.push(SourceImport {
            specifier: decl.source.value.to_string(),
            type_only: decl.export_kind.is_type(),
            dynamic: false,
        });
        walk::walk_export_all_declaration(self, decl);
    }

    fn visit_import_expression(&mut self, expression: &oxc_ast::ast::ImportExpression<'a>) {
        self.analysis.has_dynamic_import = true;
        if let Expression::StringLiteral(literal) = &expression.source {
            self.analysis.imports.push(SourceImport {
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
            self.analysis.imports.push(SourceImport {
                specifier: reference.expression.value.to_string(),
                type_only: decl.import_kind.is_type(),
                dynamic: false,
            });
        }
        walk::walk_ts_import_equals_declaration(self, decl);
    }

    fn visit_variable_declarator(&mut self, declarator: &VariableDeclarator<'a>) {
        if let Some(Expression::Identifier(namespace)) = &declarator.init
            && self
                .symbol(namespace)
                .is_some_and(|symbol| self.vue_namespace_symbols.contains(&symbol))
            && let BindingPattern::ObjectPattern(pattern) = &declarator.id
        {
            for property in &pattern.properties {
                if property.key.is_specific_static_name("useCssModule")
                    && let BindingPattern::BindingIdentifier(identifier) = &property.value
                    && let Some(symbol) = identifier.symbol_id.get()
                {
                    self.use_css_module_symbols.push(symbol);
                }
            }
        }
        if let Some(init) = &declarator.init
            && self.is_this_alias(init)
        {
            match &declarator.id {
                BindingPattern::BindingIdentifier(identifier) => {
                    if let Some(symbol) = identifier.symbol_id.get() {
                        self.this_alias_symbols.push(symbol);
                    }
                }
                BindingPattern::ObjectPattern(pattern)
                    if pattern
                        .properties
                        .iter()
                        .any(|property| property.key.is_specific_static_name("$style")) =>
                {
                    self.analysis.uses_css_module = true;
                }
                _ => {}
            }
        }
        walk::walk_variable_declarator(self, declarator);
    }

    fn visit_call_expression(&mut self, call: &oxc_ast::ast::CallExpression<'a>) {
        if let Expression::Identifier(callee) = &call.callee {
            if callee.name == "require"
                && let Some(Argument::StringLiteral(literal)) = call.arguments.first()
            {
                self.analysis.imports.push(SourceImport {
                    specifier: literal.value.to_string(),
                    type_only: false,
                    dynamic: true,
                });
            }
            if matches!(callee.name.as_str(), "defineOptions" | "defineProps")
                && self.is_unbound(callee)
            {
                self.analysis.has_vue_fallthrough_macro = true;
            }
            if self
                .symbol(callee)
                .is_some_and(|symbol| self.use_css_module_symbols.contains(&symbol))
            {
                self.analysis.uses_css_module = true;
            }
        } else if let Expression::StaticMemberExpression(member) = &call.callee {
            if member.property.name == "glob"
                && matches!(&member.object, Expression::ImportMeta(_))
            {
                if call
                    .arguments
                    .first()
                    .is_none_or(|argument| !self.collect_glob_argument(argument))
                {
                    self.analysis.vue_glob_unverifiable = true;
                }
            } else if member.property.name == "useCssModule"
                && let Expression::Identifier(object) = &member.object
                && self
                    .symbol(object)
                    .is_some_and(|symbol| self.vue_namespace_symbols.contains(&symbol))
            {
                self.analysis.uses_css_module = true;
            }
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        if identifier.name == "$style" && self.is_unbound(identifier) {
            self.analysis.uses_css_module = true;
        }
        if self
            .symbol(identifier)
            .is_some_and(|symbol| self.use_css_module_symbols.contains(&symbol))
        {
            self.analysis.uses_css_module = true;
        }
        walk::walk_identifier_reference(self, identifier);
    }

    fn visit_static_member_expression(&mut self, member: &oxc_ast::ast::StaticMemberExpression<'a>) {
        if member.property.name == "$style" && self.is_this_alias(&member.object) {
            self.analysis.uses_css_module = true;
        }
        if member.property.name == "useCssModule"
            && let Expression::Identifier(object) = &member.object
            && self
                .symbol(object)
                .is_some_and(|symbol| self.vue_namespace_symbols.contains(&symbol))
        {
            self.analysis.uses_css_module = true;
        }
        walk::walk_static_member_expression(self, member);
    }

    fn visit_computed_member_expression(
        &mut self,
        member: &oxc_ast::ast::ComputedMemberExpression<'a>,
    ) {
        if self.is_this_alias(&member.object)
            && matches!(&member.expression, Expression::StringLiteral(literal) if literal.value == "$style")
        {
            self.analysis.uses_css_module = true;
        }
        if matches!(&member.expression, Expression::StringLiteral(literal) if literal.value == "useCssModule")
            && let Expression::Identifier(object) = &member.object
            && self
                .symbol(object)
                .is_some_and(|symbol| self.vue_namespace_symbols.contains(&symbol))
        {
            self.analysis.uses_css_module = true;
        }
        walk::walk_computed_member_expression(self, member);
    }
}

/// Parse and semantically analyze one JavaScript or TypeScript source once,
/// returning every fact the TypeScript orchestration layer needs.
pub fn source_analysis_json(path: &str, source: &str) -> Result<String, String> {
    let source_type = source_type_for_path(path)?;
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return Err(format!("Failed to parse {path}"));
    }
    let semantic = SemanticBuilder::new_compiler().build(&parsed.program);
    if !semantic.diagnostics.is_empty() {
        return Err(format!("Failed to analyze {path}"));
    }
    let mut use_css_module_symbols = Vec::new();
    let mut vue_namespace_symbols = Vec::new();
    for statement in &parsed.program.body {
        let oxc_ast::ast::Statement::ImportDeclaration(declaration) = statement else {
            continue;
        };
        if declaration.source.value != "vue" || declaration.import_kind.is_type() {
            continue;
        }
        for specifier in declaration.specifiers.iter().flatten() {
            match specifier {
                ImportDeclarationSpecifier::ImportSpecifier(specifier)
                    if specifier.imported.name() == "useCssModule"
                        && !specifier.import_kind.is_type() =>
                {
                    if let Some(symbol) = specifier.local.symbol_id.get() {
                        use_css_module_symbols.push(symbol);
                    }
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(specifier) => {
                    if let Some(symbol) = specifier.local.symbol_id.get() {
                        vue_namespace_symbols.push(symbol);
                    }
                }
                _ => {}
            }
        }
    }
    let mut collector = SourceCollector {
        scoping: semantic.semantic.scoping(),
        use_css_module_symbols,
        vue_namespace_symbols,
        this_alias_symbols: Vec::new(),
        analysis: SourceAnalysis {
            imports: Vec::new(),
            static_imports: Vec::new(),
            default_imports: Vec::new(),
            vue_glob_patterns: Vec::new(),
            vue_glob_unverifiable: false,
            has_dynamic_import: false,
            has_vue_fallthrough_macro: false,
            uses_css_module: false,
        },
    };
    collector.visit_program(&parsed.program);
    serde_json::to_string(&collector.analysis).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    fn imports(source: &str, path: &str) -> Vec<(String, bool, bool)> {
        let parsed: serde_json::Value =
            serde_json::from_str(&super::source_analysis_json(path, source).unwrap()).unwrap();
        parsed["imports"]
            .as_array()
            .unwrap()
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
    fn analyzes_template_expressions_structurally() {
        for (source, static_string, member, uses_css_module) in [
            ("'card'", Some("card"), None, false),
            ("`card`", Some("card"), None, false),
            ("$style.card", None, Some("card"), true),
            ("active ? $style.card : ''", None, None, true),
            ("'$style.card useCssModule()'", Some("$style.card useCssModule()"), None, false),
        ] {
            let parsed: serde_json::Value = serde_json::from_str(
                &super::expression_analysis_json("Component.js", source).unwrap(),
            )
            .unwrap();
            assert_eq!(parsed["staticString"], serde_json::json!(static_string));
            assert_eq!(parsed["vueModuleMember"], serde_json::json!(member));
            assert_eq!(parsed["usesCssModule"], uses_css_module);
        }
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
        assert!(super::source_analysis_json("/p/x.ts", "import from from;").is_err());
    }

    #[test]
    fn analyzes_vue_syntax_semantically() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/registry.ts",
                "import Child from './Child.vue';\n\
                 import { useCssModule as css } from 'vue';\n\
                 import * as Vue from 'vue';\n\
                 const patterns = import.meta.glob(['./*.vue', `./nested/*`]);\n\
                 const lazy = import('./Lazy.vue');\n\
                 defineProps<{ class?: string }>();\n\
                 const getStyles = css;\n\
                 getStyles();\n\
                 Vue.useCssModule();\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed["defaultImports"],
            serde_json::json!([{ "source": "./Child.vue", "local": "Child" }])
        );
        assert_eq!(
            parsed["vueGlobPatterns"],
            serde_json::json!(["./*.vue", "./nested/*"])
        );
        assert_eq!(parsed["hasDynamicImport"], true);
        assert_eq!(parsed["vueGlobUnverifiable"], false);
        assert_eq!(parsed["hasVueFallthroughMacro"], true);
        assert_eq!(parsed["usesCssModule"], true);
    }

    #[test]
    fn ignores_unused_use_css_module_imports() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.vue.js",
                "import { useCssModule } from 'vue';",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["usesCssModule"], false);
    }

    #[test]
    fn recognizes_destructured_options_api_css_module_references() {
        for source in [
            "export default { mounted() { const { $style: styles } = this; void styles.card; } };",
            "export default { mounted() { const { $style } = this; void $style.card; } };",
            "export default { mounted() { const vm = this; void vm.$style.card; } };",
            "export default { mounted() { const vm = this; const self = vm; void self['$style'].card; } };",
            "const vm = this as ComponentPublicInstance; void vm.$style.card;",
            "const vm = this satisfies ComponentPublicInstance; void vm.$style.card;",
            "const vm = this!; void vm.$style.card;",
        ] {
            let parsed: serde_json::Value = serde_json::from_str(
                &super::source_analysis_json("/p/Card.vue.ts", source).unwrap(),
            )
            .unwrap();
            assert_eq!(parsed["usesCssModule"], true);
        }

        let unrelated: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.vue.js",
                "const value = {}; const { $style } = value; void $style.card;",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(unrelated["usesCssModule"], false);
    }

    #[test]
    fn recognizes_computed_vue_namespace_css_module_references() {
        for source in [
            "import * as Vue from 'vue'; Vue['useCssModule']();",
            "import * as Vue from 'vue'; const { useCssModule } = Vue; useCssModule();",
            "import * as Vue from 'vue'; const { useCssModule: css } = Vue; css();",
        ] {
            let parsed: serde_json::Value = serde_json::from_str(
                &super::source_analysis_json("/p/Card.vue.ts", source).unwrap(),
            )
            .unwrap();
            assert_eq!(parsed["usesCssModule"], true);
        }

        let shadowed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.vue.ts",
                "const Vue = {}; Vue['useCssModule']();",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(shadowed["usesCssModule"], false);
    }

    #[test]
    fn marks_unreadable_vue_globs_unverifiable() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/registry.ts",
                "const pattern = './*.vue'; import.meta.glob(pattern);",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["vueGlobPatterns"], serde_json::json!([]));
        assert_eq!(parsed["vueGlobUnverifiable"], true);
    }

    #[test]
    fn ignores_vue_names_in_comments_strings_and_shadowed_bindings() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/component.ts",
                "// import('./Comment.vue'); defineProps(); useCssModule(); $style\n\
                 const text = \"defineOptions inheritAttrs useCssModule $style\";\n\
                 const defineProps = () => {};\n\
                 const useCssModule = () => {};\n\
                 const unrelated = { $style: true };\n\
                 defineProps();\n\
                 useCssModule();\n\
                 unrelated.$style;\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["hasDynamicImport"], false);
        assert_eq!(parsed["hasVueFallthroughMacro"], false);
        assert_eq!(parsed["usesCssModule"], false);
    }
}

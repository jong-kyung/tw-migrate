//! Parsed module imports for the TypeScript proof layer. Shared-entry
//! loading proofs need real import statements rather than substring
//! matches: comments, dead strings, and type-only clauses must never count
//! as runtime loading, so the sources are parsed with oxc and only actual
//! module records are reported.

use std::collections::BTreeMap;

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

use crate::{at_rules::parse_css, js_rewrite::source_type_for_path};
use oxc_css_parser::ast::{
    AtRulePrelude, ColorProfilePrelude, CombinatorKind, ComplexSelectorChild, ComponentValue,
    CompoundSelector, FontFamilyName, Function, FunctionName, ImportPrelude, ImportPreludeHref,
    InterpolableIdent, InterpolableStr, MediaQuery, PseudoClassSelectorArgKind, SimpleSelector,
    Statement, UrlValue,
};
use oxc_css_parser::{Syntax, token};

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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StylesheetImport {
    href: String,
    media: String,
    start: usize,
    end: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StylesheetAnalysis {
    references: Vec<String>,
    imports: Vec<StylesheetImport>,
    unverifiable: bool,
    scope_escapes: Vec<String>,
    scope_shadow_css: Vec<String>,
    scope_escapes_unverifiable: bool,
    selectors_unverifiable: bool,
    theme_tokens: BTreeMap<String, String>,
    global_at_rule_identities: Vec<String>,
    global_at_rules_unverifiable: bool,
}

fn stylesheet_syntax(path: &str) -> Result<Syntax, String> {
    match std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("css") => Ok(Syntax::Css),
        Some("scss") => Ok(Syntax::Scss),
        Some("sass") => Ok(Syntax::Sass),
        Some("less") => Ok(Syntax::Less),
        _ => Err(format!("Unsupported stylesheet: {path}")),
    }
}

fn import_href(href: &ImportPreludeHref<'_>) -> Option<String> {
    match href {
        ImportPreludeHref::Str(value) => literal_str(value),
        ImportPreludeHref::Url(url) => match &url.value {
            Some(UrlValue::Raw(raw)) => Some(raw.value.to_string()),
            Some(UrlValue::Str(value)) => literal_str(value),
            _ => None,
        },
        ImportPreludeHref::Function(_) => None,
    }
}

fn interpolable_str_end(value: &InterpolableStr<'_>) -> usize {
    match value {
        InterpolableStr::Literal(value) => value.span.end,
        InterpolableStr::SassInterpolated(value) => value.span.end,
        InterpolableStr::LessInterpolated(value) => value.span.end,
    }
}

fn import_href_end(href: &ImportPreludeHref<'_>) -> usize {
    match href {
        ImportPreludeHref::Str(value) => interpolable_str_end(value),
        ImportPreludeHref::Url(value) => value.span.end,
        ImportPreludeHref::Function(value) => value.span.end,
    }
}

fn import_media(source: &str, prelude: &ImportPrelude<'_>) -> String {
    if prelude.layer.is_some() || prelude.supports.is_some() || prelude.modifiers.is_some() {
        return source[import_href_end(&prelude.href)..prelude.span.end]
            .trim()
            .to_string();
    }
    prelude
        .media
        .as_ref()
        .map(|media| source[media.span.start..media.span.end].trim().to_string())
        .unwrap_or_default()
}

fn scope_escape_name(name: &InterpolableIdent<'_>) -> bool {
    matches!(
        name,
        InterpolableIdent::Literal(name)
            if matches!(name.name, "deep" | "global" | "slotted" | "v-deep" | "v-global" | "v-slotted")
    )
}

fn selector_is_unverifiable(selector: &oxc_css_parser::ast::ComplexSelector<'_>) -> bool {
    selector.children.iter().any(|child| {
        let ComplexSelectorChild::CompoundSelector(compound) = child else {
            return false;
        };
        compound.children.iter().any(|simple| match simple {
            SimpleSelector::Class(class) => !matches!(class.name, InterpolableIdent::Literal(_)),
            SimpleSelector::Id(id) => !matches!(id.name, InterpolableIdent::Literal(_)),
            SimpleSelector::Type(oxc_css_parser::ast::TypeSelector::TagName(tag)) => {
                !matches!(tag.name.name, InterpolableIdent::Literal(_))
            }
            SimpleSelector::Nesting(nesting) => nesting.suffix.is_some(),
            _ => false,
        })
    })
}

fn collect_compound_scope_escapes(
    compound: &CompoundSelector<'_>,
    source: &str,
    analysis: &mut StylesheetAnalysis,
) -> bool {
    let mut found = false;
    for simple in &compound.children {
        let escape_bounds = match simple {
            SimpleSelector::PseudoClass(pseudo) if scope_escape_name(&pseudo.name) => Some(
                pseudo
                    .arg
                    .as_ref()
                    .map(|arg| (arg.l_paren.end, arg.r_paren.start)),
            ),
            SimpleSelector::PseudoElement(pseudo) if scope_escape_name(&pseudo.name) => Some(
                pseudo
                    .arg
                    .as_ref()
                    .map(|arg| (arg.l_paren.end, arg.r_paren.start)),
            ),
            _ => None,
        };
        if let Some(bounds) = escape_bounds {
            found = true;
            if let Some((start, end)) = bounds {
                analysis
                    .scope_escapes
                    .push(format!("{} {{}}", &source[start..end]));
            } else {
                analysis.scope_escapes_unverifiable = true;
            }
            continue;
        }

        let SimpleSelector::PseudoClass(pseudo) = simple else {
            continue;
        };
        let Some(arg) = &pseudo.arg else { continue };
        match &arg.kind {
            PseudoClassSelectorArgKind::CompoundSelectorList(list) => {
                for selector in &list.selectors {
                    found |= collect_compound_scope_escapes(selector, source, analysis);
                }
            }
            PseudoClassSelectorArgKind::RelativeSelectorList(list) => {
                for selector in &list.selectors {
                    found |= collect_scope_escapes(&selector.complex_selector, source, analysis);
                }
            }
            PseudoClassSelectorArgKind::SelectorList(list) => {
                for selector in &list.selectors {
                    found |= collect_scope_escapes(selector, source, analysis);
                }
            }
            PseudoClassSelectorArgKind::Nth(nth) => {
                if let Some(list) = nth.matcher.as_ref().and_then(|matcher| matcher.selector.as_ref())
                {
                    for selector in &list.selectors {
                        found |= collect_scope_escapes(selector, source, analysis);
                    }
                }
            }
            _ => {}
        }
    }
    found
}

fn collect_scope_escapes(
    selector: &oxc_css_parser::ast::ComplexSelector<'_>,
    source: &str,
    analysis: &mut StylesheetAnalysis,
) -> bool {
    let mut found = false;
    for child in &selector.children {
        match child {
            ComplexSelectorChild::Combinator(combinator)
                if matches!(
                    combinator.kind,
                    CombinatorKind::Deep
                        | CombinatorKind::ShadowChild
                        | CombinatorKind::ShadowDescendant
                ) =>
            {
                found = true;
                analysis.scope_escapes_unverifiable = true;
            }
            ComplexSelectorChild::Combinator(_) => {}
            ComplexSelectorChild::CompoundSelector(compound) => {
                found |= collect_compound_scope_escapes(compound, source, analysis);
            }
        }
    }
    found
}

fn declaration_value(source: &str, declaration: &oxc_css_parser::ast::Declaration<'_>) -> String {
    let Some(first) = declaration.value.first() else {
        return String::new();
    };
    let last = declaration.value.last().expect("checked");
    source[first.span().start..last.span().end].trim().to_string()
}

fn literal_ident<'a>(value: &'a InterpolableIdent<'a>) -> Option<&'a str> {
    let InterpolableIdent::Literal(value) = value else {
        return None;
    };
    Some(value.name)
}

fn registration_prelude(prelude: Option<&AtRulePrelude<'_>>) -> Option<String> {
    match prelude? {
        AtRulePrelude::ColorProfile(ColorProfilePrelude::DashedIdent(value))
        | AtRulePrelude::CounterStyle(value)
        | AtRulePrelude::FontPaletteValues(value)
        | AtRulePrelude::PositionTry(value)
        | AtRulePrelude::Property(value) => literal_ident(value).map(str::to_string),
        AtRulePrelude::ColorProfile(ColorProfilePrelude::DeviceCmyk(value)) => {
            Some(value.name.to_string())
        }
        AtRulePrelude::FontFeatureValues(FontFamilyName::Str(value)) => literal_str(value),
        AtRulePrelude::FontFeatureValues(FontFamilyName::Unquoted(value)) => value
            .idents
            .iter()
            .map(literal_ident)
            .collect::<Option<Vec<_>>>()
            .map(|names| names.join(" ")),
        _ => None,
    }
}

fn collect_at_rule_metadata(
    statements: &[Statement<'_>],
    source: &str,
    syntax: Syntax,
    analysis: &mut StylesheetAnalysis,
) {
    for statement in statements {
        let Statement::AtRule(at_rule) = statement else {
            continue;
        };
        let Some(block) = &at_rule.block else {
            continue;
        };
        if at_rule.name.name == "theme" {
            for statement in &block.statements {
                let Statement::Declaration(declaration) = statement else {
                    continue;
                };
                let InterpolableIdent::Literal(name) = &declaration.name else {
                    continue;
                };
                if let Some(token) = name.name.strip_prefix("--") {
                    analysis
                        .theme_tokens
                        .insert(token.to_string(), declaration_value(source, declaration));
                }
            }
        }
        match at_rule.name.name {
            "font-face" => {
                let family = block.statements.iter().find_map(|statement| {
                    let Statement::Declaration(declaration) = statement else {
                        return None;
                    };
                    let InterpolableIdent::Literal(name) = &declaration.name else {
                        return None;
                    };
                    (name.name == "font-family").then(|| {
                        declaration_value(source, declaration)
                            .trim_matches(['"', '\''])
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                            .to_lowercase()
                    })
                });
                analysis
                    .global_at_rule_identities
                    .push(format!("font-face {}", family.unwrap_or_default()));
            }
            "page" => analysis.global_at_rule_identities.push("page".to_string()),
            name
                @ ("color-profile"
                | "counter-style"
                | "font-feature-values"
                | "font-palette-values"
                | "position-try"
                | "property"
                | "view-transition") =>
            {
                if let Some(prelude) = registration_prelude(at_rule.prelude.as_ref()) {
                    analysis
                        .global_at_rule_identities
                        .push(format!("{name} {prelude}"));
                } else if syntax == Syntax::Css {
                    let prelude = source[at_rule.name.span.end..block.span.start].trim();
                    analysis
                        .global_at_rule_identities
                        .push(format!("{name} {prelude}"));
                } else {
                    analysis.global_at_rules_unverifiable = true;
                }
            }
            _ => {}
        }
        collect_at_rule_metadata(&block.statements, source, syntax, analysis);
    }
}

fn collect_stylesheet_statements(
    statements: &[Statement<'_>],
    source: &str,
    analysis: &mut StylesheetAnalysis,
) -> bool {
    let mut has_scope_escape = false;
    for statement in statements {
        match statement {
            Statement::AtRule(at_rule) => {
                match &at_rule.prelude {
                    Some(AtRulePrelude::Import(prelude)) => match import_href(&prelude.href) {
                        Some(reference) => analysis.references.push(reference),
                        None => analysis.unverifiable = true,
                    },
                    Some(AtRulePrelude::LessImport(prelude)) => match import_href(&prelude.href) {
                        Some(reference) => analysis.references.push(reference),
                        None => analysis.unverifiable = true,
                    },
                    Some(AtRulePrelude::SassImport(prelude)) => {
                        analysis
                            .references
                            .extend(prelude.paths.iter().map(|path| path.value.to_string()));
                    }
                    Some(AtRulePrelude::SassUse(prelude)) => match literal_str(&prelude.path) {
                        Some(reference) => analysis.references.push(reference),
                        None => analysis.unverifiable = true,
                    },
                    Some(AtRulePrelude::SassForward(prelude)) => match literal_str(&prelude.path) {
                        Some(reference) => analysis.references.push(reference),
                        None => analysis.unverifiable = true,
                    },
                    _ => {}
                }
                if let Some(block) = &at_rule.block {
                    has_scope_escape |=
                        collect_stylesheet_statements(&block.statements, source, analysis);
                }
            }
            Statement::QualifiedRule(rule) => {
                let mut direct = false;
                for selector in &rule.selector.selectors {
                    direct |= collect_scope_escapes(selector, source, analysis);
                    if selector_is_unverifiable(selector) {
                        analysis.selectors_unverifiable = true;
                    }
                }
                let nested = collect_stylesheet_statements(&rule.block.statements, source, analysis);
                if nested {
                    // Reconstructing mixed declaration and nested-rule scope is
                    // outside this collector; retain conservatively.
                    analysis.scope_escapes_unverifiable = true;
                }
                if !direct && !nested {
                    analysis.scope_shadow_css.push(format!(
                        "{} {{}}",
                        &source[rule.selector.span.start..rule.selector.span.end]
                    ));
                }
                has_scope_escape |= direct || nested;
            }
            Statement::Declaration(declaration) => {
                let InterpolableIdent::Literal(name) = &declaration.name else {
                    continue;
                };
                if name.name != "composes" {
                    continue;
                }
                let mut from = false;
                let mut reference_read = false;
                for value in &declaration.value {
                    if from {
                        match value {
                            ComponentValue::InterpolableStr(value) => match literal_str(value) {
                                Some(reference) => {
                                    analysis.references.push(reference);
                                    reference_read = true;
                                }
                                None => analysis.unverifiable = true,
                            },
                            ComponentValue::InterpolableIdent(InterpolableIdent::Literal(ident))
                                if ident.name == "global" =>
                            {
                                reference_read = true;
                            }
                            _ => analysis.unverifiable = true,
                        }
                        break;
                    }
                    if matches!(value, ComponentValue::InterpolableIdent(InterpolableIdent::Literal(ident)) if ident.name == "from")
                    {
                        from = true;
                    }
                }
                if from && !reference_read {
                    analysis.unverifiable = true;
                }
            }
            _ => {}
        }
    }
    has_scope_escape
}

fn collect_loading_imports(
    statements: &[Statement<'_>],
    source: &str,
    analysis: &mut StylesheetAnalysis,
) {
    let mut imports_allowed = true;
    for statement in statements {
        let Statement::AtRule(at_rule) = statement else {
            imports_allowed = false;
            continue;
        };
        if let Some(AtRulePrelude::Import(prelude)) = &at_rule.prelude {
            if imports_allowed {
                if let Some(href) = import_href(&prelude.href) {
                    let end = at_rule.span.end
                        + usize::from(source.as_bytes().get(at_rule.span.end) == Some(&b';'));
                    analysis.imports.push(StylesheetImport {
                        href,
                        media: import_media(source, prelude),
                        start: at_rule.span.start,
                        end,
                    });
                } else {
                    analysis.unverifiable = true;
                }
            }
        } else if at_rule.block.is_some() {
            imports_allowed = false;
        }
    }
}

pub fn stylesheet_analysis_json(path: &str, source: &str) -> Result<String, String> {
    let syntax = stylesheet_syntax(path)?;
    let allocator = oxc_css_parser::Allocator::default();
    let parsed = parse_css(&allocator, source, syntax)
        .map_err(|error| format!("Failed to parse {path}: {error}"))?;
    let mut analysis = StylesheetAnalysis {
        references: Vec::new(),
        imports: Vec::new(),
        unverifiable: false,
        scope_escapes: Vec::new(),
        scope_shadow_css: Vec::new(),
        scope_escapes_unverifiable: false,
        selectors_unverifiable: false,
        theme_tokens: BTreeMap::new(),
        global_at_rule_identities: Vec::new(),
        global_at_rules_unverifiable: false,
    };
    collect_stylesheet_statements(&parsed.statements, source, &mut analysis);
    collect_at_rule_metadata(&parsed.statements, source, syntax, &mut analysis);
    if syntax == Syntax::Css {
        collect_loading_imports(&parsed.statements, source, &mut analysis);
    }
    analysis.references.sort();
    analysis.references.dedup();
    serde_json::to_string(&analysis).map_err(|error| error.to_string())
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
    let specifier = import_href(&prelude.href);
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

    #[test]
    fn collects_stylesheet_dependencies_for_every_supported_syntax() {
        for (path, source, expected) in [
            (
                "/p/main.css",
                "@import \"./base.css\" screen;\n.x { composes: y from \"./x.module.css\"; }\n.global { composes: reset from global; }\n",
                vec!["./base.css", "./x.module.css"],
            ),
            (
                "/p/main.scss",
                "@use \"./tokens\";\n@forward \"./shared\";\n@import \"./legacy\";\n.x { composes: y from \"./x.module.css\"; }\n",
                vec!["./legacy", "./shared", "./tokens", "./x.module.css"],
            ),
            (
                "/p/main.sass",
                "@use \"./tokens\"\n@forward \"./shared\"\n@import \"./legacy\"\n.x\n  composes: y from \"./x.module.css\"\n",
                vec!["./legacy", "./shared", "./tokens", "./x.module.css"],
            ),
            (
                "/p/main.less",
                "@import (reference) \"./tokens.less\";\n.x { composes: y from \"./x.module.css\"; }\n",
                vec!["./tokens.less", "./x.module.css"],
            ),
        ] {
            let parsed: serde_json::Value = serde_json::from_str(
                &super::stylesheet_analysis_json(path, source).unwrap(),
            )
            .unwrap();
            assert_eq!(parsed["references"], serde_json::json!(expected), "{path}");
            assert_eq!(parsed["unverifiable"], false, "{path}");
        }
    }

    #[test]
    fn collects_loading_import_media_and_byte_spans() {
        let source = "/* 😀 */\n@import url(\"./print.css\") print;\n.rule {}\n@import \"./late.css\";\n";
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json("/p/main.css", source).unwrap(),
        )
        .unwrap();
        let start = source.find("@import").unwrap();
        let end = source[start..].find(';').unwrap() + start + 1;
        assert_eq!(
            parsed["imports"],
            serde_json::json!([{
                "href": "./print.css",
                "media": "print",
                "start": start,
                "end": end
            }])
        );
    }

    #[test]
    fn collects_theme_tokens_and_global_at_rule_identities() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/theme.css",
                "@theme { --spacing-card: calc(1rem + 2px); }\n\
                 @font-face { font-family: \"My  Font\"; src: url(font.woff2); }\n\
                 @media print { @font-face { font-family: Print; src: url(print.woff2); } }\n\
                 @property --angle { syntax: \"<angle>\"; }\n\
                 @property /* docs */ --angle { syntax: \"<angle>\"; }\n\
                 @page :left { margin: 1cm; }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed["themeTokens"],
            serde_json::json!({ "spacing-card": "calc(1rem + 2px)" })
        );
        assert_eq!(
            parsed["globalAtRuleIdentities"],
            serde_json::json!([
                "font-face my font",
                "font-face print",
                "property --angle",
                "property --angle",
                "page"
            ])
        );
    }

    #[test]
    fn collects_vue_scope_escapes_from_selector_nodes() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/scoped.css",
                ".a :deep(.inside:is(.x, .y)) {}\n.b::v-global(.free) {}\n.c :global .open {}\n.d /deep/ .legacy {}\n.e :deep(.host):is(:deep(.card), :not(:global(.note))) {}\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed["scopeEscapes"],
            serde_json::json!([
                ".inside:is(.x, .y) {}",
                ".free {}",
                ".host {}",
                ".card {}",
                ".note {}"
            ])
        );
        assert_eq!(parsed["scopeEscapesUnverifiable"], true);
        assert_eq!(parsed["selectorsUnverifiable"], false);
    }

    #[test]
    fn marks_interpolated_registration_identities_unverifiable() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/main.scss",
                "$name: brand;\n@property --#{$name} { syntax: \"<color>\"; }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["globalAtRuleIdentities"], serde_json::json!([]));
        assert_eq!(parsed["globalAtRulesUnverifiable"], true);
    }

    #[test]
    fn marks_interpolated_selectors_unverifiable() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/main.scss",
                "$kind: card;\n.#{$kind} {}\n.block { &-active {} }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["selectorsUnverifiable"], true);
    }

    #[test]
    fn marks_interpolated_stylesheet_dependencies_unverifiable() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/main.scss",
                "$name: \"theme\";\n@use \"./#{$name}\";\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["references"], serde_json::json!([]));
        assert_eq!(parsed["unverifiable"], true);
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

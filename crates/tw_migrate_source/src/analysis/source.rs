//! Parsed JavaScript and TypeScript facts for the TypeScript proof layer.
//! Shared-entry loading proofs and Vue analysis need real syntax rather
//! than substring matches: comments, dead strings, and type-only clauses
//! must never count as runtime loading, so the sources are parsed with oxc
//! and analyzed semantically before any fact is reported.

use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, AssignmentTarget, AssignmentTargetProperty, BindingPattern,
    Expression, IdentifierReference, ImportDeclarationSpecifier, VariableDeclarator,
};
use oxc_syntax::operator::BinaryOperator;
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_semantic::{Semantic, SemanticBuilder};
use oxc_syntax::symbol::SymbolId;
use serde::Serialize;
use tw_migrate_error::{MigrationError, MigrationResult};

use crate::source_type_for_path;

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
    references_use_css_module: bool,
    /// Identifier reference names, for matching script-provided helper
    /// aliases the expression may call.
    references: Vec<String>,
    /// Nonempty leading quasis of template literals with expressions, for
    /// dynamic class spelling reservations.
    template_prefixes: Vec<String>,
}

/// The class token each interpolation continues: the trailing
/// whitespace-delimited token of every quasi that precedes an expression,
/// because `` `base mr-${side}` `` constrains the dynamic class to the
/// `mr-` prefix while `base` stays a static token. A quasi ending in
/// whitespace leaves its expression unconstrained and reserves nothing.
fn collect_template_prefixes(
    template: &oxc_ast::ast::TemplateLiteral<'_>,
    prefixes: &mut Vec<String>,
) {
    if template.expressions.is_empty() {
        return;
    }
    for quasi in template.quasis.iter().take(template.expressions.len()) {
        let Some(cooked) = quasi.value.cooked.as_ref() else {
            continue;
        };
        record_trailing_prefix(cooked, prefixes);
    }
}

/// The trailing whitespace-delimited token of static text preceding a
/// dynamic part constrains the produced class; whitespace-terminated text
/// constrains nothing.
fn record_trailing_prefix(text: &str, prefixes: &mut Vec<String>) {
    let token = text
        .rsplit(|character: char| character.is_whitespace())
        .next()
        .unwrap_or("");
    if !token.is_empty() && !prefixes.iter().any(|existing| existing == token) {
        prefixes.push(token.to_string());
    }
}

/// A string concatenation such as `'mr-' + side` constrains the produced
/// class the same way a template quasi does: the trailing
/// whitespace-delimited token of the static text before a dynamic part is
/// a prefix the runtime value could complete.
fn collect_concat_prefixes(
    expression: &oxc_ast::ast::BinaryExpression<'_>,
    prefixes: &mut Vec<String>,
) {
    if expression.operator != BinaryOperator::Addition {
        return;
    }
    let Some(text) = trailing_static_string(&expression.left) else {
        return;
    };
    record_trailing_prefix(text, prefixes);
}

fn trailing_static_string<'e>(expression: &'e Expression<'_>) -> Option<&'e str> {
    match expression {
        Expression::StringLiteral(literal) => Some(literal.value.as_str()),
        Expression::BinaryExpression(binary) if binary.operator == BinaryOperator::Addition => {
            trailing_static_string(&binary.right)
        }
        _ => None,
    }
}

/// Template expressions carry no bindings of their own, so any `$style` or
/// `useCssModule` reference is a potential CSS Module access; the caller
/// decides whether a script-provided binding shadows the Vue API.
struct ExpressionCollector {
    uses_css_module: bool,
    references_use_css_module: bool,
    references: Vec<String>,
    template_prefixes: Vec<String>,
}

impl ExpressionCollector {
    fn record(&mut self, name: &str) {
        if name == "$style" {
            self.uses_css_module = true;
        }
        if name == "useCssModule" {
            self.references_use_css_module = true;
        }
    }
}

impl<'a> Visit<'a> for ExpressionCollector {
    fn visit_template_literal(&mut self, template: &oxc_ast::ast::TemplateLiteral<'a>) {
        collect_template_prefixes(template, &mut self.template_prefixes);
        walk::walk_template_literal(self, template);
    }

    fn visit_binary_expression(&mut self, expression: &oxc_ast::ast::BinaryExpression<'a>) {
        collect_concat_prefixes(expression, &mut self.template_prefixes);
        walk::walk_binary_expression(self, expression);
    }

    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        self.record(&identifier.name);
        if !self
            .references
            .iter()
            .any(|name| name == identifier.name.as_str())
        {
            self.references.push(identifier.name.to_string());
        }
        walk::walk_identifier_reference(self, identifier);
    }

    fn visit_static_member_expression(
        &mut self,
        member: &oxc_ast::ast::StaticMemberExpression<'a>,
    ) {
        self.record(&member.property.name);
        walk::walk_static_member_expression(self, member);
    }

    fn visit_computed_member_expression(
        &mut self,
        member: &oxc_ast::ast::ComputedMemberExpression<'a>,
    ) {
        if let Expression::StringLiteral(literal) = &member.expression {
            self.record(&literal.value);
        }
        walk::walk_computed_member_expression(self, member);
    }
}

pub fn expression_analysis_json(path: &str, source: &str) -> MigrationResult<String> {
    let allocator = Allocator::default();
    let source_type = source_type_for_path(path)
        .map_err(|message| MigrationError::UnsupportedSource { message })?;
    let expression = Parser::new(&allocator, source, source_type)
        .parse_expression()
        .map_err(|diagnostics| MigrationError::SourceParse {
            message: format!("Failed to parse {path}: {diagnostics:?}"),
        })?;
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
        Expression::StaticMemberExpression(member) if matches!(&member.object, Expression::Identifier(object) if object.name == "$style") => {
            Some(member.property.name.to_string())
        }
        _ => None,
    };
    let mut collector = ExpressionCollector {
        uses_css_module: false,
        references_use_css_module: false,
        references: Vec::new(),
        template_prefixes: Vec::new(),
    };
    collector.visit_expression(&expression);
    // The proven `$style.member` form is rewritten rather than retained, so
    // it does not additionally count as an unresolvable API reference.
    serde_json::to_string(&ExpressionAnalysis {
        static_string,
        vue_module_member,
        uses_css_module: collector.uses_css_module,
        references_use_css_module: collector.references_use_css_module,
        references: collector.references,
        template_prefixes: collector.template_prefixes,
    })
    .map_err(|error| MigrationError::Serialization {
        message: error.to_string(),
    })
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
    /// An unbound `useCssModule` reference: possibly the Vue API through
    /// tooling-provided auto-imports, so CSS Module closure stays open.
    has_unbound_use_css_module: bool,
    /// A root-scope binding named `useCssModule` that is not the Vue
    /// import; template references resolve to it instead of the Vue API.
    defines_root_use_css_module: bool,
    /// Nonempty leading quasis of template literals with expressions, so
    /// spelling reservations can match statically constrained dynamic
    /// class construction such as `mr-${value}`.
    template_prefixes: Vec<String>,
    /// Local names bound to the Vue `useCssModule` helper, including
    /// import aliases and namespace destructures, so template analysis can
    /// recognize calls through them.
    use_css_module_locals: Vec<String>,
    /// Unbound identifier reference names, for handler sources whose
    /// bindings live in the surrounding script.
    unbound_references: Vec<String>,
    /// Static hrefs of JSX `<link rel="stylesheet">` elements, so spelling
    /// reservations can resolve the loaded sheet like an HTML link.
    stylesheet_links: Vec<String>,
    /// True when a JSX link's rel or href cannot be read statically, so
    /// the loaded target is unknowable.
    stylesheet_links_unverifiable: bool,
    /// True when the source renders a `<style>` element (styled-jsx and
    /// similar inline CSS), whose selectors this analysis never reads.
    renders_style_element: bool,
    /// True when the source injects runtime markup through
    /// dangerouslySetInnerHTML, whose classes can never join the scan
    /// corpus.
    injects_markup: bool,
    /// Custom-property name literals (`--*` strings) appearing anywhere in
    /// the source, for font token name reservations.
    custom_properties: Vec<String>,
    /// True when a style property API receives a property name the parser
    /// cannot read, so property-keyed reservations cannot be bounded.
    custom_properties_unbounded: bool,
}

struct SourceCollector<'s, 'a> {
    semantic: &'s Semantic<'a>,
    use_css_module_symbols: Vec<SymbolId>,
    vue_namespace_symbols: Vec<SymbolId>,
    analysis: SourceAnalysis,
}

impl<'a> SourceCollector<'_, 'a> {
    fn symbol(&self, identifier: &IdentifierReference<'_>) -> Option<SymbolId> {
        self.semantic
            .scoping()
            .get_reference(identifier.reference_id.get()?)
            .symbol_id()
    }

    fn is_unbound(&self, identifier: &IdentifierReference<'_>) -> bool {
        self.symbol(identifier).is_none()
    }

    /// True when the expression is provably not the component instance: a
    /// literal-shaped value, or an identifier whose only binding is such a
    /// value and is never reassigned.
    fn provably_non_instance_expression(expression: &Expression<'_>) -> bool {
        matches!(
            expression.get_inner_expression(),
            Expression::ObjectExpression(_)
                | Expression::ArrayExpression(_)
                | Expression::StringLiteral(_)
                | Expression::NumericLiteral(_)
                | Expression::BigIntLiteral(_)
                | Expression::BooleanLiteral(_)
                | Expression::NullLiteral(_)
                | Expression::RegExpLiteral(_)
                | Expression::TemplateLiteral(_)
                | Expression::ArrowFunctionExpression(_)
                | Expression::FunctionExpression(_)
                | Expression::ClassExpression(_)
        )
    }

    fn provably_non_instance_symbol(&self, symbol: SymbolId) -> bool {
        if self.semantic.scoping().symbol_is_mutated(symbol) {
            return false;
        }
        match self.semantic.symbol_declaration(symbol).kind() {
            AstKind::VariableDeclarator(declarator) => declarator
                .init
                .as_ref()
                .is_some_and(Self::provably_non_instance_expression),
            AstKind::Function(_) | AstKind::Class(_) => true,
            _ => false,
        }
    }

    /// True when the symbol is a Vue namespace: an import binding, an
    /// assignment-propagated alias, or an unmutated const alias chain
    /// resolved through symbol declarations so declaration order and
    /// closure hoisting never hide the identity.
    fn is_namespace_symbol(&self, symbol: SymbolId, depth: u8) -> bool {
        if self.vue_namespace_symbols.contains(&symbol) {
            return true;
        }
        if depth == 0 || self.semantic.scoping().symbol_is_mutated(symbol) {
            return false;
        }
        match self.semantic.symbol_declaration(symbol).kind() {
            AstKind::VariableDeclarator(declarator) => {
                match declarator
                    .init
                    .as_ref()
                    .map(|init| init.get_inner_expression())
                {
                    Some(Expression::Identifier(identifier)) => self
                        .symbol(identifier)
                        .is_some_and(|inner| self.is_namespace_symbol(inner, depth - 1)),
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn is_vue_namespace_object(&self, expression: &Expression<'_>) -> bool {
        matches!(
            expression.get_inner_expression(),
            Expression::Identifier(identifier)
                if self.symbol(identifier).is_some_and(|symbol| self.is_namespace_symbol(symbol, 8))
        )
    }

    /// A computed key that is provably the string `$style`: an inline
    /// literal, or an identifier whose only binding is an unmutated string
    /// literal initializer.
    fn is_style_key(&self, expression: &Expression<'_>) -> bool {
        match expression.get_inner_expression() {
            Expression::StringLiteral(literal) => literal.value == "$style",
            Expression::Identifier(identifier) => self.symbol(identifier).is_some_and(|symbol| {
                if self.semantic.scoping().symbol_is_mutated(symbol) {
                    return false;
                }
                match self.semantic.symbol_declaration(symbol).kind() {
                    AstKind::VariableDeclarator(declarator) => matches!(
                        declarator.init.as_ref().map(|init| init.get_inner_expression()),
                        Some(Expression::StringLiteral(literal)) if literal.value == "$style"
                    ),
                    _ => false,
                }
            }),
            _ => false,
        }
    }

    /// The old text scan treated every `$style` access as CSS Module usage.
    /// Semantic analysis relaxes that only for objects provably unrelated
    /// to the component instance; aliases through assignment, calls such as
    /// `getCurrentInstance().proxy`, and unbound names all stay usage.
    fn may_be_instance(&self, expression: &Expression<'_>) -> bool {
        match expression.get_inner_expression() {
            Expression::Identifier(identifier) => self
                .symbol(identifier)
                .is_none_or(|symbol| !self.provably_non_instance_symbol(symbol)),
            expression => !Self::provably_non_instance_expression(expression),
        }
    }

    fn collect_glob_argument(&mut self, argument: &Argument<'_>) -> bool {
        match argument {
            Argument::StringLiteral(literal) => {
                self.analysis
                    .vue_glob_patterns
                    .push(literal.value.to_string());
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

impl<'a> Visit<'a> for SourceCollector<'_, 'a> {
    /// A rendered `<link rel="stylesheet">` loads a sheet exactly like an
    /// HTML document link, so its target joins spelling reservations. A
    /// rel or href the parser cannot read statically, or a spread that may
    /// supply them, leaves the loaded target unknowable.
    fn visit_jsx_opening_element(&mut self, element: &oxc_ast::ast::JSXOpeningElement<'a>) {
        if let oxc_ast::ast::JSXElementName::Identifier(name) = &element.name
            && name.name == "style"
        {
            self.analysis.renders_style_element = true;
        }
        // A dynamic style value can set any custom property at runtime,
        // and a prop spread can supply a style prop the same way. JSX
        // props apply left to right, so a literal-object style after the
        // last spread pins the value again; a literal object is
        // transparent because its custom-property keys are string
        // literals the literal visitor already collects.
        let mut style_opaque = false;
        for attribute in &element.attributes {
            match attribute {
                oxc_ast::ast::JSXAttributeItem::SpreadAttribute(_) => style_opaque = true,
                oxc_ast::ast::JSXAttributeItem::Attribute(attribute) => {
                    let oxc_ast::ast::JSXAttributeName::Identifier(attribute_name) =
                        &attribute.name
                    else {
                        continue;
                    };
                    if attribute_name.name == "dangerouslySetInnerHTML" {
                        self.analysis.injects_markup = true;
                    }
                    if attribute_name.name != "style" {
                        continue;
                    }
                    let transparent = match &attribute.value {
                        Some(oxc_ast::ast::JSXAttributeValue::ExpressionContainer(container)) => {
                            match container.expression.as_expression() {
                                Some(Expression::ObjectExpression(object)) => {
                                    object.properties.iter().all(|property| {
                                        matches!(
                                            property,
                                            oxc_ast::ast::ObjectPropertyKind::ObjectProperty(entry)
                                                if !entry.computed
                                        )
                                    })
                                }
                                _ => false,
                            }
                        }
                        _ => true,
                    };
                    style_opaque = !transparent;
                }
            }
        }
        if style_opaque {
            self.analysis.custom_properties_unbounded = true;
        }
        if let oxc_ast::ast::JSXElementName::Identifier(name) = &element.name
            && name.name == "link"
        {
            let mut rel = None;
            let mut href = None;
            let mut spread = false;
            for attribute in &element.attributes {
                match attribute {
                    oxc_ast::ast::JSXAttributeItem::Attribute(attribute) => {
                        let oxc_ast::ast::JSXAttributeName::Identifier(attribute_name) =
                            &attribute.name
                        else {
                            continue;
                        };
                        let value = match &attribute.value {
                            Some(oxc_ast::ast::JSXAttributeValue::StringLiteral(literal)) => {
                                Some(literal.value.as_str())
                            }
                            _ => None,
                        };
                        match attribute_name.name.as_str() {
                            "rel" => rel = Some(value),
                            "href" => href = Some(value),
                            _ => {}
                        }
                    }
                    // JSX props apply left to right, so a spread replaces
                    // any earlier explicit value with a runtime one; a
                    // later explicit attribute pins the value again.
                    oxc_ast::ast::JSXAttributeItem::SpreadAttribute(_) => {
                        spread = true;
                        if rel.is_some() {
                            rel = Some(None);
                        }
                        if href.is_some() {
                            href = Some(None);
                        }
                    }
                }
            }
            let may_be_stylesheet = match rel {
                Some(Some(value)) => value
                    .split_ascii_whitespace()
                    .any(|token| token.eq_ignore_ascii_case("stylesheet")),
                // A dynamic rel, or a spread that may supply one, can
                // resolve to a stylesheet at runtime.
                Some(None) => true,
                None => spread,
            };
            if may_be_stylesheet {
                match href {
                    Some(Some(value)) => {
                        self.analysis.stylesheet_links.push(value.to_string());
                    }
                    _ => self.analysis.stylesheet_links_unverifiable = true,
                }
            }
        }
        walk::walk_jsx_opening_element(self, element);
    }

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
        // An import attribute clause constructs a stylesheet object without
        // applying it, so it never counts as an applied static import.
        if !type_only && decl.phase.is_none() && decl.with_clause.is_none() {
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
        // Const alias chains resolve on demand through is_namespace_symbol,
        // so only helper destructures need collection here.
        if let Some(init) = &declarator.init
            && self.is_vue_namespace_object(init)
            && let BindingPattern::ObjectPattern(pattern) = &declarator.id
        {
            for property in &pattern.properties {
                if property.key.is_specific_static_name("useCssModule")
                    && let BindingPattern::BindingIdentifier(identifier) = &property.value
                    && let Some(symbol) = identifier.symbol_id.get()
                {
                    self.use_css_module_symbols.push(symbol);
                    self.analysis
                        .use_css_module_locals
                        .push(identifier.name.to_string());
                }
            }
        }
        if let Some(init) = &declarator.init
            && self.may_be_instance(init)
            && let BindingPattern::ObjectPattern(pattern) = &declarator.id
            && pattern
                .properties
                .iter()
                .any(|property| property.key.is_specific_static_name("$style"))
        {
            self.analysis.uses_css_module = true;
        }
        walk::walk_variable_declarator(self, declarator);
    }

    fn visit_assignment_expression(&mut self, assignment: &oxc_ast::ast::AssignmentExpression<'a>) {
        // Assigning a namespace after declaration carries its identity to
        // the target, matching the declarator alias path.
        if let AssignmentTarget::AssignmentTargetIdentifier(target) = &assignment.left
            && self.is_vue_namespace_object(&assignment.right)
            && let Some(symbol) = self.symbol(target)
        {
            self.vue_namespace_symbols.push(symbol);
        }
        // Assignment destructuring reads `$style` off the instance just like
        // a declarator pattern does.
        if let AssignmentTarget::ObjectAssignmentTarget(pattern) = &assignment.left
            && self.may_be_instance(&assignment.right)
            && pattern.properties.iter().any(|property| match property {
                AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(shorthand) => {
                    shorthand.binding.name == "$style"
                }
                AssignmentTargetProperty::AssignmentTargetPropertyProperty(named) => {
                    named.name.is_specific_static_name("$style")
                }
            })
        {
            self.analysis.uses_css_module = true;
        }
        walk::walk_assignment_expression(self, assignment);
    }

    fn visit_call_expression(&mut self, call: &oxc_ast::ast::CallExpression<'a>) {
        // A style property API taking a computed name can touch any custom
        // property, so allocation cannot prove a free name.
        if let Expression::StaticMemberExpression(member) = &call.callee
            && matches!(
                member.property.name.as_str(),
                "setProperty" | "getPropertyValue" | "removeProperty"
            )
            && !matches!(call.arguments.first(), Some(Argument::StringLiteral(_)) | None)
        {
            self.analysis.custom_properties_unbounded = true;
        }
        if let Expression::Identifier(callee) = &call.callee {
            if callee.name == "require" {
                if let Some(Argument::StringLiteral(literal)) = call.arguments.first() {
                    self.analysis.imports.push(SourceImport {
                        specifier: literal.value.to_string(),
                        type_only: false,
                        dynamic: true,
                    });
                } else if self.is_unbound(callee) {
                    // A CommonJS require whose target cannot be read
                    // statically may load any module, so caller surfaces
                    // stay open; a locally bound `require` is not a loader.
                    self.analysis.has_dynamic_import = true;
                }
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
            if member.property.name == "context"
                && matches!(&member.object, Expression::Identifier(object) if object.name == "require")
            {
                // webpack's require.context selects modules at runtime, so
                // the caller surface stays open like any dynamic loader.
                self.analysis.has_dynamic_import = true;
            } else if member.property.name == "glob"
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
                && self.is_vue_namespace_object(&member.object)
            {
                self.analysis.uses_css_module = true;
            }
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_template_literal(&mut self, template: &oxc_ast::ast::TemplateLiteral<'a>) {
        collect_template_prefixes(template, &mut self.analysis.template_prefixes);
        walk::walk_template_literal(self, template);
    }

    fn visit_binary_expression(&mut self, expression: &oxc_ast::ast::BinaryExpression<'a>) {
        collect_concat_prefixes(expression, &mut self.analysis.template_prefixes);
        walk::walk_binary_expression(self, expression);
    }

    /// Any `--` string literal may name a custom property this run would
    /// otherwise allocate, so it reserves the spelling wherever it
    /// appears; ownership precision is not worth chasing call shapes.
    fn visit_string_literal(&mut self, literal: &oxc_ast::ast::StringLiteral<'a>) {
        if literal.value.starts_with("--") {
            self.analysis.custom_properties.push(literal.value.to_string());
        }
        walk::walk_string_literal(self, literal);
    }

    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        if identifier.name == "$style" && self.is_unbound(identifier) {
            self.analysis.uses_css_module = true;
        }
        if self.is_unbound(identifier) {
            if identifier.name == "useCssModule" {
                self.analysis.has_unbound_use_css_module = true;
            }
            if !self
                .analysis
                .unbound_references
                .iter()
                .any(|name| name == identifier.name.as_str())
            {
                self.analysis
                    .unbound_references
                    .push(identifier.name.to_string());
            }
        }
        if self
            .symbol(identifier)
            .is_some_and(|symbol| self.use_css_module_symbols.contains(&symbol))
        {
            self.analysis.uses_css_module = true;
        }
        walk::walk_identifier_reference(self, identifier);
    }

    fn visit_static_member_expression(
        &mut self,
        member: &oxc_ast::ast::StaticMemberExpression<'a>,
    ) {
        if member.property.name == "$style" && self.may_be_instance(&member.object) {
            self.analysis.uses_css_module = true;
        }
        if member.property.name == "useCssModule" && self.is_vue_namespace_object(&member.object) {
            self.analysis.uses_css_module = true;
        }
        walk::walk_static_member_expression(self, member);
    }

    fn visit_computed_member_expression(
        &mut self,
        member: &oxc_ast::ast::ComputedMemberExpression<'a>,
    ) {
        if self.is_style_key(&member.expression) && self.may_be_instance(&member.object) {
            self.analysis.uses_css_module = true;
        }
        if matches!(&member.expression, Expression::StringLiteral(literal) if literal.value == "useCssModule")
            && self.is_vue_namespace_object(&member.object)
        {
            self.analysis.uses_css_module = true;
        }
        walk::walk_computed_member_expression(self, member);
    }
}

/// Parse and semantically analyze one JavaScript or TypeScript source once,
/// returning every fact the TypeScript orchestration layer needs.
pub fn source_analysis_json(path: &str, source: &str) -> MigrationResult<String> {
    let source_type = source_type_for_path(path)
        .map_err(|message| MigrationError::UnsupportedSource { message })?;
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return Err(MigrationError::SourceParse {
            message: format!("Failed to parse {path}"),
        });
    }
    // Instance-alias checks resolve symbols back to their declaring node,
    // which needs the full node store rather than the compiler profile.
    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .with_check_syntax_error(true)
        .build(&parsed.program);
    if !semantic.diagnostics.is_empty() {
        return Err(MigrationError::SourceAnalysis {
            message: format!("Failed to analyze {path}"),
        });
    }
    let mut use_css_module_symbols = Vec::new();
    let mut use_css_module_locals: Vec<String> = Vec::new();
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
                        use_css_module_locals.push(specifier.local.name.to_string());
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
        semantic: &semantic.semantic,
        use_css_module_symbols,
        vue_namespace_symbols,
        analysis: SourceAnalysis {
            imports: Vec::new(),
            static_imports: Vec::new(),
            default_imports: Vec::new(),
            vue_glob_patterns: Vec::new(),
            vue_glob_unverifiable: false,
            has_dynamic_import: false,
            has_vue_fallthrough_macro: false,
            uses_css_module: false,
            has_unbound_use_css_module: false,
            defines_root_use_css_module: false,
            template_prefixes: Vec::new(),
            use_css_module_locals: use_css_module_locals.clone(),
            unbound_references: Vec::new(),
            stylesheet_links: Vec::new(),
            stylesheet_links_unverifiable: false,
            renders_style_element: false,
            injects_markup: false,
            custom_properties: Vec::new(),
            custom_properties_unbounded: false,
        },
    };
    collector.visit_program(&parsed.program);
    // Classified after the visit so helpers destructured from a Vue
    // namespace count as the Vue API rather than a local shadow.
    let scoping = semantic.semantic.scoping();
    collector.analysis.defines_root_use_css_module = scoping
        .get_binding(scoping.root_scope_id(), "useCssModule".into())
        .is_some_and(|symbol| !collector.use_css_module_symbols.contains(&symbol));
    collector.analysis.custom_properties.sort();
    collector.analysis.custom_properties.dedup();
    serde_json::to_string(&collector.analysis).map_err(|error| MigrationError::Serialization {
        message: error.to_string(),
    })
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
        for (source, static_string, member, uses_css_module, references_api) in [
            ("'card'", Some("card"), None, false, false),
            ("`card`", Some("card"), None, false, false),
            ("$style.card", None, Some("card"), true, false),
            ("active ? $style.card : ''", None, None, true, false),
            ("useCssModule().card", None, None, false, true),
            ("vm.$style.card", None, None, true, false),
            (
                "'$style.card useCssModule()'",
                Some("$style.card useCssModule()"),
                None,
                false,
                false,
            ),
        ] {
            let parsed: serde_json::Value = serde_json::from_str(
                &super::expression_analysis_json("Component.js", source).unwrap(),
            )
            .unwrap();
            assert_eq!(
                parsed["staticString"],
                serde_json::json!(static_string),
                "{source}"
            );
            assert_eq!(
                parsed["vueModuleMember"],
                serde_json::json!(member),
                "{source}"
            );
            assert_eq!(parsed["usesCssModule"], uses_css_module, "{source}");
            assert_eq!(parsed["referencesUseCssModule"], references_api, "{source}");
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
    fn excludes_import_attribute_clauses_from_static_imports() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.vue.ts",
                "import sheet from './card.css' with { type: 'css' };\nimport './applied.css';\nvoid sheet;\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed["staticImports"],
            serde_json::json!(["./applied.css"])
        );
    }

    #[test]
    fn rejects_unparseable_sources() {
        assert!(super::source_analysis_json("/p/x.ts", "import from from;").is_err());
    }

    #[test]
    fn marks_nonliteral_requires_dynamic() {
        for source in [
            "const name = 'Card'; require(`./${name}.vue`);",
            "require.context('./components', true, /\\.vue$/);",
        ] {
            let parsed: serde_json::Value = serde_json::from_str(
                &super::source_analysis_json("/p/registry.js", source).unwrap(),
            )
            .unwrap();
            assert_eq!(parsed["imports"], serde_json::json!([]), "{source}");
            assert_eq!(parsed["hasDynamicImport"], true, "{source}");
        }
    }

    #[test]
    fn ignores_locally_bound_require_helpers() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/registry.js",
                "const require = (name) => name; const x = 'Card'; require(`./${x}.vue`);",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["hasDynamicImport"], false);
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
            &super::source_analysis_json("/p/Card.vue.js", "import { useCssModule } from 'vue';")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["usesCssModule"], false);
        // The binding exists but is the Vue API, not a local shadow.
        assert_eq!(parsed["definesRootUseCssModule"], false);
    }

    #[test]
    fn lists_helper_locals_and_unbound_references() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.vue.ts",
                "import { useCssModule as css } from 'vue';\nimport * as Vue from 'vue';\nconst { useCssModule: helper } = Vue;\nfirst(); second();\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed["useCssModuleLocals"],
            serde_json::json!(["css", "helper"])
        );
        assert_eq!(
            parsed["unboundReferences"],
            serde_json::json!(["first", "second"])
        );
    }

    #[test]
    fn collects_constrained_template_prefixes() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.tsx",
                "const cls = `mr-${side}`; const combined = `base p-${size} pt-${top}`; const open = `loose ${value}`; const fixed = `static`;",
            )
            .unwrap(),
        )
        .unwrap();
        // Trailing tokens of pre-expression quasis constrain the dynamic
        // class; static tokens and whitespace-terminated quasis do not.
        assert_eq!(parsed["templatePrefixes"], serde_json::json!(["mr-", "p-", "pt-"]));
    }

    #[test]
    fn marks_injected_markup_sources() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.tsx",
                "export const Card = ({ markup }) => <div dangerouslySetInnerHTML={{ __html: markup }} />;",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["injectsMarkup"], true);
    }

    #[test]
    fn treats_prop_spreads_as_hiding_style_overrides() {
        let spread: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.tsx",
                "export const Card = (props) => <div {...props} className=\"x\" />;",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(spread["customPropertiesUnbounded"], true);

        // A literal style after the spread pins the value again.
        let repinned: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.tsx",
                "export const Card = (props) => <div {...props} style={{ width: 4 }} />;",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(repinned["customPropertiesUnbounded"], false);
    }

    #[test]
    fn treats_dynamic_jsx_styles_as_unbounded_properties() {
        let dynamic: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.tsx",
                "export const Card = (props) => <div style={props.style} />;",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(dynamic["customPropertiesUnbounded"], true);

        // A literal object is transparent: custom-property keys are
        // string literals the literal visitor collects.
        let literal: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.tsx",
                "export const Card = () => <div style={{ width: 4, \"--font-brand\": \"serif\" }} />;",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(literal["customPropertiesUnbounded"], false);
        assert_eq!(literal["customProperties"], serde_json::json!(["--font-brand"]));
    }

    #[test]
    fn collects_custom_property_literals_and_dynamic_property_apis() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Theme.ts",
                "const token = \"--font-brand\";\ndocument.body.style.setProperty(token, value);\nelement.style.getPropertyValue(\"--gap\");\nconst plain = \"font-brand\";",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed["customProperties"],
            serde_json::json!(["--font-brand", "--gap"]),
        );
        // setProperty received a computed name.
        assert_eq!(parsed["customPropertiesUnbounded"], true);

        let bounded: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Theme.ts",
                "element.style.setProperty(\"--font-brand\", value);\nelement.style.removeProperty(\"--font-brand\");",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(bounded["customPropertiesUnbounded"], false);
    }

    #[test]
    fn collects_jsx_stylesheet_links() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Layout.tsx",
                "export const Layout = () => (<head><link rel=\"stylesheet\" href=\"https://cdn.example/legacy.css\" /><link rel=\"icon\" href=\"/favicon.ico\" /><link rel=\"preload stylesheet\" href=\"./theme.css\" /></head>);",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed["stylesheetLinks"],
            serde_json::json!(["https://cdn.example/legacy.css", "./theme.css"])
        );
        assert_eq!(parsed["stylesheetLinksUnverifiable"], false);

        let dynamic: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Layout.tsx",
                "export const Layout = ({ href }) => <link rel=\"stylesheet\" href={href} />;",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(dynamic["stylesheetLinksUnverifiable"], true);

        // A later spread overrides earlier explicit props, so the final
        // rel and href are runtime values; a spread before them is
        // repinned by the explicit attributes.
        let overridden: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Layout.tsx",
                "export const Layout = (props) => <link rel=\"icon\" href=\"/favicon.ico\" {...props} />;",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(overridden["stylesheetLinksUnverifiable"], true);

        let styled: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Page.tsx",
                "export const Page = () => <style>{`.mr-auto { margin-right: 1px }`}</style>;",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(styled["rendersStyleElement"], true);

        let repinned: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Layout.tsx",
                "export const Layout = (props) => <link {...props} rel=\"icon\" href=\"/favicon.ico\" />;",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(repinned["stylesheetLinksUnverifiable"], false);
        assert_eq!(repinned["stylesheetLinks"], serde_json::json!([]));
    }

    #[test]
    fn collects_constrained_concat_prefixes() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.tsx",
                "const cls = 'mr-' + side; const chained = base + 'pt-' + top; const loose = 'open ' + value; const numeric = count + 1;",
            )
            .unwrap(),
        )
        .unwrap();
        // The trailing static token before a dynamic part constrains the
        // class; whitespace-terminated and non-string additions do not.
        assert_eq!(parsed["templatePrefixes"], serde_json::json!(["mr-", "pt-"]));
    }

    #[test]
    fn distinguishes_local_use_css_module_shadows_from_unbound_references() {
        let shadowed: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.vue.js",
                "const useCssModule = () => {}; useCssModule();",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(shadowed["definesRootUseCssModule"], true);
        assert_eq!(shadowed["hasUnboundUseCssModule"], false);

        // A helper destructured from the Vue namespace is the Vue API, not
        // a local shadow, even when nothing in the script calls it.
        let destructured: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json(
                "/p/Card.vue.js",
                "import * as Vue from 'vue'; const { useCssModule } = Vue;",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(destructured["definesRootUseCssModule"], false);

        let unbound: serde_json::Value = serde_json::from_str(
            &super::source_analysis_json("/p/Card.vue.js", "const styles = useCssModule();")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(unbound["definesRootUseCssModule"], false);
        assert_eq!(unbound["hasUnboundUseCssModule"], true);
    }

    #[test]
    fn recognizes_destructured_options_api_css_module_references() {
        for source in [
            "export default { mounted() { const { $style: styles } = this; void styles.card; } };",
            "export default { mounted() { const { $style } = this; void $style.card; } };",
            "export default { mounted() { const vm = this; void vm.$style.card; } };",
            "export default { mounted() { const vm = this; const self = vm; void self['$style'].card; } };",
            "export default { mounted() { const key = '$style'; void this[key].card; } };",
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

        for source in [
            "const value = {}; const { $style } = value; void $style.card;",
            "const value = {}; let styles; ({ $style: styles } = value); void styles.card;",
        ] {
            let unrelated: serde_json::Value = serde_json::from_str(
                &super::source_analysis_json("/p/Card.vue.js", source).unwrap(),
            )
            .unwrap();
            assert_eq!(unrelated["usesCssModule"], false, "{source}");
        }
    }

    #[test]
    fn recognizes_untracked_instance_aliases() {
        for source in [
            "let vm; vm = this; void vm.$style.card;",
            "let styles; ({ $style: styles } = this); void styles.card;",
            "export default { mounted() { let s; ({ $style: s } = this); void s.card; } };",
            "export default { mounted() { later(); const vm = this; function later() { void vm.$style.card; } } };",
            "import { getCurrentInstance } from 'vue'; void getCurrentInstance().proxy.$style.card;",
            "import { getCurrentInstance } from 'vue'; const instance = getCurrentInstance(); void instance.proxy.$style.card;",
        ] {
            let parsed: serde_json::Value = serde_json::from_str(
                &super::source_analysis_json("/p/Card.vue.ts", source).unwrap(),
            )
            .unwrap();
            assert_eq!(parsed["usesCssModule"], true, "{source}");
        }
    }

    #[test]
    fn recognizes_computed_vue_namespace_css_module_references() {
        for source in [
            "import * as Vue from 'vue'; Vue['useCssModule']();",
            "import * as Vue from 'vue'; const V = Vue; V.useCssModule();",
            "import * as Vue from 'vue'; const V = Vue as typeof Vue; V.useCssModule();",
            "import * as Vue from 'vue'; let V; V = Vue; V.useCssModule();",
            "import * as Vue from 'vue'; function read() { return V.useCssModule(); } const V = Vue; read();",
            "import * as Vue from 'vue'; const V = Vue; const W = V; W.useCssModule();",
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
        assert_eq!(parsed["hasUnboundUseCssModule"], false);
        assert_eq!(parsed["definesRootUseCssModule"], true);
    }
}

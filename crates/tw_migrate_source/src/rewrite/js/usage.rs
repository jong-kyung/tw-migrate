use std::collections::{BTreeSet, HashMap};

use oxc_ast::ast::{
    ComputedMemberExpression, Expression, JSXAttribute, JSXAttributeItem, JSXAttributeName,
    JSXAttributeValue, JSXExpression, JSXOpeningElement, StaticMemberExpression, TemplateLiteral,
    VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};
use oxc_semantic::Scoping;
use oxc_span::{GetSpan, Span};
use oxc_syntax::symbol::SymbolId;
use tw_migrate_css::{
    SelectorKey, css_properties_conflict, tailwind_utilities_conflict, tailwind_variants_match,
    utility_conflict,
};

use super::{CandidateMatch, ImportBinding};
use crate::{Edit, Warning};

struct StaticTemplate {
    value: String,
    members: Vec<String>,
    static_classes: Vec<String>,
    partial_edits: Vec<Edit>,
    preserved_candidates: Vec<String>,
    preserved_expression: Option<String>,
    append_at: Option<usize>,
}

/// Collect the class-producing result leaves of the bounded expression
/// grammar: the right operand of `&&`, `||`, and `??`, both branches of a
/// conditional expression, and anything nested through those positions.
/// Parentheses and TypeScript `as`, `satisfies`, and non-null wrappers are
/// transparent. Conditions and logical left operands stay opaque.
fn collect_result_leaves<'b, 'a>(
    expression: &'b Expression<'a>,
    leaves: &mut Vec<&'b Expression<'a>>,
) {
    match expression.get_inner_expression() {
        Expression::LogicalExpression(logical) => collect_result_leaves(&logical.right, leaves),
        Expression::ConditionalExpression(conditional) => {
            collect_result_leaves(&conditional.consequent, leaves);
            collect_result_leaves(&conditional.alternate, leaves);
        }
        leaf => leaves.push(leaf),
    }
}

/// The bounded dynamic grammar applies only when the className expression is
/// a logical or conditional expression under transparent wrappers; direct
/// static forms keep their existing handling.
fn class_expression_leaves<'b, 'a>(
    expression: &'b JSXExpression<'a>,
) -> Option<Vec<&'b Expression<'a>>> {
    let expression = expression.as_expression()?.get_inner_expression();
    matches!(
        expression,
        Expression::LogicalExpression(_) | Expression::ConditionalExpression(_)
    )
    .then(|| {
        let mut leaves = Vec::new();
        collect_result_leaves(expression, &mut leaves);
        leaves
    })
}

/// A JS string literal for an expression position, keeping the original
/// quote style when the value permits it.
fn js_string_literal(value: &str, preferred_quote: u8) -> String {
    let quote = preferred_quote as char;
    if (quote == '"' || quote == '\'')
        && !value.contains([quote, '\\'])
        && !value.contains(|c: char| c.is_control())
    {
        format!("{quote}{value}{quote}")
    } else {
        serde_json::to_string(value).expect("string serialization")
    }
}

pub(super) struct UsageCollector<'s> {
    pub(super) source: &'s str,
    pub(super) file_path: &'s str,
    pub(super) is_module: bool,
    pub(super) scoping: &'s Scoping,
    pub(super) import_bindings: &'s [ImportBinding],
    pub(super) global_module_symbols: &'s [SymbolId],
    pub(super) candidates: &'s HashMap<SelectorKey, Vec<String>>,
    /// Union of transferred source properties per candidate; authoritative
    /// for conflicts when both sides carry metadata so canonical aliases
    /// are never classified by their class prefix.
    pub(super) candidate_properties: &'s HashMap<String, BTreeSet<String>>,
    pub(super) preserved_module_classes: &'s BTreeSet<String>,
    pub(super) edits: Vec<Edit>,
    pub(super) emitted_candidates: BTreeSet<String>,
    pub(super) matches: Vec<CandidateMatch>,
    pub(super) module_refs: HashMap<String, usize>,
    pub(super) matched_module_refs: HashMap<String, usize>,
    pub(super) class_name_depth: u32,
    pub(super) alias_spans: HashMap<u32, Span>,
    pub(super) computed_refs: usize,
    pub(super) unsafe_reference: bool,
    pub(super) warnings: Vec<Warning>,
}

impl UsageCollector<'_> {
    /// `null`, `undefined`, and `false` are warning-free no-op class leaves.
    /// `undefined` qualifies only as the unbound global; a local binding
    /// named `undefined` can hold a runtime class value, so it stays an
    /// unsupported leaf.
    fn is_noop_class_leaf(&self, expression: &Expression<'_>) -> bool {
        match expression {
            Expression::NullLiteral(_) => true,
            Expression::BooleanLiteral(literal) => !literal.value,
            Expression::Identifier(identifier) => {
                identifier.name == "undefined" && self.identifier_symbol(expression).is_none()
            }
            _ => false,
        }
    }

    /// Record a `dynamic-class-name` warning for an unsupported class site.
    fn warn_dynamic_class_name(&mut self, span: Span, message: &str) {
        self.warnings.push(Warning::new(
            "dynamic-class-name",
            self.file_path.to_string(),
            (span.start as usize, span.end as usize),
            message.to_string(),
        ));
    }

    /// Record one emitted candidate and its match site.
    fn record_match(&mut self, (start, end): (usize, usize), key: &SelectorKey, candidate: &str) {
        self.emitted_candidates.insert(candidate.to_string());
        self.matches.push(CandidateMatch {
            start,
            end,
            key: key.clone(),
            candidate: candidate.to_string(),
            origin_candidate: candidate.to_string(),
        });
    }

    fn identifier_symbol(&self, expr: &Expression<'_>) -> Option<SymbolId> {
        let Expression::Identifier(identifier) = expr else {
            return None;
        };
        let reference = identifier.reference_id.get()?;
        self.scoping.get_reference(reference).symbol_id()
    }

    fn is_module_object(&self, object: &Expression<'_>) -> bool {
        let Some(symbol) = self.identifier_symbol(object) else {
            return false;
        };
        self.import_bindings
            .iter()
            .any(|binding| binding.symbol == symbol)
    }

    fn module_member_name<'a>(&self, member: &'a StaticMemberExpression<'a>) -> Option<&'a str> {
        self.is_module_object(&member.object)
            .then(|| member.property.name.as_str())
    }

    /// Whether a global-path className expression is a member of a CSS Module
    /// import, which can never match a global class and is classified by the
    /// module's own plan instead.
    fn is_global_module_member(&self, expression: &JSXExpression<'_>) -> bool {
        let object = match expression {
            JSXExpression::StaticMemberExpression(member) => &member.object,
            JSXExpression::ComputedMemberExpression(member) => &member.object,
            _ => return false,
        };
        self.is_global_module_object(object)
    }

    /// Whether `object` is a CSS Module import binding on the global path.
    fn is_global_module_object(&self, object: &Expression<'_>) -> bool {
        let Some(symbol) = self.identifier_symbol(object) else {
            return false;
        };
        self.global_module_symbols.contains(&symbol)
    }

    fn static_template(&self, template: &TemplateLiteral<'_>) -> Option<StaticTemplate> {
        let mut value = String::new();
        let mut original = String::new();
        let mut members = Vec::new();
        let mut member_edits = Vec::new();
        let mut preserved_candidates = Vec::new();
        let mut partial = false;
        for (index, quasi) in template.quasis.iter().enumerate() {
            let cooked = quasi.value.cooked.as_ref()?.as_str();
            value.push_str(cooked);
            original.push_str(cooked);
            let Some(expression) = template.expressions.get(index) else {
                continue;
            };
            match expression {
                Expression::StaticMemberExpression(member) => {
                    let Some(name) = self.module_member_name(member).map(str::to_string) else {
                        partial = true;
                        original.push('\0');
                        continue;
                    };
                    let Some(candidates) = self.candidates.get(&SelectorKey::Class(name.clone()))
                    else {
                        return Some(StaticTemplate {
                            value: String::new(),
                            members: Vec::new(),
                            static_classes: Vec::new(),
                            partial_edits: Vec::new(),
                            preserved_candidates: Vec::new(),
                            preserved_expression: None,
                            append_at: None,
                        });
                    };
                    original.push('\0');
                    members.push(name.clone());
                    if self.preserved_module_classes.contains(&name) {
                        partial = true;
                        preserved_candidates.extend(candidates.iter().cloned());
                    } else {
                        let replacement = candidates.join(" ");
                        value.push_str(&replacement);
                        member_edits.push(Edit {
                            start: member.span.start as usize,
                            end: member.span.end as usize,
                            replacement: serde_json::to_string(&replacement)
                                .expect("string serialization"),
                        });
                    }
                }
                Expression::StringLiteral(literal) => {
                    value.push_str(literal.value.as_str());
                    original.push_str(literal.value.as_str());
                }
                _ => return None,
            }
        }
        let static_classes = original
            .split_whitespace()
            .filter(|class| !class.contains('\0'))
            .map(str::to_string)
            .collect();
        Some(StaticTemplate {
            value: value.split_whitespace().collect::<Vec<_>>().join(" "),
            members,
            static_classes,
            partial_edits: if partial { member_edits } else { Vec::new() },
            preserved_candidates,
            preserved_expression: None,
            append_at: partial.then_some(template.span.end as usize - 1),
        })
    }

    fn conflicting_utilities(
        &self,
        members: &[String],
        static_classes: &[String],
    ) -> Option<(String, String)> {
        let existing = static_classes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        for member in members {
            let candidates = self.candidates.get(&SelectorKey::Class(member.clone()))?;
            if let Some(conflict) = utility_conflict(candidates, &existing) {
                return Some(conflict);
            }
        }
        None
    }

    fn candidates_conflict(&self, left: &str, right: &str) -> bool {
        // Identical spellings emit one effective class, matching the
        // heuristic's equality exit.
        if left == right {
            return false;
        }
        if let (Some(left_properties), Some(right_properties)) = (
            self.candidate_properties.get(left),
            self.candidate_properties.get(right),
        ) && !left_properties.is_empty()
            && !right_properties.is_empty()
        {
            // Utilities under different variant chains occupy different
            // slots and never compete, matching the spelling heuristic's
            // variant gate.
            return tailwind_variants_match(left, right)
                && left_properties.iter().any(|left_property| {
                    right_properties
                        .iter()
                        .any(|right_property| css_properties_conflict(left_property, right_property))
                });
        }
        tailwind_utilities_conflict(left, right)
    }

    fn conflicting_member_utilities(&self, members: &[String]) -> Option<(String, String)> {
        // Utilities generated from different module classes are ordered by
        // Tailwind's stylesheet, not by the source CSS cascade, so an
        // overlapping pair must retain its rules instead of migrating.
        for (index, left) in members.iter().enumerate() {
            let left_candidates = self.candidates.get(&SelectorKey::Class(left.clone()))?;
            for right in &members[index + 1..] {
                let right_candidates = self.candidates.get(&SelectorKey::Class(right.clone()))?;
                for left_candidate in left_candidates {
                    if let Some(conflict) = right_candidates.iter().find(|right_candidate| {
                        self.candidates_conflict(left_candidate, right_candidate)
                    }) {
                        return Some((left_candidate.clone(), conflict.clone()));
                    }
                }
            }
        }
        None
    }

    fn global_element(&mut self, element: &JSXOpeningElement<'_>) {
        let mut id_candidates = Vec::new();
        let mut class_literal = None;
        let mut has_class_name = false;

        for item in &element.attributes {
            let JSXAttributeItem::Attribute(attribute) = item else {
                continue;
            };
            let JSXAttributeName::Identifier(name) = &attribute.name else {
                continue;
            };
            if name.name == "className" {
                has_class_name = true;
                match &attribute.value {
                    Some(JSXAttributeValue::StringLiteral(literal)) => {
                        class_literal = Some((literal.span, literal.value.to_string()));
                    }
                    Some(JSXAttributeValue::ExpressionContainer(container)) => {
                        match &container.expression {
                            JSXExpression::StringLiteral(literal) => {
                                class_literal = Some((container.span, literal.value.to_string()));
                            }
                            JSXExpression::TemplateLiteral(template)
                                if template.expressions.is_empty()
                                    && template
                                        .quasis
                                        .first()
                                        .is_some_and(|quasi| quasi.value.cooked.is_some()) =>
                            {
                                let cooked = template.quasis[0].value.cooked.as_ref().unwrap();
                                class_literal = Some((container.span, cooked.to_string()));
                            }
                            expression => {
                                if let Some(leaves) = class_expression_leaves(expression) {
                                    for leaf in leaves {
                                        self.global_result_leaf(leaf);
                                    }
                                } else if !self.is_global_module_member(expression) {
                                    self.warn_dynamic_class_name(
                                        container.span,
                                        "Only static className values are supported.",
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
            } else if name.name == "id"
                && let Some(JSXAttributeValue::StringLiteral(literal)) = &attribute.value
            {
                let key = SelectorKey::Id(literal.value.to_string());
                if let Some(candidates) = self.candidates.get(&key) {
                    id_candidates.extend(
                        candidates
                            .iter()
                            .cloned()
                            .map(|candidate| (key.clone(), candidate)),
                    );
                }
            }
        }

        if let Some((span, value)) = class_literal {
            self.global_literal_edit(span, &value, &id_candidates);
        } else if !has_class_name && !id_candidates.is_empty() {
            let end = element.span.end as usize;
            let mut insertion = if self.source[..end].ends_with("/>") {
                end - 2
            } else {
                end - 1
            };
            while insertion > element.span.start as usize
                && self.source.as_bytes()[insertion - 1].is_ascii_whitespace()
            {
                insertion -= 1;
            }
            for (key, candidate) in &id_candidates {
                self.record_match((insertion, insertion), key, candidate);
            }
            self.edits.push(Edit {
                start: insertion,
                end: insertion,
                replacement: format!(
                    " className={}",
                    jsx_attribute_value(
                        &id_candidates
                            .iter()
                            .map(|(_, candidate)| candidate.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                ),
            });
        }
    }

    fn global_literal_edit(
        &mut self,
        span: Span,
        value: &str,
        extra_candidates: &[(SelectorKey, String)],
    ) {
        let Some(replacement_value) = self.appended_class_value(span, value, extra_candidates)
        else {
            return;
        };
        self.edits.push(Edit {
            start: span.start as usize,
            end: span.end as usize,
            replacement: jsx_attribute_value(&replacement_value),
        });
    }

    /// Append this stylesheet's candidates for `value`'s class tokens (plus
    /// `extra_candidates`), recording matches at `span`. Returns the new
    /// class value, or `None` when nothing changes.
    fn appended_class_value(
        &mut self,
        span: Span,
        value: &str,
        extra_candidates: &[(SelectorKey, String)],
    ) -> Option<String> {
        let mut classes = value
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let class_candidates = classes
            .iter()
            .filter_map(|class| {
                let key = SelectorKey::Class(class.clone());
                self.candidates
                    .get(&key)
                    .map(|candidates| (key, candidates.clone()))
            })
            .flat_map(|(key, candidates)| {
                candidates
                    .into_iter()
                    .map(move |candidate| (key.clone(), candidate))
            })
            .collect::<Vec<_>>();
        for (key, candidate) in class_candidates.iter().chain(extra_candidates) {
            self.record_match((span.start as usize, span.end as usize), key, candidate);
            if !classes.contains(candidate) {
                classes.push(candidate.clone());
            }
        }
        let replacement_value = classes.join(" ");
        (replacement_value != value).then_some(replacement_value)
    }

    /// Append candidates to a string-like leaf and rewrite it as a JS string
    /// literal delimited by `quote` when the value permits it.
    fn global_string_leaf(&mut self, span: Span, value: &str, quote: u8) {
        let has_candidates = value.split_whitespace().any(|class| {
            self.candidates
                .contains_key(&SelectorKey::Class(class.to_string()))
        });
        if !has_candidates {
            return;
        }
        let Some(replacement_value) = self.appended_class_value(span, value, &[]) else {
            return;
        };
        self.edits.push(Edit {
            start: span.start as usize,
            end: span.end as usize,
            replacement: js_string_literal(&replacement_value, quote),
        });
    }

    /// Plan one supported result leaf of a global className expression.
    fn global_result_leaf(&mut self, leaf: &Expression<'_>) {
        match leaf {
            Expression::StringLiteral(literal) => {
                self.global_string_leaf(
                    literal.span,
                    literal.value.as_str(),
                    self.source.as_bytes()[literal.span.start as usize],
                );
            }
            Expression::TemplateLiteral(template)
                if template.expressions.is_empty()
                    && template
                        .quasis
                        .first()
                        .is_some_and(|quasi| quasi.value.cooked.is_some()) =>
            {
                // A substitution-free template is a static value; like the
                // whole-attribute template handling, the rewrite emits a
                // plain string literal.
                let cooked = template.quasis[0].value.cooked.as_ref().unwrap().as_str();
                self.global_string_leaf(template.span, cooked, b'"');
            }
            Expression::StaticMemberExpression(member)
                if self.is_global_module_object(&member.object) => {}
            Expression::ComputedMemberExpression(member)
                if self.is_global_module_object(&member.object) => {}
            leaf if self.is_noop_class_leaf(leaf) => {}
            leaf => {
                self.warn_dynamic_class_name(
                    leaf.span(),
                    "Only static className values are supported.",
                );
            }
        }
    }
}

/// JSX string attributes have no escape sequences (the lexer scans to the
/// matching quote), so quote-bearing values must pick the other quote, and a
/// value with both quotes falls back to a JS string in an expression
/// container, where escapes do apply.
fn jsx_attribute_value(value: &str) -> String {
    if !value.contains('"') {
        format!("\"{value}\"")
    } else if !value.contains('\'') {
        format!("'{value}'")
    } else {
        format!(
            "{{{}}}",
            serde_json::to_string(value).expect("string serialization")
        )
    }
}

impl<'a> Visit<'a> for UsageCollector<'_> {
    fn visit_static_member_expression(&mut self, member: &StaticMemberExpression<'a>) {
        if let Some(name) = self.module_member_name(member).map(str::to_string) {
            *self.module_refs.entry(name).or_default() += 1;
            if self.class_name_depth == 0 {
                self.unsafe_reference = true;
                if let Some(declarator) = self.alias_spans.get(&member.span.start) {
                    self.warnings.push(Warning::new(
                        "aliased-css-module-reference",
                        self.file_path.to_string(),
                        (declarator.start as usize, declarator.end as usize),
                        "A CSS Module class is aliased to a binding, so the module is retained."
                            .to_string(),
                    ));
                } else {
                    self.warnings.push(Warning::new(
                        "non-classname-css-module-reference",
                        self.file_path.to_string(),
                        (member.span.start as usize, member.span.end as usize),
                        "A CSS Module class is used outside a supported className, so the module is retained."
                            .to_string(),
                    ));
                }
            }
        }
        walk::walk_static_member_expression(self, member);
    }

    fn visit_computed_member_expression(&mut self, member: &ComputedMemberExpression<'a>) {
        if self.is_module_object(&member.object) {
            self.computed_refs += 1;
            self.unsafe_reference = true;
            self.warnings.push(Warning::new(
                "computed-css-module-reference",
                self.file_path.to_string(),
                (member.span.start as usize, member.span.end as usize),
                "A computed CSS Module access cannot be verified, so the module is retained."
                    .to_string(),
            ));
        }
        walk::walk_computed_member_expression(self, member);
    }

    fn visit_variable_declarator(&mut self, declarator: &VariableDeclarator<'a>) {
        if let Some(Expression::StaticMemberExpression(member)) = &declarator.init
            && self.module_member_name(member).is_some()
        {
            self.alias_spans.insert(member.span.start, declarator.span);
        }
        walk::walk_variable_declarator(self, declarator);
    }

    fn visit_jsx_attribute(&mut self, attribute: &JSXAttribute<'a>) {
        let class_name = matches!(
            &attribute.name,
            JSXAttributeName::Identifier(name) if name.name == "className"
        );
        if class_name {
            self.class_name_depth += 1;
        }
        walk::walk_jsx_attribute(self, attribute);
        if class_name {
            self.class_name_depth -= 1;
        }
    }

    fn visit_jsx_opening_element(&mut self, element: &JSXOpeningElement<'a>) {
        if !self.is_module {
            self.global_element(element);
            walk::walk_jsx_opening_element(self, element);
            return;
        }

        for item in &element.attributes {
            let JSXAttributeItem::Attribute(attribute) = item else {
                continue;
            };
            let JSXAttributeName::Identifier(name) = &attribute.name else {
                continue;
            };
            if name.name != "className" {
                continue;
            }
            let Some(JSXAttributeValue::ExpressionContainer(container)) = &attribute.value else {
                continue;
            };
            if let Some(leaves) = class_expression_leaves(&container.expression) {
                for leaf in leaves {
                    self.module_result_leaf(leaf);
                }
                continue;
            }
            let template = match &container.expression {
                JSXExpression::StaticMemberExpression(member) => {
                    let Some(template) = self.member_site(member) else {
                        continue;
                    };
                    template
                }
                JSXExpression::TemplateLiteral(template) => {
                    let Some(result) = self.static_template(template) else {
                        self.warn_dynamic_class_name(
                            container.span,
                            "The template contains a dynamic or unsupported class.",
                        );
                        continue;
                    };
                    if result.members.is_empty() {
                        // No module members resolved: rewriting would only
                        // reformat an unrelated template literal.
                        continue;
                    }
                    result
                }
                _ => {
                    self.warn_dynamic_class_name(
                        container.span,
                        "Only static className values are supported.",
                    );
                    continue;
                }
            };
            self.module_class_site(container.span, template, false);
        }
        walk::walk_jsx_opening_element(self, element);
    }
}

impl UsageCollector<'_> {
    /// Synthesize the site data for a direct `styles.name` reference, or
    /// `None` when the member is not this module's or has no candidates.
    fn member_site(&self, member: &StaticMemberExpression<'_>) -> Option<StaticTemplate> {
        let member_name = self.module_member_name(member).map(str::to_string)?;
        let key = SelectorKey::Class(member_name.clone());
        let candidates = self.candidates.get(&key)?;
        let preserved = self.preserved_module_classes.contains(&member_name);
        Some(StaticTemplate {
            value: candidates.join(" "),
            members: vec![member_name],
            static_classes: Vec::new(),
            partial_edits: Vec::new(),
            preserved_candidates: if preserved {
                candidates.clone()
            } else {
                Vec::new()
            },
            preserved_expression: preserved.then(|| {
                self.source[member.span.start as usize..member.span.end as usize].to_string()
            }),
            append_at: None,
        })
    }

    /// Plan one supported result leaf of a module-path className expression.
    /// String and no-op leaves are the global plan's concern; members of
    /// other bindings stay opaque, matching the static member handling.
    fn module_result_leaf(&mut self, leaf: &Expression<'_>) {
        match leaf {
            Expression::StaticMemberExpression(member) => {
                if let Some(template) = self.member_site(member) {
                    self.module_class_site(member.span, template, true);
                }
            }
            Expression::TemplateLiteral(template) => {
                let Some(result) = self.static_template(template) else {
                    self.warn_dynamic_class_name(
                        template.span,
                        "The template contains a dynamic or unsupported class.",
                    );
                    return;
                };
                if result.members.is_empty() {
                    return;
                }
                self.module_class_site(template.span, result, true);
            }
            Expression::StringLiteral(_) => {}
            leaf if self.is_noop_class_leaf(leaf) => {}
            leaf => {
                self.warn_dynamic_class_name(
                    leaf.span(),
                    "Only static className values are supported.",
                );
            }
        }
    }

    /// Plan candidates, conflicts, edits, and accounting for one module class
    /// site: the whole className attribute for the static forms
    /// (`leaf: false`), or a single result leaf of a supported dynamic
    /// expression (`leaf: true`).
    fn module_class_site(&mut self, span: Span, template: StaticTemplate, leaf: bool) {
        let site = (span.start as usize, span.end as usize);
        let StaticTemplate {
            value: replacement_value,
            members,
            static_classes,
            mut partial_edits,
            preserved_candidates,
            preserved_expression,
            append_at,
        } = template;
        for member in &members {
            let key = SelectorKey::Class(member.clone());
            for candidate in &self.candidates[&key] {
                self.matches.push(CandidateMatch {
                    start: site.0,
                    end: site.1,
                    key: key.clone(),
                    candidate: candidate.clone(),
                    origin_candidate: candidate.clone(),
                });
            }
        }
        if let Some((left, right)) = self.conflicting_member_utilities(&members) {
            self.warnings.push(Warning::new(
                "module-utilities-conflict",
                self.file_path.to_string(),
                site,
                format!(
                    "Generated utilities `{left}` and `{right}` overlap; the CSS Module source order would be lost."
                ),
            ));
            return;
        }
        if let Some((generated, existing)) = self.conflicting_utilities(&members, &static_classes) {
            self.warnings.push(Warning::existing_tailwind_conflict(
                self.file_path,
                site,
                &generated,
                &existing,
            ));
        }
        // A previous --write run may already have appended the preserved
        // candidates as a static segment; appending them again would grow
        // the template on every run.
        let preserved_candidates: Vec<String> = preserved_candidates
            .into_iter()
            .filter(|candidate| !static_classes.contains(candidate))
            .collect();
        if let Some(expression) = preserved_expression {
            if !preserved_candidates.is_empty() {
                let appended = format!(" {}", preserved_candidates.join(" "));
                let template_body = format!(
                    "`${{{expression}}}${{{}}}`",
                    serde_json::to_string(&appended).expect("string serialization")
                );
                self.edits.push(Edit {
                    start: site.0,
                    end: site.1,
                    replacement: if leaf {
                        template_body
                    } else {
                        format!("{{{template_body}}}")
                    },
                });
            }
        } else if let Some(append_at) = append_at {
            if !preserved_candidates.is_empty() {
                let appended = format!(" {}", preserved_candidates.join(" "));
                partial_edits.push(Edit {
                    start: append_at,
                    end: append_at,
                    replacement: format!(
                        "${{{}}}",
                        serde_json::to_string(&appended).expect("string serialization")
                    ),
                });
            }
            self.edits.extend(partial_edits);
        } else if partial_edits.is_empty() {
            self.edits.push(Edit {
                start: site.0,
                end: site.1,
                replacement: if leaf {
                    js_string_literal(&replacement_value, b'"')
                } else {
                    jsx_attribute_value(&replacement_value)
                },
            });
        } else {
            self.edits.extend(partial_edits);
        }
        for member in members {
            let key = SelectorKey::Class(member.clone());
            for candidate in &self.candidates[&key] {
                self.emitted_candidates.insert(candidate.clone());
            }
            *self.matched_module_refs.entry(member).or_default() += 1;
        }
    }
}

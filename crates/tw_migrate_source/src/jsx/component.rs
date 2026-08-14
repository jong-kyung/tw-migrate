use oxc_ast::ast::{
    Argument, ArrowFunctionExpression, BindingPattern, CallExpression, Expression,
    FormalParameters, Function, FunctionBody, IdentifierReference, ImportExpression,
    JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement, JSXElementName,
    JSXExpression, PropertyKey, ReturnStatement, Statement, StaticMemberExpression,
};
use oxc_ast_visit::{Visit, walk};
use oxc_syntax::{scope::ScopeFlags, symbol::SymbolId};
use tw_migrate_css::SelectorKey;

use super::collect::{
    FileOut, FileSymbols, FnRef, ident_is, mark_namespace_member, member_on, span2, symbol_of,
};
use super::{Comp, Forward, Node, NodeKind, R_BOUNDARY, R_CONDITIONAL, R_HOC, R_PORTAL, TagRef};
use crate::resolve_import;

struct ParamInfo {
    props: Option<SymbolId>,
    class_name: Option<SymbolId>,
    children: Option<SymbolId>,
    bad: bool,
}

fn analyze_params(params: &FormalParameters<'_>) -> ParamInfo {
    let mut info = ParamInfo {
        props: None,
        class_name: None,
        children: None,
        bad: params.rest.is_some(),
    };
    if params.items.len() > 1 {
        info.bad = true;
        return info;
    }
    let Some(param) = params.items.first() else {
        return info;
    };
    match &param.pattern {
        BindingPattern::BindingIdentifier(id) => info.props = id.symbol_id.get(),
        BindingPattern::ObjectPattern(pattern) => {
            if pattern.rest.is_some() {
                // `{...rest}` can smuggle className/children invisibly.
                info.bad = true;
            }
            for property in &pattern.properties {
                let PropertyKey::StaticIdentifier(key) = &property.key else {
                    continue;
                };
                let binding = match &property.value {
                    BindingPattern::BindingIdentifier(id) => Some(id),
                    BindingPattern::AssignmentPattern(assignment) => match &assignment.left {
                        BindingPattern::BindingIdentifier(id) => Some(id),
                        _ => None,
                    },
                    _ => None,
                };
                match key.name.as_str() {
                    "className" => match binding {
                        Some(id) => info.class_name = id.symbol_id.get(),
                        None => info.bad = true,
                    },
                    "children" => match binding {
                        Some(id) => info.children = id.symbol_id.get(),
                        None => info.bad = true,
                    },
                    _ => {}
                }
            }
        }
        _ => info.bad = true,
    }
    info
}

fn block_body<'a>(fnref: FnRef<'a>) -> Option<&'a FunctionBody<'a>> {
    match fnref {
        FnRef::Function(func) => func.body.as_deref(),
        FnRef::Arrow(arrow) => arrow.get_function_body(),
    }
}

/// Extract the single statically analyzable JSX return, or the reason there
/// is none.
fn qualify<'a>(fnref: FnRef<'a>) -> Result<&'a Expression<'a>, &'static str> {
    let mut portals = PortalScan { found: false };
    match fnref {
        FnRef::Function(func) => {
            if let Some(body) = &func.body {
                portals.visit_function_body(body);
            }
        }
        FnRef::Arrow(arrow) => portals.visit_arrow_function_body(&arrow.body),
    }
    if portals.found {
        return Err(R_PORTAL);
    }
    let argument = match fnref {
        FnRef::Arrow(arrow) if arrow.is_expression() => arrow.get_expression().ok_or(R_HOC)?,
        _ => {
            let body = block_body(fnref).ok_or(R_HOC)?;
            let mut counter = ReturnCounter { count: 0 };
            counter.visit_function_body(body);
            if counter.count == 0 {
                return Err(R_HOC);
            }
            if counter.count > 1 {
                return Err(R_CONDITIONAL);
            }
            // The single return must be a direct statement of the body;
            // otherwise it sits inside a conditional branch.
            body.statements
                .iter()
                .find_map(|stmt| match stmt {
                    Statement::ReturnStatement(ret) => ret.argument.as_ref(),
                    _ => None,
                })
                .ok_or(R_CONDITIONAL)?
        }
    };
    match argument.get_inner_expression() {
        Expression::JSXElement(_) | Expression::JSXFragment(_) => Ok(argument),
        Expression::ConditionalExpression(_) | Expression::LogicalExpression(_) => {
            Err(R_CONDITIONAL)
        }
        _ => Err(R_HOC),
    }
}

struct ReturnCounter {
    count: usize,
}

impl<'a> Visit<'a> for ReturnCounter {
    fn visit_return_statement(&mut self, it: &ReturnStatement<'a>) {
        self.count += 1;
        walk::walk_return_statement(self, it);
    }

    fn visit_function(&mut self, _it: &Function<'a>, _flags: ScopeFlags) {}

    fn visit_arrow_function_expression(&mut self, _it: &ArrowFunctionExpression<'a>) {}
}

struct PortalScan {
    found: bool,
}

impl<'a> Visit<'a> for PortalScan {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        let portal = match call.callee.get_inner_expression() {
            Expression::Identifier(ident) => ident.name == "createPortal",
            Expression::StaticMemberExpression(member) => member.property.name == "createPortal",
            _ => false,
        };
        if portal {
            self.found = true;
        }
        walk::walk_call_expression(self, call);
    }
}

// ---------------------------------------------------------------------------
// Tree building and sweeping
// ---------------------------------------------------------------------------

pub(super) fn build_component(
    syms: &FileSymbols<'_>,
    out: &mut FileOut,
    comps: &mut [Comp],
    ix: usize,
    fnref: FnRef<'_>,
) {
    let params = analyze_params(match fnref {
        FnRef::Function(func) => &func.params,
        FnRef::Arrow(arrow) => &arrow.params,
    });
    match qualify(fnref) {
        Err(reason) => {
            comps[ix].body = Err(reason);
            let mut sweep = Sweep::file_level(syms, out, reason);
            match fnref {
                FnRef::Function(func) => {
                    sweep.visit_formal_parameters(&func.params);
                    if let Some(body) = &func.body {
                        sweep.visit_function_body(body);
                    }
                }
                FnRef::Arrow(arrow) => {
                    sweep.visit_formal_parameters(&arrow.params);
                    sweep.visit_arrow_function_body(&arrow.body);
                }
            }
        }
        Ok(root) => {
            let mut builder = CompBuilder {
                syms,
                out,
                props_sym: params.props,
                class_sym: params.class_name,
                children_sym: params.children,
                nodes: Vec::new(),
                slots: Vec::new(),
                forward_targets: Vec::new(),
                forward_bad: params.bad,
                children_bad: params.bad,
            };
            builder.sweep_params(
                match fnref {
                    FnRef::Function(func) => &func.params,
                    FnRef::Arrow(arrow) => &arrow.params,
                },
                R_BOUNDARY,
            );
            if let Some(body) = block_body(fnref) {
                for stmt in &body.statements {
                    if !matches!(stmt, Statement::ReturnStatement(_)) {
                        builder.sweep_stmt(stmt, R_BOUNDARY);
                    }
                }
            }
            builder.build_root(root);
            let comp = &mut comps[ix];
            comp.slots = builder.slots;
            comp.children_bad = builder.children_bad;
            comp.forward = if builder.forward_bad || builder.forward_targets.len() > 1 {
                Forward::Bad
            } else if let [target] = builder.forward_targets[..] {
                Forward::Target(target)
            } else {
                Forward::No
            };
            comp.body = Ok(builder.nodes);
        }
    }
}

struct CompBuilder<'x, 's> {
    syms: &'x FileSymbols<'s>,
    out: &'x mut FileOut,
    props_sym: Option<SymbolId>,
    class_sym: Option<SymbolId>,
    children_sym: Option<SymbolId>,
    nodes: Vec<Node>,
    slots: Vec<usize>,
    forward_targets: Vec<usize>,
    forward_bad: bool,
    children_bad: bool,
}

/// Run a sweep entry method and fold its props flags back into the builder.
macro_rules! sweep_into {
    ($builder:ident, $reason:expr, $method:ident, $($arg:expr),+) => {{
        let mut sweep = $builder.sweep($reason);
        sweep.$method($($arg),+);
        let (forward_bad, children_bad) = (sweep.forward_bad, sweep.children_bad);
        $builder.forward_bad |= forward_bad;
        $builder.children_bad |= children_bad;
    }};
}

impl<'s> CompBuilder<'_, 's> {
    fn sweep(&mut self, reason: &'static str) -> Sweep<'_, 's> {
        Sweep {
            syms: self.syms,
            out: &mut *self.out,
            reason,
            props_sym: self.props_sym,
            class_sym: self.class_sym,
            children_sym: self.children_sym,
            forward_bad: false,
            children_bad: false,
        }
    }

    fn sweep_expr(&mut self, expression: &Expression<'_>, reason: &'static str) {
        sweep_into!(self, reason, visit_expression, expression);
    }

    fn sweep_stmt(&mut self, stmt: &Statement<'_>, reason: &'static str) {
        sweep_into!(self, reason, visit_statement, stmt);
    }

    fn sweep_params(&mut self, params: &FormalParameters<'_>, reason: &'static str) {
        sweep_into!(self, reason, visit_formal_parameters, params);
    }

    fn sweep_jsx_expr(&mut self, expression: &JSXExpression<'_>, reason: &'static str) {
        if let Some(inner) = expression.as_expression() {
            self.sweep_expr(inner, reason);
        }
    }

    fn push(&mut self, parent: Option<usize>, kind: NodeKind) -> usize {
        self.nodes.push(Node { parent, kind });
        self.nodes.len() - 1
    }

    fn build_root(&mut self, root: &Expression<'_>) {
        match root.get_inner_expression() {
            Expression::JSXElement(element) => self.build_element(None, element),
            Expression::JSXFragment(fragment) => {
                for child in &fragment.children {
                    self.build_child(None, child);
                }
            }
            _ => {}
        }
    }

    fn tag_ref(&mut self, name: &JSXElementName<'_>) -> Option<TagRef> {
        match name {
            JSXElementName::Identifier(_) => None,
            JSXElementName::IdentifierReference(reference) => {
                Some(match symbol_of(reference, self.syms.scoping) {
                    Some(sym) => {
                        if let Some(&ix) = self.syms.comp_symbols.get(&sym) {
                            TagRef::Local(ix)
                        } else if let Some(local) = self.syms.import_symbols.get(&sym) {
                            TagRef::Import(local.clone())
                        } else {
                            TagRef::Unknown
                        }
                    }
                    None => TagRef::Unknown,
                })
            }
            JSXElementName::MemberExpression(member) => {
                mark_namespace_member(self.syms, self.out, member);
                Some(TagRef::Unknown)
            }
            _ => Some(TagRef::Unknown),
        }
    }

    fn build_element(&mut self, parent: Option<usize>, element: &JSXElement<'_>) {
        match self.tag_ref(&element.opening_element.name) {
            None => {
                let mut keys = Vec::new();
                let mut forward = false;
                let mut tainted = false;
                for item in &element.opening_element.attributes {
                    match item {
                        JSXAttributeItem::Attribute(attribute) => {
                            let JSXAttributeName::Identifier(name) = &attribute.name else {
                                continue;
                            };
                            match name.name.as_str() {
                                "className" => self.element_class_value(
                                    &attribute.value,
                                    &mut keys,
                                    &mut forward,
                                ),
                                "id" => self.element_id_value(&attribute.value, &mut keys),
                                _ => self.sweep_attribute_value(&attribute.value),
                            }
                        }
                        JSXAttributeItem::SpreadAttribute(spread) => {
                            // Spread props may override className at runtime:
                            // usages stay recorded, but the element can no
                            // longer witness an ancestor.
                            tainted = true;
                            self.sweep_expr(&spread.argument, R_BOUNDARY);
                        }
                    }
                }
                let ix = self.push(parent, NodeKind::Element { keys, tainted });
                if forward {
                    self.forward_targets.push(ix);
                }
                for child in &element.children {
                    self.build_child(Some(ix), child);
                }
            }
            Some(tag) => {
                let mut class_keys = Vec::new();
                for item in &element.opening_element.attributes {
                    match item {
                        JSXAttributeItem::Attribute(attribute) => {
                            let JSXAttributeName::Identifier(name) = &attribute.name else {
                                continue;
                            };
                            if name.name == "className" {
                                if let Some(JSXAttributeValue::ExpressionContainer(container)) =
                                    &attribute.value
                                    && let Some(inner) = container.expression.as_expression()
                                {
                                    // Chained forwarding into another
                                    // component is not proven.
                                    let mut forwarded = false;
                                    self.class_part(inner, &mut class_keys, &mut forwarded);
                                    self.forward_bad |= forwarded;
                                }
                            } else {
                                self.sweep_attribute_value(&attribute.value);
                            }
                        }
                        JSXAttributeItem::SpreadAttribute(spread) => {
                            self.sweep_expr(&spread.argument, R_BOUNDARY);
                        }
                    }
                }
                let ix = self.push(parent, NodeKind::ComponentUse { tag, class_keys });
                for child in &element.children {
                    self.build_child(Some(ix), child);
                }
            }
        }
    }

    /// One part of a `className` value: a module key, a forwarded
    /// `props.className`, a nested template, or an opaque expression. The
    /// caller decides what forwarding means at its site: fall-through on a
    /// host element, an unproven chain on a component invocation.
    fn class_part(
        &mut self,
        expression: &Expression<'_>,
        keys: &mut Vec<(String, (usize, usize))>,
        forward: &mut bool,
    ) {
        match expression {
            Expression::StaticMemberExpression(member) => {
                if let Some(name) = member_on(self.syms.scoping, self.syms.css_symbol, member) {
                    keys.push((name.to_string(), span2(member.span)));
                } else if member_on(self.syms.scoping, self.props_sym, member) == Some("className")
                {
                    *forward = true;
                } else {
                    self.sweep_expr(expression, R_BOUNDARY);
                }
            }
            Expression::Identifier(ident) if ident_is(self.syms.scoping, self.class_sym, ident) => {
                *forward = true;
            }
            Expression::TemplateLiteral(template) => {
                for part in &template.expressions {
                    self.class_part(part, keys, forward);
                }
            }
            Expression::StringLiteral(_) => {}
            _ => self.sweep_expr(expression, R_BOUNDARY),
        }
    }

    fn element_class_value(
        &mut self,
        value: &Option<JSXAttributeValue<'_>>,
        keys: &mut Vec<(SelectorKey, (usize, usize))>,
        forward: &mut bool,
    ) {
        let Some(JSXAttributeValue::ExpressionContainer(container)) = value else {
            return;
        };
        match &container.expression {
            JSXExpression::EmptyExpression(_) => {}
            expression => {
                if let Some(inner) = expression.as_expression() {
                    let mut parts = Vec::new();
                    let mut forward_here = false;
                    self.class_part(inner, &mut parts, &mut forward_here);
                    keys.extend(
                        parts
                            .into_iter()
                            .map(|(name, span)| (SelectorKey::Class(name), span)),
                    );
                    *forward |= forward_here;
                }
            }
        }
    }

    fn element_id_value(
        &mut self,
        value: &Option<JSXAttributeValue<'_>>,
        keys: &mut Vec<(SelectorKey, (usize, usize))>,
    ) {
        let Some(JSXAttributeValue::ExpressionContainer(container)) = value else {
            return;
        };
        match &container.expression {
            JSXExpression::StaticMemberExpression(member) => {
                if let Some(name) = member_on(self.syms.scoping, self.syms.css_symbol, member) {
                    keys.push((SelectorKey::Id(name.to_string()), span2(member.span)));
                } else {
                    self.sweep_jsx_expr(&container.expression, R_BOUNDARY);
                }
            }
            JSXExpression::EmptyExpression(_) => {}
            other => self.sweep_jsx_expr(other, R_BOUNDARY),
        }
    }

    fn sweep_attribute_value(&mut self, value: &Option<JSXAttributeValue<'_>>) {
        match value {
            Some(JSXAttributeValue::ExpressionContainer(container)) => {
                self.sweep_jsx_expr(&container.expression, R_BOUNDARY);
            }
            Some(JSXAttributeValue::Element(element)) => {
                sweep_into!(self, R_BOUNDARY, visit_jsx_element, element);
            }
            Some(JSXAttributeValue::Fragment(fragment)) => {
                sweep_into!(self, R_BOUNDARY, visit_jsx_fragment, fragment);
            }
            _ => {}
        }
    }

    fn build_child(&mut self, parent: Option<usize>, child: &JSXChild<'_>) {
        match child {
            JSXChild::Text(_) => {}
            JSXChild::Element(element) => self.build_element(parent, element),
            JSXChild::Fragment(fragment) => {
                for child in &fragment.children {
                    self.build_child(parent, child);
                }
            }
            JSXChild::ExpressionContainer(container) => {
                self.build_expression_child(parent, &container.expression);
            }
            JSXChild::Spread(spread) => self.sweep_expr(&spread.expression, R_BOUNDARY),
        }
    }

    fn build_expression_child(&mut self, parent: Option<usize>, expression: &JSXExpression<'_>) {
        match expression {
            JSXExpression::EmptyExpression(_) => {}
            JSXExpression::Identifier(ident)
                if ident_is(self.syms.scoping, self.children_sym, ident) =>
            {
                let ix = self.push(parent, NodeKind::Slot);
                self.slots.push(ix);
            }
            JSXExpression::StaticMemberExpression(member)
                if member_on(self.syms.scoping, self.props_sym, member) == Some("children") =>
            {
                let ix = self.push(parent, NodeKind::Slot);
                self.slots.push(ix);
            }
            JSXExpression::CallExpression(call) => {
                if !self.try_map_call(parent, call) {
                    sweep_into!(self, R_BOUNDARY, visit_call_expression, call);
                }
            }
            other => self.sweep_jsx_expr(other, R_BOUNDARY),
        }
    }

    /// `{expr.map(cb)}` whose callback statically returns a single JSX
    /// expression is part of the tree (a repeated static subtree).
    fn try_map_call(&mut self, parent: Option<usize>, call: &CallExpression<'_>) -> bool {
        let Expression::StaticMemberExpression(callee) = &call.callee else {
            return false;
        };
        if callee.property.name != "map" {
            return false;
        }
        let Some(first) = call.arguments.first() else {
            return false;
        };
        let callback = match first {
            Argument::ArrowFunctionExpression(arrow) => FnRef::Arrow(arrow),
            Argument::FunctionExpression(func) => FnRef::Function(func),
            _ => return false,
        };
        let Ok(root) = qualify(callback) else {
            return false;
        };
        self.sweep_expr(&callee.object, R_BOUNDARY);
        for argument in call.arguments.iter().skip(1) {
            if let Some(expr) = argument.as_expression() {
                self.sweep_expr(expr, R_BOUNDARY);
            }
        }
        self.sweep_params(
            match callback {
                FnRef::Function(func) => &func.params,
                FnRef::Arrow(arrow) => &arrow.params,
            },
            R_BOUNDARY,
        );
        if let Some(body) = block_body(callback) {
            for stmt in &body.statements {
                if !matches!(stmt, Statement::ReturnStatement(_)) {
                    self.sweep_stmt(stmt, R_BOUNDARY);
                }
            }
        }
        match root.get_inner_expression() {
            Expression::JSXElement(element) => self.build_element(parent, element),
            Expression::JSXFragment(fragment) => {
                for child in &fragment.children {
                    self.build_child(parent, child);
                }
            }
            _ => return false,
        }
        true
    }
}

/// Visitor for regions the proof cannot follow. Records CSS Module usages
/// (pre-disqualified), components rendered or escaping there, and props
/// references that break forwarding/children contracts.
pub(super) struct Sweep<'x, 's> {
    syms: &'x FileSymbols<'s>,
    out: &'x mut FileOut,
    reason: &'static str,
    props_sym: Option<SymbolId>,
    class_sym: Option<SymbolId>,
    children_sym: Option<SymbolId>,
    forward_bad: bool,
    children_bad: bool,
}

impl<'x, 's> Sweep<'x, 's> {
    pub(super) fn file_level(
        syms: &'x FileSymbols<'s>,
        out: &'x mut FileOut,
        reason: &'static str,
    ) -> Self {
        Sweep {
            syms,
            out,
            reason,
            props_sym: None,
            class_sym: None,
            children_sym: None,
            forward_bad: false,
            children_bad: false,
        }
    }
}

impl<'a> Visit<'a> for Sweep<'_, '_> {
    fn visit_static_member_expression(&mut self, member: &StaticMemberExpression<'a>) {
        if let Some(name) = member_on(self.syms.scoping, self.syms.css_symbol, member) {
            self.out
                .boundary_usages
                .push((name.to_string(), span2(member.span), self.reason));
            return;
        }
        if let Some(property) = member_on(self.syms.scoping, self.props_sym, member) {
            match property {
                "className" => self.forward_bad = true,
                "children" => self.children_bad = true,
                _ => {}
            }
            return;
        }
        walk::walk_static_member_expression(self, member);
    }

    fn visit_identifier_reference(&mut self, reference: &IdentifierReference<'a>) {
        let Some(sym) = symbol_of(reference, self.syms.scoping) else {
            return;
        };
        if Some(sym) == self.syms.css_symbol {
            // The module binding itself escapes; usages become untrackable.
            self.out.unsound = true;
        } else if Some(sym) == self.props_sym {
            self.forward_bad = true;
            self.children_bad = true;
        } else if Some(sym) == self.class_sym {
            self.forward_bad = true;
        } else if Some(sym) == self.children_sym {
            self.children_bad = true;
        } else if let Some(&ix) = self.syms.comp_symbols.get(&sym) {
            // The component binding escapes as a value; its render sites can
            // no longer be enumerated.
            self.out.rendered_marks.push((TagRef::Local(ix), R_HOC));
        } else if let Some(local) = self.syms.import_symbols.get(&sym) {
            self.out
                .rendered_marks
                .push((TagRef::Import(local.clone()), R_HOC));
        }
    }

    fn visit_jsx_element_name(&mut self, name: &JSXElementName<'a>) {
        match name {
            JSXElementName::IdentifierReference(reference) => {
                if let Some(sym) = symbol_of(reference, self.syms.scoping) {
                    if let Some(&ix) = self.syms.comp_symbols.get(&sym) {
                        self.out
                            .rendered_marks
                            .push((TagRef::Local(ix), self.reason));
                    } else if let Some(local) = self.syms.import_symbols.get(&sym) {
                        self.out
                            .rendered_marks
                            .push((TagRef::Import(local.clone()), self.reason));
                    }
                }
            }
            JSXElementName::MemberExpression(member) => {
                mark_namespace_member(self.syms, self.out, member);
            }
            _ => {}
        }
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Expression::Identifier(callee) = &call.callee
            && callee.name == "require"
            && let Some(Argument::StringLiteral(source)) = call.arguments.first()
            && resolve_import(self.syms.file_path, source.value.as_str()) == self.syms.css_target
        {
            self.out.unsound = true;
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_import_expression(&mut self, import: &ImportExpression<'a>) {
        if let Expression::StringLiteral(source) = &import.source
            && resolve_import(self.syms.file_path, source.value.as_str()) == self.syms.css_target
        {
            self.out.unsound = true;
        }
        walk::walk_import_expression(self, import);
    }
}

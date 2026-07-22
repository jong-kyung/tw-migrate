//! JS/JSX-side rewriting: locate CSS Module references and plan span edits.

use std::{
    collections::{BTreeSet, HashMap},
    path::{Component, Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, CallExpression, ComputedMemberExpression, ExportAllDeclaration,
    ExportNamedDeclaration, Expression, ImportDeclaration, ImportDeclarationSpecifier,
    ImportExpression, JSXAttribute, JSXAttributeItem, JSXAttributeName, JSXAttributeValue,
    JSXExpression, JSXOpeningElement, StaticMemberExpression, TemplateLiteral, VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_span::{SourceType, Span};
use oxc_syntax::symbol::SymbolId;

use crate::{
    css_plan::SelectorKey,
    planner::{Edit, SourceFile, Warning},
    utilities::tailwind_utilities_conflict,
};

pub(crate) struct CandidateMatch {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) key: SelectorKey,
    pub(crate) candidate: String,
    pub(crate) origin_candidate: String,
}

pub(crate) struct SourcePlan {
    pub(crate) edits: Vec<Edit>,
    pub(crate) removable_import_edits: Vec<Edit>,
    pub(crate) candidates: Vec<String>,
    pub(crate) matches: Vec<CandidateMatch>,
    pub(crate) module_refs: HashMap<String, usize>,
    pub(crate) matched_module_refs: HashMap<String, usize>,
    pub(crate) module_references_safe: bool,
    pub(crate) warnings: Vec<Warning>,
}

pub(crate) fn source_type_for_path(path: &str) -> Result<SourceType, String> {
    let source_type = SourceType::from_path(Path::new(path)).map_err(|error| error.to_string())?;
    Ok(
        if Path::new(path)
            .extension()
            .is_some_and(|extension| extension == "js")
        {
            source_type.with_jsx(true)
        } else {
            source_type
        },
    )
}

/// A scan-only file that cannot be parsed is data, not an error: when its
/// text names this stylesheet it becomes an unverifiable reference that
/// conservatively retains the module; otherwise it has no effect. Writable
/// files still fail loudly -- migration targets must be analyzable.
fn opaque_reference_plan(file: &SourceFile, css_path: &str, is_module: bool) -> SourcePlan {
    let referenced = is_module
        && Path::new(css_path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| file.source.contains(name));
    SourcePlan {
        edits: Vec::new(),
        removable_import_edits: Vec::new(),
        candidates: Vec::new(),
        matches: Vec::new(),
        module_refs: HashMap::new(),
        matched_module_refs: HashMap::new(),
        module_references_safe: !referenced,
        warnings: if referenced {
            vec![Warning {
                code: "unsupported-css-module-reference",
                file: file.path.clone(),
                start: 0,
                end: 0,
                message: "The file could not be parsed, so its possible reference retains the CSS Module."
                    .to_string(),
            }]
        } else {
            Vec::new()
        },
    }
}

pub(crate) fn plan_source_file(
    file: &SourceFile,
    css_path: &str,
    is_module: bool,
    candidates: &HashMap<SelectorKey, Vec<String>>,
) -> Result<SourcePlan, String> {
    plan_source_file_with_mode(
        file,
        css_path,
        is_module,
        candidates,
        &BTreeSet::new(),
        false,
    )
}

pub(crate) fn plan_batch_source_file(
    file: &SourceFile,
    css_path: &str,
    is_module: bool,
    candidates: &HashMap<SelectorKey, Vec<String>>,
    preserved_module_classes: &BTreeSet<String>,
) -> Result<SourcePlan, String> {
    plan_source_file_with_mode(
        file,
        css_path,
        is_module,
        candidates,
        preserved_module_classes,
        true,
    )
}

fn plan_source_file_with_mode(
    file: &SourceFile,
    css_path: &str,
    is_module: bool,
    candidates: &HashMap<SelectorKey, Vec<String>>,
    preserved_module_classes: &BTreeSet<String>,
    batch_mode: bool,
) -> Result<SourcePlan, String> {
    let allocator = Allocator::default();
    let source_type = source_type_for_path(&file.path)
        .map_err(|error| format!("Unsupported source file {}: {error}", file.path))?;
    let parsed = Parser::new(&allocator, &file.source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        if !file.writable {
            return Ok(opaque_reference_plan(file, css_path, is_module));
        }
        return Err(format!(
            "Failed to parse {}: {:?}",
            file.path, parsed.diagnostics
        ));
    }
    let semantic = SemanticBuilder::new_compiler().build(&parsed.program);
    if !semantic.diagnostics.is_empty() {
        if !file.writable {
            return Ok(opaque_reference_plan(file, css_path, is_module));
        }
        return Err(format!(
            "Failed to analyze {}: {:?}",
            file.path, semantic.diagnostics
        ));
    }

    let mut imports = ImportCollector {
        file_path: &file.path,
        css_path,
        bindings: Vec::new(),
        unsupported_shape: false,
        warning_span: None,
    };
    if is_module {
        imports.visit_program(&parsed.program);
    }
    // On the global path, members of CSS Module imports can never match a
    // global class: they are module references handled by the module's own
    // plan, not dynamic class names.
    let mut global_module_symbols = Vec::new();
    if !is_module {
        for statement in &parsed.program.body {
            let oxc_ast::ast::Statement::ImportDeclaration(declaration) = statement else {
                continue;
            };
            if !declaration.source.value.ends_with(".module.css") {
                continue;
            }
            for specifier in declaration.specifiers.iter().flatten() {
                if let ImportDeclarationSpecifier::ImportDefaultSpecifier(specifier) = specifier
                    && let Some(symbol) = specifier.local.symbol_id.get()
                {
                    global_module_symbols.push(symbol);
                }
            }
        }
    }

    let scoping = semantic.semantic.scoping();
    let total_import_refs = imports
        .bindings
        .iter()
        .map(|binding| scoping.get_resolved_reference_ids(binding.symbol).len())
        .sum::<usize>();
    let mut collector = UsageCollector {
        source: &file.source,
        file_path: &file.path,
        is_module,
        scoping,
        import_bindings: &imports.bindings,
        global_module_symbols: &global_module_symbols,
        candidates,
        preserved_module_classes,
        batch_mode,
        edits: Vec::new(),
        emitted_candidates: BTreeSet::new(),
        matches: Vec::new(),
        module_refs: HashMap::new(),
        matched_module_refs: HashMap::new(),
        class_name_depth: 0,
        alias_spans: HashMap::new(),
        computed_refs: 0,
        unsafe_reference: false,
        warnings: Vec::new(),
    };
    collector.visit_program(&parsed.program);

    let classified_import_refs =
        collector.module_refs.values().sum::<usize>() + collector.computed_refs;
    let counts_match = total_import_refs == classified_import_refs;
    let module_references_safe =
        !imports.unsupported_shape && counts_match && !collector.unsafe_reference;
    // Computed, aliased, and non-className sites already carry their own
    // per-site warnings; the import-site warning covers the remaining
    // import-shape and unclassified-identifier cases.
    if (imports.unsupported_shape || !counts_match)
        && let Some(span) = imports.warning_span
    {
        collector.warnings.push(Warning {
            code: "unsupported-css-module-reference",
            file: file.path.clone(),
            start: span.start as usize,
            end: span.end as usize,
            message: "The CSS Module has an import or reference that cannot be migrated safely."
                .to_string(),
        });
    }

    let removable_import_edits = if is_module
        && module_references_safe
        && !imports.bindings.is_empty()
        && classified_import_refs == collector.matched_module_refs.values().sum::<usize>()
    {
        imports
            .bindings
            .iter()
            .map(|binding| Edit {
                start: binding.span.start as usize,
                end: consume_following_newline(&file.source, binding.span.end as usize),
                replacement: String::new(),
            })
            .collect()
    } else {
        Vec::new()
    };

    Ok(SourcePlan {
        edits: collector.edits,
        removable_import_edits,
        candidates: collector.emitted_candidates.into_iter().collect(),
        matches: collector.matches,
        module_refs: collector.module_refs,
        matched_module_refs: collector.matched_module_refs,
        module_references_safe,
        warnings: collector.warnings,
    })
}

struct ImportBinding {
    symbol: SymbolId,
    span: Span,
}

struct ImportCollector<'s> {
    file_path: &'s str,
    css_path: &'s str,
    bindings: Vec<ImportBinding>,
    unsupported_shape: bool,
    warning_span: Option<Span>,
}

impl<'a> Visit<'a> for ImportCollector<'_> {
    fn visit_import_declaration(&mut self, declaration: &ImportDeclaration<'a>) {
        let resolved = resolve_import(self.file_path, declaration.source.value.as_str());
        if resolved == normalize_path(Path::new(self.css_path)) {
            self.warning_span.get_or_insert(declaration.span);
            let Some(specifiers) = &declaration.specifiers else {
                self.unsupported_shape = true;
                walk::walk_import_declaration(self, declaration);
                return;
            };
            if specifiers.len() != 1 {
                self.unsupported_shape = true;
            }
            for specifier in specifiers {
                if let ImportDeclarationSpecifier::ImportDefaultSpecifier(specifier) = specifier
                    && let Some(symbol) = specifier.local.symbol_id.get()
                {
                    self.bindings.push(ImportBinding {
                        symbol,
                        span: declaration.span,
                    });
                } else {
                    self.unsupported_shape = true;
                }
            }
        }
        walk::walk_import_declaration(self, declaration);
    }

    fn visit_export_named_declaration(&mut self, declaration: &ExportNamedDeclaration<'a>) {
        if declaration.source.as_ref().is_some_and(|source| {
            resolve_import(self.file_path, source.value.as_str())
                == normalize_path(Path::new(self.css_path))
        }) {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(declaration.span);
        }
        walk::walk_export_named_declaration(self, declaration);
    }

    fn visit_export_all_declaration(&mut self, declaration: &ExportAllDeclaration<'a>) {
        if resolve_import(self.file_path, declaration.source.value.as_str())
            == normalize_path(Path::new(self.css_path))
        {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(declaration.span);
        }
        walk::walk_export_all_declaration(self, declaration);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Expression::Identifier(callee) = &call.callee
            && callee.name == "require"
            && let Some(Argument::StringLiteral(source)) = call.arguments.first()
            && resolve_import(self.file_path, source.value.as_str())
                == normalize_path(Path::new(self.css_path))
        {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(call.span);
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_import_expression(&mut self, import: &ImportExpression<'a>) {
        if let Expression::StringLiteral(source) = &import.source
            && resolve_import(self.file_path, source.value.as_str())
                == normalize_path(Path::new(self.css_path))
        {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(import.span);
        }
        walk::walk_import_expression(self, import);
    }
}

struct StaticTemplate {
    value: String,
    members: Vec<String>,
    static_classes: Vec<String>,
    partial_edits: Vec<Edit>,
    preserved_candidates: Vec<String>,
    preserved_expression: Option<String>,
    append_at: Option<usize>,
}

struct UsageCollector<'s> {
    source: &'s str,
    file_path: &'s str,
    is_module: bool,
    scoping: &'s Scoping,
    import_bindings: &'s [ImportBinding],
    global_module_symbols: &'s [SymbolId],
    candidates: &'s HashMap<SelectorKey, Vec<String>>,
    preserved_module_classes: &'s BTreeSet<String>,
    batch_mode: bool,
    edits: Vec<Edit>,
    emitted_candidates: BTreeSet<String>,
    matches: Vec<CandidateMatch>,
    module_refs: HashMap<String, usize>,
    matched_module_refs: HashMap<String, usize>,
    class_name_depth: u32,
    alias_spans: HashMap<u32, Span>,
    computed_refs: usize,
    unsafe_reference: bool,
    warnings: Vec<Warning>,
}

impl UsageCollector<'_> {
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
                        if self.batch_mode {
                            partial = true;
                            original.push('\0');
                            continue;
                        }
                        return None;
                    };
                    let Some(candidates) = self.candidates.get(&SelectorKey::Class(name.clone()))
                    else {
                        if self.batch_mode {
                            return Some(StaticTemplate {
                                value: String::new(),
                                members: Vec::new(),
                                static_classes: Vec::new(),
                                partial_edits: Vec::new(),
                                preserved_candidates: Vec::new(),
                                preserved_expression: None,
                                append_at: None,
                            });
                        }
                        return None;
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
                Expression::StringLiteral(literal) if self.batch_mode => {
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
        for member in members {
            let candidates = self.candidates.get(&SelectorKey::Class(member.clone()))?;
            for candidate in candidates {
                if let Some(existing) = static_classes
                    .iter()
                    .find(|existing| tailwind_utilities_conflict(candidate, existing))
                {
                    return Some((candidate.clone(), existing.clone()));
                }
            }
        }
        None
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
                        tailwind_utilities_conflict(left_candidate, right_candidate)
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
                                if !self.is_global_module_member(expression) {
                                    self.warnings.push(Warning {
                                        code: "dynamic-class-name",
                                        file: self.file_path.to_string(),
                                        start: container.span.start as usize,
                                        end: container.span.end as usize,
                                        message: "Only static className values are supported."
                                            .to_string(),
                                    });
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
                self.emitted_candidates.insert(candidate.clone());
                self.matches.push(CandidateMatch {
                    start: insertion,
                    end: insertion,
                    key: key.clone(),
                    candidate: candidate.clone(),
                    origin_candidate: candidate.clone(),
                });
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
        let mut classes = value
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let original_classes = classes.clone();
        for class in original_classes {
            let key = SelectorKey::Class(class);
            if let Some(candidates) = self.candidates.get(&key) {
                for candidate in candidates {
                    self.emitted_candidates.insert(candidate.clone());
                    self.matches.push(CandidateMatch {
                        start: span.start as usize,
                        end: span.end as usize,
                        key: key.clone(),
                        candidate: candidate.clone(),
                        origin_candidate: candidate.clone(),
                    });
                    if !classes.contains(candidate) {
                        classes.push(candidate.clone());
                    }
                }
            }
        }
        for (key, candidate) in extra_candidates {
            self.emitted_candidates.insert(candidate.clone());
            self.matches.push(CandidateMatch {
                start: span.start as usize,
                end: span.end as usize,
                key: key.clone(),
                candidate: candidate.clone(),
                origin_candidate: candidate.clone(),
            });
            if !classes.contains(candidate) {
                classes.push(candidate.clone());
            }
        }
        let replacement_value = classes.join(" ");
        if replacement_value == value {
            return;
        }
        self.edits.push(Edit {
            start: span.start as usize,
            end: span.end as usize,
            replacement: jsx_attribute_value(&replacement_value),
        });
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
                    self.warnings.push(Warning {
                        code: "aliased-css-module-reference",
                        file: self.file_path.to_string(),
                        start: declarator.start as usize,
                        end: declarator.end as usize,
                        message:
                            "A CSS Module class is aliased to a binding, so the module is retained."
                                .to_string(),
                    });
                } else {
                    self.warnings.push(Warning {
                        code: "non-classname-css-module-reference",
                        file: self.file_path.to_string(),
                        start: member.span.start as usize,
                        end: member.span.end as usize,
                        message: "A CSS Module class is used outside a supported className, so the module is retained."
                            .to_string(),
                    });
                }
            }
        }
        walk::walk_static_member_expression(self, member);
    }

    fn visit_computed_member_expression(&mut self, member: &ComputedMemberExpression<'a>) {
        if self.is_module_object(&member.object) {
            self.computed_refs += 1;
            self.unsafe_reference = true;
            self.warnings.push(Warning {
                code: "computed-css-module-reference",
                file: self.file_path.to_string(),
                start: member.span.start as usize,
                end: member.span.end as usize,
                message:
                    "A computed CSS Module access cannot be verified, so the module is retained."
                        .to_string(),
            });
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
            let template = match &container.expression {
                JSXExpression::StaticMemberExpression(member) => {
                    let Some(member_name) = self.module_member_name(member).map(str::to_string)
                    else {
                        continue;
                    };
                    let key = SelectorKey::Class(member_name.clone());
                    let Some(candidates) = self.candidates.get(&key) else {
                        continue;
                    };
                    let preserved = self.preserved_module_classes.contains(&member_name);
                    StaticTemplate {
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
                            self.source[member.span.start as usize..member.span.end as usize]
                                .to_string()
                        }),
                        append_at: None,
                    }
                }
                JSXExpression::TemplateLiteral(template) => {
                    let Some(result) = self.static_template(template) else {
                        self.warnings.push(Warning {
                            code: "dynamic-class-name",
                            file: self.file_path.to_string(),
                            start: container.span.start as usize,
                            end: container.span.end as usize,
                            message: "The template contains a dynamic or unsupported class."
                                .to_string(),
                        });
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
                    self.warnings.push(Warning {
                        code: "dynamic-class-name",
                        file: self.file_path.to_string(),
                        start: container.span.start as usize,
                        end: container.span.end as usize,
                        message: "Only static className values are supported.".to_string(),
                    });
                    continue;
                }
            };
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
                        start: container.span.start as usize,
                        end: container.span.end as usize,
                        key: key.clone(),
                        candidate: candidate.clone(),
                        origin_candidate: candidate.clone(),
                    });
                }
            }
            if let Some((left, right)) = self.conflicting_member_utilities(&members) {
                self.warnings.push(Warning {
                    code: "module-utilities-conflict",
                    file: self.file_path.to_string(),
                    start: container.span.start as usize,
                    end: container.span.end as usize,
                    message: format!(
                        "Generated utilities `{left}` and `{right}` overlap; the CSS Module source order would be lost."
                    ),
                });
                continue;
            }
            if let Some((generated, existing)) =
                self.conflicting_utilities(&members, &static_classes)
            {
                self.warnings.push(Warning {
                    code: "existing-tailwind-conflict",
                    file: self.file_path.to_string(),
                    start: container.span.start as usize,
                    end: container.span.end as usize,
                    message: format!(
                        "Generated utility `{generated}` may conflict with existing `{existing}`."
                    ),
                });
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
                    self.edits.push(Edit {
                        start: container.span.start as usize,
                        end: container.span.end as usize,
                        replacement: format!(
                            "{{`${{{expression}}}${{{}}}`}}",
                            serde_json::to_string(&appended).expect("string serialization")
                        ),
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
                    start: container.span.start as usize,
                    end: container.span.end as usize,
                    replacement: jsx_attribute_value(&replacement_value),
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
        walk::walk_jsx_opening_element(self, element);
    }
}

pub(crate) fn resolve_import(file_path: &str, import: &str) -> PathBuf {
    let parent = Path::new(file_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    normalize_path(&parent.join(import))
}

pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn consume_following_newline(source: &str, end: usize) -> usize {
    if source[end..].starts_with("\r\n") {
        end + 2
    } else if source[end..].starts_with('\n') {
        end + 1
    } else {
        end
    }
}

pub(crate) fn validate_js(path: &str, source: &str) -> Result<(), String> {
    let allocator = Allocator::default();
    let source_type = source_type_for_path(path)
        .map_err(|error| format!("Unsupported source file {path}: {error}"))?;
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.diagnostics.is_empty() {
        Ok(())
    } else {
        Err(format!("Edited source no longer parses: {path}"))
    }
}

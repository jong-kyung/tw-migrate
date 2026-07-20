//! JS/JSX-side rewriting: locate CSS Module references and plan span edits.

use std::{
    collections::{BTreeSet, HashMap},
    path::{Component, Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, CallExpression, ExportAllDeclaration, ExportNamedDeclaration, Expression,
    ImportDeclaration, ImportDeclarationSpecifier, ImportExpression, JSXAttributeItem,
    JSXAttributeName, JSXAttributeValue, JSXExpression, JSXOpeningElement,
    StaticMemberExpression, TemplateLiteral,
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

pub(crate) struct SourcePlan {
    pub(crate) edits: Vec<Edit>,
    pub(crate) removable_import_edits: Vec<Edit>,
    pub(crate) candidates: Vec<String>,
    pub(crate) module_refs: HashMap<String, usize>,
    pub(crate) matched_module_refs: HashMap<String, usize>,
    pub(crate) module_references_safe: bool,
    pub(crate) warnings: Vec<Warning>,
}

fn source_type_for_path(path: &str) -> Result<SourceType, String> {
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

pub(crate) fn plan_source_file(
    file: &SourceFile,
    css_path: &str,
    is_module: bool,
    candidates: &HashMap<SelectorKey, Vec<String>>,
) -> Result<SourcePlan, String> {
    let allocator = Allocator::default();
    let source_type = source_type_for_path(&file.path)
        .map_err(|error| format!("Unsupported source file {}: {error}", file.path))?;
    let parsed = Parser::new(&allocator, &file.source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "Failed to parse {}: {:?}",
            file.path, parsed.diagnostics
        ));
    }
    let semantic = SemanticBuilder::new_compiler().build(&parsed.program);
    if !semantic.diagnostics.is_empty() {
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
        candidates,
        edits: Vec::new(),
        emitted_candidates: BTreeSet::new(),
        module_refs: HashMap::new(),
        matched_module_refs: HashMap::new(),
        warnings: Vec::new(),
    };
    collector.visit_program(&parsed.program);

    let classified_import_refs = collector.module_refs.values().sum::<usize>();
    let module_references_safe =
        !imports.unsupported_shape && total_import_refs == classified_import_refs;
    if !module_references_safe && let Some(span) = imports.warning_span {
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

struct UsageCollector<'s> {
    source: &'s str,
    file_path: &'s str,
    is_module: bool,
    scoping: &'s Scoping,
    import_bindings: &'s [ImportBinding],
    candidates: &'s HashMap<SelectorKey, Vec<String>>,
    edits: Vec<Edit>,
    emitted_candidates: BTreeSet<String>,
    module_refs: HashMap<String, usize>,
    matched_module_refs: HashMap<String, usize>,
    warnings: Vec<Warning>,
}

impl UsageCollector<'_> {
    fn module_member_name<'a>(&self, member: &'a StaticMemberExpression<'a>) -> Option<&'a str> {
        let Expression::Identifier(object) = &member.object else {
            return None;
        };
        let reference = object.reference_id.get()?;
        let symbol = self.scoping.get_reference(reference).symbol_id()?;
        self.import_bindings
            .iter()
            .any(|binding| binding.symbol == symbol)
            .then(|| member.property.name.as_str())
    }

    fn static_template(
        &self,
        template: &TemplateLiteral<'_>,
    ) -> Option<(String, Vec<String>, Vec<String>)> {
        let mut value = String::new();
        let mut original = String::new();
        let mut members = Vec::new();
        for (index, quasi) in template.quasis.iter().enumerate() {
            let cooked = quasi.value.cooked.as_ref()?.as_str();
            value.push_str(cooked);
            original.push_str(cooked);
            let Some(expression) = template.expressions.get(index) else {
                continue;
            };
            let Expression::StaticMemberExpression(member) = expression else {
                return None;
            };
            let name = self.module_member_name(member)?.to_string();
            let candidates = self.candidates.get(&SelectorKey::Class(name.clone()))?;
            value.push_str(&candidates.join(" "));
            original.push('\0');
            members.push(name);
        }
        let static_classes = original
            .split_whitespace()
            .filter(|class| !class.contains('\0'))
            .map(str::to_string)
            .collect();
        Some((
            value.split_whitespace().collect::<Vec<_>>().join(" "),
            members,
            static_classes,
        ))
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
                if let Some(JSXAttributeValue::StringLiteral(literal)) = &attribute.value {
                    class_literal = Some((literal.span, literal.value.to_string()));
                }
            } else if name.name == "id"
                && let Some(JSXAttributeValue::StringLiteral(literal)) = &attribute.value
                && let Some(candidates) = self
                    .candidates
                    .get(&SelectorKey::Id(literal.value.to_string()))
            {
                id_candidates.extend(candidates.clone());
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
            for candidate in &id_candidates {
                self.emitted_candidates.insert(candidate.clone());
            }
            self.edits.push(Edit {
                start: insertion,
                end: insertion,
                replacement: format!(" className=\"{}\"", id_candidates.join(" ")),
            });
        }
    }

    fn global_literal_edit(&mut self, span: Span, value: &str, extra_candidates: &[String]) {
        let mut classes = value
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let original_classes = classes.clone();
        for class in original_classes {
            if let Some(candidates) = self.candidates.get(&SelectorKey::Class(class)) {
                for candidate in candidates {
                    self.emitted_candidates.insert(candidate.clone());
                    if !classes.contains(candidate) {
                        classes.push(candidate.clone());
                    }
                }
            }
        }
        for candidate in extra_candidates {
            self.emitted_candidates.insert(candidate.clone());
            if !classes.contains(candidate) {
                classes.push(candidate.clone());
            }
        }
        let replacement_value = classes.join(" ");
        if replacement_value == value {
            return;
        }
        let quote = self.source.as_bytes()[span.start as usize] as char;
        self.edits.push(Edit {
            start: span.start as usize,
            end: span.end as usize,
            replacement: format!("{quote}{replacement_value}{quote}"),
        });
    }
}

impl<'a> Visit<'a> for UsageCollector<'_> {
    fn visit_static_member_expression(&mut self, member: &StaticMemberExpression<'a>) {
        if let Some(name) = self.module_member_name(member).map(str::to_string) {
            *self.module_refs.entry(name).or_default() += 1;
        }
        walk::walk_static_member_expression(self, member);
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
            let (replacement_value, members, static_classes) = match &container.expression {
                JSXExpression::StaticMemberExpression(member) => {
                    let Some(member_name) = self.module_member_name(member).map(str::to_string)
                    else {
                        continue;
                    };
                    let key = SelectorKey::Class(member_name.clone());
                    let Some(candidates) = self.candidates.get(&key) else {
                        continue;
                    };
                    (candidates.join(" "), vec![member_name], Vec::new())
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
            self.edits.push(Edit {
                start: container.span.start as usize,
                end: container.span.end as usize,
                replacement: serde_json::to_string(&replacement_value)
                    .expect("string serialization"),
            });
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

fn resolve_import(file_path: &str, import: &str) -> PathBuf {
    let parent = Path::new(file_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    normalize_path(&parent.join(import))
}

fn normalize_path(path: &Path) -> PathBuf {
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

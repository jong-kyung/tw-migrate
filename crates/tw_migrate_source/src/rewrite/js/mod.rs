//! JS/JSX-side rewriting: locate CSS Module references and plan span edits.

use std::{
    collections::{BTreeSet, HashMap},
    path::{Component, Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, CallExpression, ExportAllDeclaration, ExportFromDeclaration, Expression,
    ImportDeclaration, ImportDeclarationSpecifier, ImportExpression,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::{SourceType, Span};
use oxc_syntax::symbol::SymbolId;
use tw_migrate_css::SelectorKey;
use tw_migrate_error::{MigrationError, MigrationResult};

use crate::{Edit, SourceFile, Warning, original_offset};

pub struct CandidateMatch {
    pub start: usize,
    pub end: usize,
    pub key: SelectorKey,
    pub candidate: String,
    pub origin_candidate: String,
}

pub struct SourcePlan {
    pub edits: Vec<Edit>,
    pub removable_import_edits: Vec<Edit>,
    pub candidates: Vec<String>,
    pub matches: Vec<CandidateMatch>,
    pub module_refs: HashMap<String, usize>,
    pub matched_module_refs: HashMap<String, usize>,
    pub module_references_safe: bool,
    pub warnings: Vec<Warning>,
}

/// The no-op plan: nothing to edit and no references observed, which is safe
/// (`module_references_safe: true`).
impl Default for SourcePlan {
    fn default() -> Self {
        Self {
            edits: Vec::new(),
            removable_import_edits: Vec::new(),
            candidates: Vec::new(),
            matches: Vec::new(),
            module_refs: HashMap::new(),
            matched_module_refs: HashMap::new(),
            module_references_safe: true,
            warnings: Vec::new(),
        }
    }
}

pub fn source_type_for_path(path: &str) -> Result<SourceType, String> {
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
pub fn opaque_reference_plan(file: &SourceFile, css_path: &str, is_module: bool) -> SourcePlan {
    let referenced = is_module
        && Path::new(css_path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| file.source.contains(name));
    SourcePlan {
        module_references_safe: !referenced,
        warnings: if referenced {
            vec![Warning::new(
                "unsupported-css-module-reference",
                file.path.clone(),
                (0, 0),
                "The file could not be parsed, so its possible reference retains the CSS Module."
                    .to_string(),
            )]
        } else {
            Vec::new()
        },
        ..Default::default()
    }
}

pub fn plan_batch_source_file(
    file: &SourceFile,
    css_path: &str,
    is_module: bool,
    candidates: &HashMap<SelectorKey, Vec<String>>,
    candidate_properties: &HashMap<String, BTreeSet<String>>,
    preserved_module_classes: &BTreeSet<String>,
) -> MigrationResult<SourcePlan> {
    let allocator = Allocator::default();
    let source_type =
        source_type_for_path(&file.path).map_err(|error| MigrationError::UnsupportedSource {
            message: format!("Unsupported source file {}: {error}", file.path),
        })?;
    let parsed = Parser::new(&allocator, &file.source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        if !file.writable {
            return Ok(opaque_reference_plan(file, css_path, is_module));
        }
        return Err(MigrationError::SourceParse {
            message: format!("Failed to parse {}: {:?}", file.path, parsed.diagnostics),
        });
    }
    let semantic = SemanticBuilder::new_compiler().build(&parsed.program);
    if !semantic.diagnostics.is_empty() {
        if !file.writable {
            return Ok(opaque_reference_plan(file, css_path, is_module));
        }
        return Err(MigrationError::SourceAnalysis {
            message: format!(
                "Failed to analyze {}: {:?}",
                file.path, semantic.diagnostics
            ),
        });
    }

    let mut imports = ImportCollector {
        file_path: &file.path,
        css_target: normalize_path(Path::new(css_path)),
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
        candidate_properties,
        preserved_module_classes,
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
        collector.warnings.push(Warning::new(
            "unsupported-css-module-reference",
            file.path.clone(),
            (span.start as usize, span.end as usize),
            "The CSS Module has an import or reference that cannot be migrated safely.".to_string(),
        ));
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

    for warning in &mut collector.warnings {
        if (warning.start, warning.end) != (0, 0) {
            warning.start = original_offset(&file.prior_edits, warning.start);
            warning.end = original_offset(&file.prior_edits, warning.end);
        }
    }

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
    css_target: PathBuf,
    bindings: Vec<ImportBinding>,
    unsupported_shape: bool,
    warning_span: Option<Span>,
}

impl<'a> Visit<'a> for ImportCollector<'_> {
    fn visit_import_declaration(&mut self, declaration: &ImportDeclaration<'a>) {
        let resolved = resolve_import(self.file_path, declaration.source.value.as_str());
        if resolved == self.css_target {
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

    fn visit_export_from_declaration(&mut self, declaration: &ExportFromDeclaration<'a>) {
        if resolve_import(self.file_path, declaration.source.value.as_str()) == self.css_target {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(declaration.span);
        }
        walk::walk_export_from_declaration(self, declaration);
    }

    fn visit_export_all_declaration(&mut self, declaration: &ExportAllDeclaration<'a>) {
        if resolve_import(self.file_path, declaration.source.value.as_str()) == self.css_target {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(declaration.span);
        }
        walk::walk_export_all_declaration(self, declaration);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Expression::Identifier(callee) = &call.callee
            && callee.name == "require"
            && let Some(Argument::StringLiteral(source)) = call.arguments.first()
            && resolve_import(self.file_path, source.value.as_str()) == self.css_target
        {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(call.span);
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_import_expression(&mut self, import: &ImportExpression<'a>) {
        if let Expression::StringLiteral(source) = &import.source
            && resolve_import(self.file_path, source.value.as_str()) == self.css_target
        {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(import.span);
        }
        walk::walk_import_expression(self, import);
    }
}

mod usage;

use usage::UsageCollector;

pub fn resolve_import(file_path: &str, import: &str) -> PathBuf {
    let parent = Path::new(file_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    normalize_path(&parent.join(import))
}

pub fn normalize_path(path: &Path) -> PathBuf {
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

pub fn validate_js(path: &str, source: &str) -> MigrationResult<()> {
    let allocator = Allocator::default();
    let source_type =
        source_type_for_path(path).map_err(|error| MigrationError::UnsupportedSource {
            message: format!("Unsupported source file {path}: {error}"),
        })?;
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.diagnostics.is_empty() {
        Ok(())
    } else {
        Err(MigrationError::OutputValidation {
            message: format!("Edited source no longer parses: {path}"),
        })
    }
}

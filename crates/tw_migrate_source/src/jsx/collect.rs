use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionExpression, BindingIdentifier, BindingPattern, Class, Declaration,
    ExportDefaultDeclarationKind, Expression, Function, IdentifierReference,
    ImportDeclarationSpecifier, JSXMemberExpression, JSXMemberExpressionObject, ModuleExportName,
    Statement, StaticMemberExpression, VariableDeclaration,
};
use oxc_ast_visit::Visit;
use oxc_parser::Parser;
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_span::Span;
use oxc_syntax::symbol::SymbolId;

use super::component::{Sweep, build_component};
use super::{Comp, FileIr, Forward, ImportedName, R_BOUNDARY, R_HOC, TagRef};
use crate::{normalize_path, resolve_import, source_type_for_path};

pub(super) struct FileSymbols<'s> {
    pub(super) scoping: &'s Scoping,
    pub(super) file_path: &'s str,
    pub(super) css_target: PathBuf,
    pub(super) css_symbol: Option<SymbolId>,
    pub(super) comp_symbols: HashMap<SymbolId, usize>,
    pub(super) import_symbols: HashMap<SymbolId, String>,
    pub(super) ns_symbols: HashMap<SymbolId, String>,
}

#[derive(Default)]
pub(super) struct FileOut {
    pub(super) boundary_usages: Vec<(String, (usize, usize), &'static str)>,
    pub(super) rendered_marks: Vec<(TagRef, &'static str)>,
    pub(super) ns_member_specs: Vec<String>,
    pub(super) unsound: bool,
}

#[derive(Clone, Copy)]
pub(super) enum FnRef<'a> {
    Function(&'a Function<'a>),
    Arrow(&'a ArrowFunctionExpression<'a>),
}

enum SweepTarget<'a> {
    Stmt(&'a Statement<'a>),
    Expr(&'a Expression<'a>),
    Class(&'a Class<'a>),
}

pub(super) fn span2(span: Span) -> (usize, usize) {
    (span.start as usize, span.end as usize)
}

pub(super) fn symbol_of(ident: &IdentifierReference, scoping: &Scoping) -> Option<SymbolId> {
    scoping.get_reference(ident.reference_id.get()?).symbol_id()
}

/// Property name of `member` when its object is exactly the binding `target`.
pub(super) fn member_on<'b>(
    scoping: &Scoping,
    target: Option<SymbolId>,
    member: &'b StaticMemberExpression<'b>,
) -> Option<&'b str> {
    let target = target?;
    let Expression::Identifier(object) = &member.object else {
        return None;
    };
    (symbol_of(object, scoping) == Some(target)).then(|| member.property.name.as_str())
}

pub(super) fn ident_is(
    scoping: &Scoping,
    target: Option<SymbolId>,
    ident: &IdentifierReference,
) -> bool {
    target.is_some() && symbol_of(ident, scoping) == target
}

pub(super) fn mark_namespace_member(
    syms: &FileSymbols<'_>,
    out: &mut FileOut,
    member: &JSXMemberExpression<'_>,
) {
    let mut object = &member.object;
    loop {
        match object {
            JSXMemberExpressionObject::MemberExpression(inner) => object = &inner.object,
            JSXMemberExpressionObject::IdentifierReference(reference) => {
                if let Some(sym) = symbol_of(reference, syms.scoping)
                    && let Some(spec) = syms.ns_symbols.get(&sym)
                {
                    out.ns_member_specs.push(spec.clone());
                }
                return;
            }
            JSXMemberExpressionObject::ThisExpression(_) => return,
        }
    }
}

pub(super) fn extract_file(path: &str, source: &str, css_path: &str) -> Option<FileIr> {
    let allocator = Allocator::default();
    let source_type = source_type_for_path(path).ok()?;
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let semantic = SemanticBuilder::new_compiler().build(&parsed.program);
    if !semantic.diagnostics.is_empty() {
        return None;
    }
    let mut syms = FileSymbols {
        scoping: semantic.semantic.scoping(),
        file_path: path,
        css_target: normalize_path(Path::new(css_path)),
        css_symbol: None,
        comp_symbols: HashMap::new(),
        import_symbols: HashMap::new(),
        ns_symbols: HashMap::new(),
    };
    let mut out = FileOut::default();
    let mut comps: Vec<Comp> = Vec::new();
    let mut named_exports = HashMap::new();
    let mut default_export = None;
    let mut imports = HashMap::new();
    let mut builds: Vec<(usize, FnRef)> = Vec::new();
    let mut sweeps: Vec<(SweepTarget, &'static str)> = Vec::new();
    let mut deferred_specifiers = Vec::new();
    let mut deferred_default: Option<&IdentifierReference> = None;

    for stmt in &parsed.program.body {
        match stmt {
            Statement::ImportDeclaration(decl) => {
                let is_css = resolve_import(path, decl.source.value.as_str()) == syms.css_target;
                let Some(specifiers) = &decl.specifiers else {
                    continue;
                };
                for specifier in specifiers {
                    match specifier {
                        ImportDeclarationSpecifier::ImportDefaultSpecifier(spec) => {
                            if is_css {
                                syms.css_symbol = spec.local.symbol_id.get();
                            } else if let Some(sym) = spec.local.symbol_id.get() {
                                let local = spec.local.name.to_string();
                                imports.insert(
                                    local.clone(),
                                    (decl.source.value.to_string(), ImportedName::Default),
                                );
                                syms.import_symbols.insert(sym, local);
                            }
                        }
                        ImportDeclarationSpecifier::ImportSpecifier(spec) => {
                            if is_css {
                                out.unsound = true;
                            } else if let Some(sym) = spec.local.symbol_id.get() {
                                let local = spec.local.name.to_string();
                                imports.insert(
                                    local.clone(),
                                    (
                                        decl.source.value.to_string(),
                                        ImportedName::Named(spec.imported.name().to_string()),
                                    ),
                                );
                                syms.import_symbols.insert(sym, local);
                            }
                        }
                        ImportDeclarationSpecifier::ImportNamespaceSpecifier(spec) => {
                            if is_css {
                                out.unsound = true;
                            } else if let Some(sym) = spec.local.symbol_id.get() {
                                syms.ns_symbols.insert(sym, decl.source.value.to_string());
                            }
                        }
                    }
                }
            }
            Statement::FunctionDeclaration(func) => {
                register(
                    &mut comps,
                    &mut syms,
                    &mut builds,
                    func.id.as_ref(),
                    FnRef::Function(func),
                );
            }
            Statement::VariableDeclaration(decl) => {
                register_declarators(
                    &mut comps,
                    &mut syms,
                    &mut builds,
                    &mut sweeps,
                    &mut named_exports,
                    decl,
                    false,
                );
            }
            // Re-exports (`export { x } from '...'`) are not followed; importers
            // fail to resolve, which is the conservative direction.
            Statement::ExportFromDeclaration(_) => {}
            Statement::ExportDeclaration(export) => match &export.declaration {
                Declaration::FunctionDeclaration(func) => {
                    let ix = register(
                        &mut comps,
                        &mut syms,
                        &mut builds,
                        func.id.as_ref(),
                        FnRef::Function(func),
                    );
                    if let Some(id) = &func.id {
                        named_exports.insert(id.name.to_string(), ix);
                    }
                }
                Declaration::VariableDeclaration(decl) => {
                    register_declarators(
                        &mut comps,
                        &mut syms,
                        &mut builds,
                        &mut sweeps,
                        &mut named_exports,
                        decl,
                        true,
                    );
                }
                Declaration::ClassDeclaration(class) => {
                    sweeps.push((SweepTarget::Class(class), R_BOUNDARY));
                }
                _ => {}
            },
            Statement::ExportNamedDeclaration(export) => {
                for specifier in &export.specifiers {
                    deferred_specifiers.push(specifier);
                }
            }
            Statement::ExportDefaultDeclaration(export) => match &export.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                    let ix = register(
                        &mut comps,
                        &mut syms,
                        &mut builds,
                        func.id.as_ref(),
                        FnRef::Function(func),
                    );
                    default_export = Some(ix);
                }
                ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
                    let ix = register(
                        &mut comps,
                        &mut syms,
                        &mut builds,
                        None,
                        FnRef::Arrow(arrow),
                    );
                    default_export = Some(ix);
                }
                ExportDefaultDeclarationKind::Identifier(ident) => {
                    deferred_default = Some(ident);
                }
                ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                    sweeps.push((SweepTarget::Class(class), R_BOUNDARY));
                }
                declaration => {
                    if let Some(expr) = declaration.as_expression() {
                        sweeps.push((SweepTarget::Expr(expr), R_BOUNDARY));
                    }
                }
            },
            Statement::ExportAllDeclaration(_) => {}
            Statement::TSTypeAliasDeclaration(_)
            | Statement::TSInterfaceDeclaration(_)
            | Statement::TSEnumDeclaration(_)
            | Statement::TSExternalModuleDeclaration(_)
            | Statement::TSNamespaceDeclaration(_)
            | Statement::TSGlobalDeclaration(_)
            | Statement::TSImportEqualsDeclaration(_) => {}
            other => sweeps.push((SweepTarget::Stmt(other), R_BOUNDARY)),
        }
    }

    for specifier in deferred_specifiers {
        if let ModuleExportName::IdentifierReference(local) = &specifier.local
            && let Some(sym) = symbol_of(local, syms.scoping)
            && let Some(&ix) = syms.comp_symbols.get(&sym)
        {
            let exported = specifier.exported.name().to_string();
            if exported == "default" {
                default_export = Some(ix);
            } else {
                named_exports.insert(exported, ix);
            }
        }
    }
    if let Some(ident) = deferred_default
        && let Some(sym) = symbol_of(ident, syms.scoping)
        && let Some(&ix) = syms.comp_symbols.get(&sym)
    {
        default_export = Some(ix);
    }

    for (ix, fnref) in &builds {
        build_component(&syms, &mut out, &mut comps, *ix, *fnref);
    }
    for (target, reason) in sweeps {
        let mut sweep = Sweep::file_level(&syms, &mut out, reason);
        match target {
            SweepTarget::Stmt(stmt) => sweep.visit_statement(stmt),
            SweepTarget::Expr(expr) => sweep.visit_expression(expr),
            SweepTarget::Class(class) => sweep.visit_class(class),
        }
    }

    Some(FileIr {
        path: path.to_string(),
        comps,
        named_exports,
        default_export,
        imports,
        boundary_usages: out.boundary_usages,
        rendered_marks: out.rendered_marks,
        ns_member_specs: out.ns_member_specs,
        unsound: out.unsound,
    })
}

fn register<'a>(
    comps: &mut Vec<Comp>,
    syms: &mut FileSymbols<'_>,
    builds: &mut Vec<(usize, FnRef<'a>)>,
    id: Option<&BindingIdentifier<'a>>,
    fnref: FnRef<'a>,
) -> usize {
    let ix = comps.len();
    comps.push(Comp {
        body: Err(R_HOC),
        slots: Vec::new(),
        forward: Forward::No,
        children_bad: false,
    });
    if let Some(id) = id
        && let Some(sym) = id.symbol_id.get()
    {
        syms.comp_symbols.insert(sym, ix);
    }
    builds.push((ix, fnref));
    ix
}

fn register_declarators<'a>(
    comps: &mut Vec<Comp>,
    syms: &mut FileSymbols<'_>,
    builds: &mut Vec<(usize, FnRef<'a>)>,
    sweeps: &mut Vec<(SweepTarget<'a>, &'static str)>,
    named_exports: &mut HashMap<String, usize>,
    decl: &'a VariableDeclaration<'a>,
    exported: bool,
) {
    for declarator in &decl.declarations {
        let BindingPattern::BindingIdentifier(id) = &declarator.id else {
            if let Some(init) = &declarator.init {
                sweeps.push((SweepTarget::Expr(init), R_BOUNDARY));
            }
            continue;
        };
        let Some(init) = &declarator.init else {
            continue;
        };
        match init.get_inner_expression() {
            Expression::ArrowFunctionExpression(arrow) => {
                let ix = register(comps, syms, builds, Some(id), FnRef::Arrow(arrow));
                if exported {
                    named_exports.insert(id.name.to_string(), ix);
                }
            }
            Expression::FunctionExpression(func) => {
                let ix = register(comps, syms, builds, Some(id), FnRef::Function(func));
                if exported {
                    named_exports.insert(id.name.to_string(), ix);
                }
            }
            Expression::CallExpression(_) => {
                // HOC-produced binding: usable as a tag, never provable.
                let ix = comps.len();
                comps.push(Comp {
                    body: Err(R_HOC),
                    slots: Vec::new(),
                    forward: Forward::No,
                    children_bad: false,
                });
                if let Some(sym) = id.symbol_id.get() {
                    syms.comp_symbols.insert(sym, ix);
                }
                if exported {
                    named_exports.insert(id.name.to_string(), ix);
                }
                sweeps.push((SweepTarget::Expr(init), R_HOC));
            }
            _ => sweeps.push((SweepTarget::Expr(init), R_BOUNDARY)),
        }
    }
}

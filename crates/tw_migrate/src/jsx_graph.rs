//! JSX/component graph engine: prove CSS Module class relationships statically.
//!
//! Pure analysis, no edits. For CSS Module classes A (ancestor) and B
//! (target), answers whether every usage of B is in a statically proven
//! relationship (descendant or direct child) under an element carrying A,
//! following the RFC-conservative rules: project-local function components, a
//! single statically analyzable JSX return, direct JSX ancestry, direct
//! `children` passthrough, and direct `className` prop forwarding. Anything
//! else (conditional renders, portals, HOCs, dynamic components, arbitrary
//! prop transformations, runtime trees) is never inferred.
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrowFunctionExpression, BindingIdentifier, BindingPattern, CallExpression,
    Class, Declaration, ExportDefaultDeclarationKind, Expression, FormalParameters, Function,
    FunctionBody, IdentifierReference, ImportDeclarationSpecifier, ImportExpression,
    JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement, JSXElementName,
    JSXExpression, JSXMemberExpression, JSXMemberExpressionObject, ModuleExportName, PropertyKey,
    ReturnStatement, Statement, StaticMemberExpression, VariableDeclaration,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_span::Span;
use oxc_syntax::scope::ScopeFlags;
use oxc_syntax::symbol::SymbolId;

use crate::css_plan::SelectorKey;
use crate::js_rewrite::{normalize_path, resolve_import, source_type_for_path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Relation {
    Child,
    Descendant,
}

#[derive(Debug)]
pub(crate) struct UsageProof {
    pub(crate) file: String,
    pub(crate) span: (usize, usize),
    pub(crate) proven: bool,
    pub(crate) reason: Option<&'static str>,
}

#[derive(Debug)]
pub(crate) struct ProofOutcome {
    pub(crate) usages: Vec<UsageProof>,
    pub(crate) aggregate_proven: bool,
    pub(crate) reason: Option<&'static str>,
}

const R_CONDITIONAL: &str = "conditional-return";
const R_PORTAL: &str = "portal";
const R_HOC: &str = "hoc-or-dynamic-component";
const R_RECURSIVE: &str = "recursive-component";
const R_UNRESOLVED: &str = "unresolved-component-import";
const R_BOUNDARY: &str = "dynamic-content-boundary";
const R_ANCESTRY: &str = "unproven-ancestry";
const R_NO_USAGES: &str = "no-usages";
const R_EXPORTED: &str = "exported-render-sites-unknown";

/// Prove `ancestor relation target` for every usage of `target` under the
/// closed-world assumption: `files` is the whole scanned project, so every
/// render site of every component can be enumerated within it.
#[cfg(test)]
pub(crate) fn prove(
    files: &[(&str, &str)],
    css_path: &str,
    ancestor: &SelectorKey,
    relation: Relation,
    target: &SelectorKey,
) -> ProofOutcome {
    prove_in_world(files, css_path, ancestor, relation, target, true)
}

/// Like [`prove`], but `closed_world: false` marks exported components as
/// having unknown render sites (`exported-render-sites-unknown`).
#[cfg(test)]
pub(crate) fn prove_in_world(
    files: &[(&str, &str)],
    css_path: &str,
    ancestor: &SelectorKey,
    relation: Relation,
    target: &SelectorKey,
    closed_world: bool,
) -> ProofOutcome {
    prove_prepared(
        &prepare(files, css_path),
        ancestor,
        relation,
        target,
        closed_world,
    )
}

/// The extracted-and-linked world for one (files, css_path) pair: the
/// query-invariant part of a proof, reusable across many queries.
pub(crate) struct PreparedWorld {
    world: World,
    linked: Linked,
}

/// Build the [`PreparedWorld`] (per-file extraction plus cross-file linking).
pub(crate) fn prepare(files: &[(&str, &str)], css_path: &str) -> PreparedWorld {
    let mut world = World {
        files: Vec::new(),
        parse_failure: false,
    };
    for (path, source) in files {
        match extract_file(path, source, css_path) {
            Some(file) => world.files.push(file),
            None => world.parse_failure = true,
        }
    }
    let linked = link(&world);
    PreparedWorld { world, linked }
}

/// Run one `ancestor relation target` query against a [`PreparedWorld`].
pub(crate) fn prove_prepared(
    prepared: &PreparedWorld,
    ancestor: &SelectorKey,
    relation: Relation,
    target: &SelectorKey,
    closed_world: bool,
) -> ProofOutcome {
    let PreparedWorld { world, linked } = prepared;
    let target_name = match target {
        SelectorKey::Class(name) | SelectorKey::Id(name) => name.as_str(),
    };
    let query = ProofQuery {
        linked,
        world,
        relation,
        ancestor,
        closed_world,
    };
    let mut usages = Vec::new();
    for (file_ix, file) in world.files.iter().enumerate() {
        for (name, span, reason) in &file.boundary_usages {
            if name == target_name {
                usages.push(UsageProof {
                    file: file.path.clone(),
                    span: *span,
                    proven: false,
                    reason: Some(reason),
                });
            }
        }
        for (comp_ix, comp) in file.comps.iter().enumerate() {
            let Ok(nodes) = &comp.body else { continue };
            for (node_ix, node) in nodes.iter().enumerate() {
                match &node.kind {
                    NodeKind::Element { keys } => {
                        for (key, span) in keys {
                            if key == target {
                                let result = prove_up(
                                    &query,
                                    (file_ix, comp_ix),
                                    node_ix,
                                    &[],
                                    &BTreeSet::new(),
                                    0,
                                );
                                usages.push(usage_proof(&file.path, *span, result));
                            }
                        }
                    }
                    NodeKind::ComponentUse { tag, class_keys } => {
                        if !matches!(target, SelectorKey::Class(_)) {
                            continue;
                        }
                        for (name, span) in class_keys {
                            if name == target_name {
                                let result = prove_forward(
                                    &query,
                                    (file_ix, comp_ix),
                                    node_ix,
                                    tag,
                                );
                                usages.push(usage_proof(&file.path, *span, result));
                            }
                        }
                    }
                    NodeKind::Slot => {}
                }
            }
        }
    }
    let unsound = world.parse_failure || world.files.iter().any(|file| file.unsound);
    let (aggregate_proven, reason) = if unsound {
        (false, Some(R_BOUNDARY))
    } else if usages.is_empty() {
        (false, Some(R_NO_USAGES))
    } else if let Some(unproven) = usages.iter().find(|usage| !usage.proven) {
        (false, unproven.reason)
    } else {
        (true, None)
    };
    ProofOutcome {
        usages,
        aggregate_proven,
        reason,
    }
}

fn usage_proof(path: &str, span: (usize, usize), result: Result<(), &'static str>) -> UsageProof {
    UsageProof {
        file: path.to_string(),
        span,
        proven: result.is_ok(),
        reason: result.err(),
    }
}

// ---------------------------------------------------------------------------
// Owned intermediate representation
// ---------------------------------------------------------------------------

/// (file index, component index) within the [`World`].
type CompId = (usize, usize);

#[derive(Clone, Debug)]
enum TagRef {
    /// Component defined in the same file.
    Local(usize),
    /// Component imported under this local name.
    Import(String),
    /// Anything else: member tags, undefined identifiers, nested components.
    Unknown,
}

#[derive(Debug)]
enum NodeKind {
    /// Host element with its statically known CSS Module keys.
    Element {
        keys: Vec<(SelectorKey, (usize, usize))>,
    },
    /// Invocation of a (possibly unresolved) component, with any CSS Module
    /// class names passed through the `className` prop.
    ComponentUse {
        tag: TagRef,
        class_keys: Vec<(String, (usize, usize))>,
    },
    /// `{props.children}` / `{children}` passthrough position.
    Slot,
}

#[derive(Debug)]
struct Node {
    parent: Option<usize>,
    kind: NodeKind,
}

#[derive(Debug)]
enum Forward {
    /// `props.className` never lands anywhere.
    No,
    /// `props.className` lands exactly on this element node.
    Target(usize),
    /// `props.className` is used in a way that cannot be proven.
    Bad,
}

#[derive(Clone, Debug)]
enum ImportedName {
    Default,
    Named(String),
}

#[derive(Debug)]
struct Comp {
    exported: bool,
    body: Result<Vec<Node>, &'static str>,
    slots: Vec<usize>,
    forward: Forward,
    children_bad: bool,
}

#[derive(Debug)]
struct FileIr {
    path: String,
    comps: Vec<Comp>,
    named_exports: HashMap<String, usize>,
    default_export: Option<usize>,
    imports: HashMap<String, (String, ImportedName)>,
    /// CSS Module member usages found in positions the proof cannot follow,
    /// pre-disqualified with their reason.
    boundary_usages: Vec<(String, (usize, usize), &'static str)>,
    /// Components rendered (or escaping as values) in unanalyzable regions.
    rendered_marks: Vec<(TagRef, &'static str)>,
    /// Specifiers of namespace imports used as member JSX tags.
    ns_member_specs: Vec<String>,
    /// The CSS Module binding itself escapes static tracking in this file.
    unsound: bool,
}

struct World {
    files: Vec<FileIr>,
    parse_failure: bool,
}

// ---------------------------------------------------------------------------
// Per-file extraction
// ---------------------------------------------------------------------------

struct FileSymbols<'s> {
    scoping: &'s Scoping,
    file_path: &'s str,
    css_target: PathBuf,
    css_symbol: Option<SymbolId>,
    comp_symbols: HashMap<SymbolId, usize>,
    import_symbols: HashMap<SymbolId, String>,
    ns_symbols: HashMap<SymbolId, String>,
}

#[derive(Default)]
struct FileOut {
    boundary_usages: Vec<(String, (usize, usize), &'static str)>,
    rendered_marks: Vec<(TagRef, &'static str)>,
    ns_member_specs: Vec<String>,
    unsound: bool,
}

#[derive(Clone, Copy)]
enum FnRef<'a> {
    Function(&'a Function<'a>),
    Arrow(&'a ArrowFunctionExpression<'a>),
}

enum SweepTarget<'a> {
    Stmt(&'a Statement<'a>),
    Expr(&'a Expression<'a>),
    Class(&'a Class<'a>),
}

fn span2(span: Span) -> (usize, usize) {
    (span.start as usize, span.end as usize)
}

fn symbol_of(ident: &IdentifierReference, scoping: &Scoping) -> Option<SymbolId> {
    scoping.get_reference(ident.reference_id.get()?).symbol_id()
}

/// Property name of `member` when its object is exactly the binding `target`.
fn member_on<'b>(
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

fn ident_is(scoping: &Scoping, target: Option<SymbolId>, ident: &IdentifierReference) -> bool {
    target.is_some() && symbol_of(ident, scoping) == target
}

fn mark_namespace_member(
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

fn extract_file(path: &str, source: &str, css_path: &str) -> Option<FileIr> {
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
                    false,
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
            Statement::ExportNamedDeclaration(export) => {
                if export.source.is_some() {
                    // Re-exports are not followed; importers fail to resolve,
                    // which is the conservative direction.
                    continue;
                }
                match &export.declaration {
                    Some(Declaration::FunctionDeclaration(func)) => {
                        let ix = register(
                            &mut comps,
                            &mut syms,
                            &mut builds,
                            func.id.as_ref(),
                            FnRef::Function(func),
                            true,
                        );
                        if let Some(id) = &func.id {
                            named_exports.insert(id.name.to_string(), ix);
                        }
                    }
                    Some(Declaration::VariableDeclaration(decl)) => {
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
                    _ => {}
                }
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
                        true,
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
                        true,
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
            | Statement::TSModuleDeclaration(_)
            | Statement::TSImportEqualsDeclaration(_) => {}
            other => sweeps.push((SweepTarget::Stmt(other), R_BOUNDARY)),
        }
    }

    for specifier in deferred_specifiers {
        if let ModuleExportName::IdentifierReference(local) = &specifier.local
            && let Some(sym) = symbol_of(local, syms.scoping)
            && let Some(&ix) = syms.comp_symbols.get(&sym)
        {
            comps[ix].exported = true;
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
        comps[ix].exported = true;
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
    exported: bool,
) -> usize {
    let ix = comps.len();
    comps.push(Comp {
        exported,
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
                let ix = register(comps, syms, builds, Some(id), FnRef::Arrow(arrow), exported);
                if exported {
                    named_exports.insert(id.name.to_string(), ix);
                }
            }
            Expression::FunctionExpression(func) => {
                let ix = register(comps, syms, builds, Some(id), FnRef::Function(func), exported);
                if exported {
                    named_exports.insert(id.name.to_string(), ix);
                }
            }
            Expression::CallExpression(_) => {
                // HOC-produced binding: usable as a tag, never provable.
                let ix = comps.len();
                comps.push(Comp {
                    exported,
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

// ---------------------------------------------------------------------------
// Component qualification
// ---------------------------------------------------------------------------

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
                    BindingPattern::AssignmentPattern(assignment) => {
                        match &assignment.left {
                            BindingPattern::BindingIdentifier(id) => Some(id),
                            _ => None,
                        }
                    }
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
        FnRef::Arrow(arrow) => (!arrow.expression).then(|| &*arrow.body),
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
        FnRef::Arrow(arrow) => portals.visit_function_body(&arrow.body),
    }
    if portals.found {
        return Err(R_PORTAL);
    }
    let argument = match fnref {
        FnRef::Arrow(arrow) if arrow.expression => arrow.get_expression().ok_or(R_HOC)?,
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
            Expression::StaticMemberExpression(member) => {
                member.property.name == "createPortal"
            }
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

fn build_component(
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
                    sweep.visit_function_body(&arrow.body);
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
                            // ponytail: spread props may override className;
                            // usages stay recorded, ancestors stay conservative.
                            self.sweep_expr(&spread.argument, R_BOUNDARY);
                        }
                    }
                }
                let ix = self.push(parent, NodeKind::Element { keys });
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
                                    self.component_class_part(inner, &mut class_keys);
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

    /// One part of a host element `className`: a module key, a forwarded
    /// `props.className`, a nested template, or an opaque expression.
    fn class_part(
        &mut self,
        expression: &Expression<'_>,
        keys: &mut Vec<(SelectorKey, (usize, usize))>,
        forward: &mut bool,
    ) {
        match expression {
            Expression::StaticMemberExpression(member) => {
                if let Some(name) = member_on(self.syms.scoping, self.syms.css_symbol, member) {
                    keys.push((SelectorKey::Class(name.to_string()), span2(member.span)));
                } else if member_on(self.syms.scoping, self.props_sym, member)
                    == Some("className")
                {
                    *forward = true;
                } else {
                    self.sweep_expr(expression, R_BOUNDARY);
                }
            }
            Expression::Identifier(ident)
                if ident_is(self.syms.scoping, self.class_sym, ident) =>
            {
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

    /// One part of a `className` passed to a component invocation.
    fn component_class_part(
        &mut self,
        expression: &Expression<'_>,
        class_keys: &mut Vec<(String, (usize, usize))>,
    ) {
        match expression {
            Expression::StaticMemberExpression(member) => {
                if let Some(name) = member_on(self.syms.scoping, self.syms.css_symbol, member) {
                    class_keys.push((name.to_string(), span2(member.span)));
                } else if member_on(self.syms.scoping, self.props_sym, member)
                    == Some("className")
                {
                    // Chained forwarding into another component is not proven.
                    self.forward_bad = true;
                } else {
                    self.sweep_expr(expression, R_BOUNDARY);
                }
            }
            Expression::Identifier(ident)
                if ident_is(self.syms.scoping, self.class_sym, ident) =>
            {
                self.forward_bad = true;
            }
            Expression::TemplateLiteral(template) => {
                for part in &template.expressions {
                    self.component_class_part(part, class_keys);
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
                    let mut forward_here = false;
                    self.class_part(inner, keys, &mut forward_here);
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
struct Sweep<'x, 's> {
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
    fn file_level(
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
                        self.out.rendered_marks.push((TagRef::Local(ix), self.reason));
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

// ---------------------------------------------------------------------------
// Cross-file linking
// ---------------------------------------------------------------------------

/// Per-file map of import local name to its resolved (file, exported name),
/// or the disqualification reason.
type ResolvedImports = HashMap<String, Result<(usize, ImportedName), &'static str>>;

struct Linked {
    imports: Vec<ResolvedImports>,
    unanalyzable: HashMap<CompId, &'static str>,
    sites: HashMap<CompId, Vec<(CompId, usize)>>,
}

const PROBE_EXTENSIONS: [&str; 6] = ["tsx", "ts", "jsx", "js", "mjs", "cjs"];

/// Resolve an extensionless relative specifier against the request file set:
/// exact path, then each extension, then `index.*` under the path. Every
/// match is returned; zero or multiple matches disqualify at the call site.
fn resolve_specifier(paths: &[PathBuf], from: &str, specifier: &str) -> Vec<usize> {
    if !specifier.starts_with('.') {
        return Vec::new();
    }
    let base = resolve_import(from, specifier);
    let mut probes = vec![base.clone()];
    for extension in PROBE_EXTENSIONS {
        let mut with_extension = base.clone().into_os_string();
        with_extension.push(format!(".{extension}"));
        probes.push(PathBuf::from(with_extension));
    }
    for extension in PROBE_EXTENSIONS {
        probes.push(base.join(format!("index.{extension}")));
    }
    let mut matches = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        if probes.contains(path) {
            matches.push(index);
        }
    }
    matches
}

fn mark_exports(
    world: &World,
    file_ix: usize,
    reason: &'static str,
    out: &mut HashMap<CompId, &'static str>,
) {
    let file = &world.files[file_ix];
    for &comp in file.named_exports.values() {
        out.entry((file_ix, comp)).or_insert(reason);
    }
    if let Some(comp) = file.default_export {
        out.entry((file_ix, comp)).or_insert(reason);
    }
}

fn link(world: &World) -> Linked {
    let paths: Vec<PathBuf> = world
        .files
        .iter()
        .map(|file| normalize_path(Path::new(&file.path)))
        .collect();
    let mut unanalyzable: HashMap<CompId, &'static str> = HashMap::new();
    let mut imports = Vec::new();
    for file in &world.files {
        let mut resolved = ResolvedImports::new();
        for (local, (specifier, imported)) in &file.imports {
            let matches = resolve_specifier(&paths, &file.path, specifier);
            let entry = match matches.as_slice() {
                [only] => Ok((*only, imported.clone())),
                [] => Err(R_UNRESOLVED),
                many => {
                    // Ambiguous probe: every candidate's exports may be
                    // rendered through this import.
                    for &candidate in many {
                        mark_exports(world, candidate, R_UNRESOLVED, &mut unanalyzable);
                    }
                    Err(R_UNRESOLVED)
                }
            };
            resolved.insert(local.clone(), entry);
        }
        imports.push(resolved);
    }
    for file in &world.files {
        for specifier in &file.ns_member_specs {
            for candidate in resolve_specifier(&paths, &file.path, specifier) {
                mark_exports(world, candidate, R_HOC, &mut unanalyzable);
            }
        }
    }
    let mut linked = Linked {
        imports,
        unanalyzable,
        sites: HashMap::new(),
    };
    let mut marks = Vec::new();
    for (file_ix, file) in world.files.iter().enumerate() {
        for (tag, reason) in &file.rendered_marks {
            if let Ok(comp) = resolve_tag(&linked, world, file_ix, tag) {
                marks.push((comp, *reason));
            }
        }
    }
    for (comp, reason) in marks {
        linked.unanalyzable.entry(comp).or_insert(reason);
    }
    for (file_ix, file) in world.files.iter().enumerate() {
        for (comp_ix, comp) in file.comps.iter().enumerate() {
            let Ok(nodes) = &comp.body else { continue };
            for (node_ix, node) in nodes.iter().enumerate() {
                if let NodeKind::ComponentUse { tag, .. } = &node.kind
                    && let Ok(target) = resolve_tag(&linked, world, file_ix, tag)
                {
                    linked
                        .sites
                        .entry(target)
                        .or_default()
                        .push(((file_ix, comp_ix), node_ix));
                }
            }
        }
    }
    linked
}

fn resolve_tag(
    linked: &Linked,
    world: &World,
    file_ix: usize,
    tag: &TagRef,
) -> Result<CompId, &'static str> {
    match tag {
        TagRef::Local(comp_ix) => Ok((file_ix, *comp_ix)),
        TagRef::Import(local) => {
            let (target_file, imported) = linked.imports[file_ix]
                .get(local)
                .ok_or(R_UNRESOLVED)?
                .as_ref()
                .map_err(|reason| *reason)?;
            let file = &world.files[*target_file];
            let comp = match imported {
                ImportedName::Default => file.default_export,
                ImportedName::Named(name) => file.named_exports.get(name).copied(),
            };
            comp.map(|comp_ix| (*target_file, comp_ix)).ok_or(R_UNRESOLVED)
        }
        TagRef::Unknown => Err(R_HOC),
    }
}

// ---------------------------------------------------------------------------
// Proof evaluation
// ---------------------------------------------------------------------------

/// Resume point after a wrapper's internal chain is exhausted: continue with
/// the ancestors of `node` inside `comp`.
#[derive(Clone)]
struct Frame {
    comp: CompId,
    node: usize,
}

/// Recursion-invariant inputs shared by every step of one proof.
struct ProofQuery<'a> {
    linked: &'a Linked,
    world: &'a World,
    relation: Relation,
    ancestor: &'a SelectorKey,
    closed_world: bool,
}

/// Walk up from `node` inside `comp` looking for the ancestor key. At a
/// component-use ancestor, interpose the wrapper's children-slot chains; at a
/// tree root, resume via `cont` or expand every render site of `comp`.
fn prove_up(
    query: &ProofQuery<'_>,
    comp: CompId,
    node: usize,
    cont: &[Frame],
    visited: &BTreeSet<CompId>,
    depth: u32,
) -> Result<(), &'static str> {
    // ponytail: depth cap instead of full interposition-cycle detection;
    // pathological wrapper cycles bail out as recursive.
    if depth > 64 {
        return Err(R_RECURSIVE);
    }
    let comp_ir = &query.world.files[comp.0].comps[comp.1];
    let nodes = comp_ir.body.as_ref().map_err(|reason| *reason)?;
    let mut current = nodes[node].parent;
    while let Some(parent) = current {
        match &nodes[parent].kind {
            NodeKind::Element { keys } => {
                if keys.iter().any(|(key, _)| key == query.ancestor) {
                    return Ok(());
                }
                if query.relation == Relation::Child {
                    // The first element ancestor is the parent; it lacks A.
                    return Err(R_ANCESTRY);
                }
            }
            NodeKind::ComponentUse { tag, .. } => {
                let wrapper = resolve_tag(query.linked, query.world, comp.0, tag)?;
                let wrapper_ir = &query.world.files[wrapper.0].comps[wrapper.1];
                if let Err(reason) = &wrapper_ir.body {
                    return Err(reason);
                }
                if wrapper_ir.children_bad || wrapper_ir.slots.is_empty() {
                    return Err(R_BOUNDARY);
                }
                let mut inner_cont = vec![Frame { comp, node: parent }];
                inner_cont.extend_from_slice(cont);
                for &slot in &wrapper_ir.slots {
                    prove_up(query, wrapper, slot, &inner_cont, visited, depth + 1)?;
                }
                return Ok(());
            }
            NodeKind::Slot => {}
        }
        current = nodes[parent].parent;
    }
    if let Some((first, rest)) = cont.split_first() {
        return prove_up(query, first.comp, first.node, rest, visited, depth + 1);
    }
    // Render-site expansion: the relationship must hold at every site.
    if visited.contains(&comp) {
        return Err(R_RECURSIVE);
    }
    if !query.closed_world && comp_ir.exported {
        return Err(R_EXPORTED);
    }
    if let Some(reason) = query.linked.unanalyzable.get(&comp) {
        return Err(reason);
    }
    let sites = query
        .linked
        .sites
        .get(&comp)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if sites.is_empty() {
        return Err(R_ANCESTRY);
    }
    let mut expanded = visited.clone();
    expanded.insert(comp);
    for (site_comp, site_node) in sites {
        prove_up(query, *site_comp, *site_node, &[], &expanded, depth + 1)?;
    }
    Ok(())
}

/// Prove a `className={styles.B}` passed to a forwarding component: the
/// effective element is the wrapper's forward target, and its ancestry
/// continues at this invocation site.
fn prove_forward(
    query: &ProofQuery<'_>,
    comp: CompId,
    node: usize,
    tag: &TagRef,
) -> Result<(), &'static str> {
    let wrapper = resolve_tag(query.linked, query.world, comp.0, tag)?;
    let wrapper_ir = &query.world.files[wrapper.0].comps[wrapper.1];
    if let Err(reason) = &wrapper_ir.body {
        return Err(reason);
    }
    match wrapper_ir.forward {
        Forward::Target(target) => prove_up(
            query,
            wrapper,
            target,
            &[Frame { comp, node }],
            &BTreeSet::new(),
            0,
        ),
        _ => Err(R_BOUNDARY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSS: &str = "src/App.module.css";

    fn class(name: &str) -> SelectorKey {
        SelectorKey::Class(name.to_string())
    }

    fn run(
        files: &[(&str, &str)],
        relation: Relation,
        ancestor: &str,
        target: &str,
    ) -> ProofOutcome {
        prove(files, CSS, &class(ancestor), relation, &class(target))
    }

    #[test]
    fn direct_nesting_proves_child_and_descendant() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
export function App() {
  return <div className={styles.parent}><span className={styles.child} /></div>;
}
"#,
        )];
        for relation in [Relation::Child, Relation::Descendant] {
            let outcome = run(&files, relation, "parent", "child");
            assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
            assert_eq!(outcome.usages.len(), 1);
            assert!(outcome.usages[0].proven);
            assert_eq!(outcome.reason, None);
        }
    }

    #[test]
    fn deep_nesting_proves_descendant_only() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
export function App() {
  return <div className={styles.parent}><section><span className={styles.child} /></section></div>;
}
"#,
        )];
        let descendant = run(&files, Relation::Descendant, "parent", "child");
        assert!(descendant.aggregate_proven, "{descendant:?}");
        let child = run(&files, Relation::Child, "parent", "child");
        assert!(!child.aggregate_proven);
        assert_eq!(child.reason, Some("unproven-ancestry"));
    }

    #[test]
    fn local_component_proves_via_render_sites() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
function Inner() {
  return <span className={styles.child} />;
}
export function App() {
  return <div className={styles.parent}><Inner /></div>;
}
"#,
        )];
        for relation in [Relation::Child, Relation::Descendant] {
            let outcome = run(&files, relation, "parent", "child");
            assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
            assert_eq!(outcome.usages.len(), 1);
        }
    }

    #[test]
    fn extensionless_import_resolves() {
        let files = [
            (
                "src/App.tsx",
                r#"import styles from "./App.module.css";
import Title from "./Title";
export function App() {
  return <div className={styles.parent}><Title /></div>;
}
"#,
            ),
            (
                "src/Title.tsx",
                r#"import styles from "./App.module.css";
export default function Title() {
  return <h1 className={styles.child} />;
}
"#,
            ),
        ];
        for relation in [Relation::Child, Relation::Descendant] {
            let outcome = run(&files, relation, "parent", "child");
            assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
        }
    }

    #[test]
    fn unresolved_import_disqualifies() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
import Missing from "./Missing";
export function App() {
  return <div className={styles.parent}><Missing className={styles.child} /></div>;
}
"#,
        )];
        let outcome = run(&files, Relation::Descendant, "parent", "child");
        assert!(!outcome.aggregate_proven);
        assert_eq!(outcome.usages.len(), 1);
        assert_eq!(outcome.usages[0].reason, Some("unresolved-component-import"));
    }

    #[test]
    fn map_callback_counts_as_static() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
export function App({ items }) {
  return <ul className={styles.parent}>{items.map((item) => <li className={styles.child} key={item} />)}</ul>;
}
"#,
        )];
        for relation in [Relation::Child, Relation::Descendant] {
            let outcome = run(&files, relation, "parent", "child");
            assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
        }
    }

    #[test]
    fn conditional_map_callback_disqualifies() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
export function App({ items }) {
  return <ul className={styles.parent}>{items.map((item) => item.on ? <li className={styles.child} /> : null)}</ul>;
}
"#,
        )];
        let outcome = run(&files, Relation::Descendant, "parent", "child");
        assert!(!outcome.aggregate_proven);
        assert_eq!(outcome.reason, Some("dynamic-content-boundary"));
    }

    #[test]
    fn conditional_return_disqualifies() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
function Inner(props) {
  if (props.compact) {
    return <span className={styles.child} />;
  }
  return <span className={styles.child} />;
}
export function App() {
  return <div className={styles.parent}><Inner /></div>;
}
"#,
        )];
        let outcome = run(&files, Relation::Descendant, "parent", "child");
        assert!(!outcome.aggregate_proven);
        assert_eq!(outcome.usages.len(), 2);
        for usage in &outcome.usages {
            assert_eq!(usage.reason, Some("conditional-return"));
        }
        assert_eq!(outcome.reason, Some("conditional-return"));
    }

    #[test]
    fn mixed_direct_usages_report_per_usage() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
export function App() {
  return (
    <div>
      <div className={styles.parent}><span className={styles.child} /></div>
      <section><span className={styles.child} /></section>
    </div>
  );
}
"#,
        )];
        let outcome = run(&files, Relation::Descendant, "parent", "child");
        assert!(!outcome.aggregate_proven);
        assert_eq!(outcome.usages.len(), 2);
        assert!(outcome.usages[0].proven);
        assert!(!outcome.usages[1].proven);
        assert_eq!(outcome.usages[1].reason, Some("unproven-ancestry"));
    }

    #[test]
    fn all_render_sites_must_match() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
function Inner() {
  return <span className={styles.child} />;
}
export function App() {
  return (
    <div>
      <div className={styles.parent}><Inner /></div>
      <section><Inner /></section>
    </div>
  );
}
"#,
        )];
        let outcome = run(&files, Relation::Descendant, "parent", "child");
        assert!(!outcome.aggregate_proven);
        assert_eq!(outcome.usages.len(), 1);
        assert_eq!(outcome.usages[0].reason, Some("unproven-ancestry"));
    }

    #[test]
    fn class_name_forwarding_proves() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
function Btn(props) {
  return <button className={props.className} />;
}
export function App() {
  return <div className={styles.parent}><Btn className={styles.child} /></div>;
}
"#,
        )];
        for relation in [Relation::Child, Relation::Descendant] {
            let outcome = run(&files, relation, "parent", "child");
            assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
            assert_eq!(outcome.usages.len(), 1);
        }
    }

    #[test]
    fn conditional_forwarding_disqualifies() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
function Btn(props) {
  return <button className={props.solid ? props.className : ""} />;
}
export function App() {
  return <div className={styles.parent}><Btn className={styles.child} /></div>;
}
"#,
        )];
        let outcome = run(&files, Relation::Descendant, "parent", "child");
        assert!(!outcome.aggregate_proven);
        assert_eq!(outcome.usages.len(), 1);
        assert_eq!(outcome.usages[0].reason, Some("dynamic-content-boundary"));
    }

    #[test]
    fn unused_class_reports_no_usages() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
export function App() {
  return <div className={styles.parent} />;
}
"#,
        )];
        let outcome = run(&files, Relation::Descendant, "parent", "child");
        assert!(!outcome.aggregate_proven);
        assert!(outcome.usages.is_empty());
        assert_eq!(outcome.reason, Some("no-usages"));
    }

    #[test]
    fn self_recursive_component_terminates() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
function Tree() {
  return <div className={styles.node}><span className={styles.child} /><Tree /></div>;
}
export function App() {
  return <div className={styles.parent}><Tree /></div>;
}
"#,
        )];
        let outcome = run(&files, Relation::Descendant, "parent", "child");
        assert!(!outcome.aggregate_proven);
        assert_eq!(outcome.usages.len(), 1);
        assert_eq!(outcome.usages[0].reason, Some("recursive-component"));
    }

    #[test]
    fn portal_disqualifies() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
import { createPortal } from "react-dom";
function Modal(props) {
  return createPortal(<div className={styles.child} />, props.host);
}
export function App() {
  return <div className={styles.parent}><Modal /></div>;
}
"#,
        )];
        let outcome = run(&files, Relation::Descendant, "parent", "child");
        assert!(!outcome.aggregate_proven);
        assert_eq!(outcome.usages.len(), 1);
        assert_eq!(outcome.usages[0].reason, Some("portal"));
    }

    #[test]
    fn children_passthrough_proves() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
function Wrapper(props) {
  return <div className={styles.parent}>{props.children}</div>;
}
export function App() {
  return <main><Wrapper><span className={styles.child} /></Wrapper></main>;
}
"#,
        )];
        for relation in [Relation::Child, Relation::Descendant] {
            let outcome = run(&files, relation, "parent", "child");
            assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
        }
    }

    #[test]
    fn deep_children_passthrough_denies_child() {
        let files = [(
            "src/App.tsx",
            r#"import styles from "./App.module.css";
function Wrapper(props) {
  return <div className={styles.parent}><section>{props.children}</section></div>;
}
export function App() {
  return <main><Wrapper><span className={styles.child} /></Wrapper></main>;
}
"#,
        )];
        let descendant = run(&files, Relation::Descendant, "parent", "child");
        assert!(descendant.aggregate_proven, "{descendant:?}");
        let child = run(&files, Relation::Child, "parent", "child");
        assert!(!child.aggregate_proven);
        assert_eq!(child.reason, Some("unproven-ancestry"));
    }

    #[test]
    fn partial_world_gates_exported_components() {
        let files = [
            (
                "src/App.tsx",
                r#"import styles from "./App.module.css";
import Title from "./Title";
export function App() {
  return <div className={styles.parent}><Title /></div>;
}
"#,
            ),
            (
                "src/Title.tsx",
                r#"import styles from "./App.module.css";
export default function Title() {
  return <h1 className={styles.child} />;
}
"#,
            ),
        ];
        let outcome = prove_in_world(
            &files,
            CSS,
            &class("parent"),
            Relation::Descendant,
            &class("child"),
            false,
        );
        assert!(!outcome.aggregate_proven);
        assert_eq!(outcome.reason, Some("exported-render-sites-unknown"));
    }
}

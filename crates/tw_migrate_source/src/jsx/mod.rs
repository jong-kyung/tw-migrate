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

use tw_migrate_css::{Relation, SelectorKey};

use collect::extract_file;
use link::{Linked, link};
use prove::{ProofQuery, prove_forward, prove_up};

mod collect;
mod component;
mod link;
mod prove;
#[cfg(test)]
mod tests;

#[derive(Debug)]
pub struct UsageProof {
    pub file: String,
    pub span: (usize, usize),
    pub proven: bool,
    pub reason: Option<&'static str>,
}

#[derive(Debug)]
pub struct ProofOutcome {
    pub usages: Vec<UsageProof>,
    pub aggregate_proven: bool,
    pub reason: Option<&'static str>,
}

const R_CONDITIONAL: &str = "conditional-return";
const R_PORTAL: &str = "portal";
const R_HOC: &str = "hoc-or-dynamic-component";
const R_RECURSIVE: &str = "recursive-component";
const R_UNRESOLVED: &str = "unresolved-component-import";
const R_BOUNDARY: &str = "dynamic-content-boundary";
const R_ANCESTRY: &str = "unproven-ancestry";
const R_NO_USAGES: &str = "no-usages";

/// The extracted-and-linked world for one (files, css_path) pair: the
/// query-invariant part of a proof, reusable across many queries.
pub struct PreparedWorld {
    world: World,
    linked: Linked,
}

/// Build the [`PreparedWorld`] (per-file extraction plus cross-file linking).
pub fn prepare(files: &[(&str, &str)], css_path: &str) -> PreparedWorld {
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
pub fn prove_prepared(
    prepared: &PreparedWorld,
    ancestor: &SelectorKey,
    relation: Relation,
    target: &SelectorKey,
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
                    NodeKind::Element { keys, .. } => {
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
                                let result =
                                    prove_forward(&query, (file_ix, comp_ix), node_ix, tag);
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
    /// Host element with its statically known CSS Module keys. `tainted`
    /// marks a spread attribute: runtime props may override `className`, so
    /// the keys remain valid target usages but never witness an ancestor.
    Element {
        keys: Vec<(SelectorKey, (usize, usize))>,
        tainted: bool,
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

use std::collections::BTreeSet;

use tw_migrate_css::{Relation, SelectorKey};

use super::link::{Linked, resolve_tag};
use super::{CompId, Forward, NodeKind, R_ANCESTRY, R_BOUNDARY, R_RECURSIVE, TagRef, World};

#[derive(Clone)]
pub(super) struct Frame {
    comp: CompId,
    node: usize,
}

/// Recursion-invariant inputs shared by every step of one proof.
pub(super) struct ProofQuery<'a> {
    pub(super) linked: &'a Linked,
    pub(super) world: &'a World,
    pub(super) relation: Relation,
    pub(super) ancestor: &'a SelectorKey,
}

/// Walk up from `node` inside `comp` looking for the ancestor key. At a
/// component-use ancestor, interpose the wrapper's children-slot chains; at a
/// tree root, resume via `cont` or expand every render site of `comp`.
pub(super) fn prove_up(
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
            NodeKind::Element { keys, tainted } => {
                if keys.iter().any(|(key, _)| key == query.ancestor) {
                    if *tainted {
                        // A spread on the witness could drop the ancestor
                        // class at runtime.
                        return Err(R_BOUNDARY);
                    }
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
pub(super) fn prove_forward(
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

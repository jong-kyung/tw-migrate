use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use super::{CompId, ImportedName, NodeKind, R_HOC, R_UNRESOLVED, TagRef, World};
use crate::{normalize_path, resolve_import};

/// Per-file map of import local name to its resolved (file, exported name),
/// or the disqualification reason.
type ResolvedImports = HashMap<String, Result<(usize, ImportedName), &'static str>>;

pub(super) struct Linked {
    imports: Vec<ResolvedImports>,
    pub(super) unanalyzable: HashMap<CompId, &'static str>,
    pub(super) sites: HashMap<CompId, Vec<(CompId, usize)>>,
}

const PROBE_EXTENSIONS: [&str; 6] = ["tsx", "ts", "jsx", "js", "mjs", "cjs"];

/// Resolve an extensionless relative specifier against the request file set:
/// exact path, then each extension, then `index.*` under the path. Every
/// match is returned; zero or multiple matches disqualify at the call site.
fn resolve_specifier(
    path_index: &HashMap<PathBuf, usize>,
    from: &str,
    specifier: &str,
) -> Vec<usize> {
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
    probes
        .into_iter()
        .filter_map(|probe| path_index.get(&probe).copied())
        .collect()
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

pub(super) fn link(world: &World) -> Linked {
    let path_index: HashMap<PathBuf, usize> = world
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| (normalize_path(Path::new(&file.path)), index))
        .collect();
    let mut unanalyzable: HashMap<CompId, &'static str> = HashMap::new();
    let mut imports = Vec::new();
    for file in &world.files {
        let mut resolved = ResolvedImports::new();
        for (local, (specifier, imported)) in &file.imports {
            let matches = resolve_specifier(&path_index, &file.path, specifier);
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
            for candidate in resolve_specifier(&path_index, &file.path, specifier) {
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

pub(super) fn resolve_tag(
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
            comp.map(|comp_ix| (*target_file, comp_ix))
                .ok_or(R_UNRESOLVED)
        }
        TagRef::Unknown => Err(R_HOC),
    }
}

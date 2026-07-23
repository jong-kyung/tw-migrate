use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const EXPECTED_BASELINE: usize = 137;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inventory {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    source: String,
    disposition: String,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[test]
fn inventory_is_unique_complete_and_resolves_e2e_targets() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let coverage_root = crate_root.join("coverage");
    let mut files = fs::read_dir(&coverage_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect::<Vec<_>>();
    files.sort();

    let snapshot_names = fs::read_dir(crate_root.join("snapshots"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut sources = BTreeMap::<String, String>::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut errors = Vec::new();

    for path in files {
        let inventory: Inventory = toml::from_str(&fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        for case in inventory.cases {
            let shard = path.file_name().unwrap().to_string_lossy().into_owned();
            if let Some(previous) = sources.insert(case.source.clone(), shard.clone()) {
                errors.push(format!(
                    "duplicate source {:?} in {previous} and {shard}",
                    case.source
                ));
            }
            *counts.entry(case.disposition.clone()).or_default() += 1;
            if !matches!(
                case.disposition.as_str(),
                "e2e" | "retained" | "platform-limited"
            ) {
                errors.push(format!(
                    "invalid disposition {:?} for {:?}",
                    case.disposition, case.source
                ));
            }
            if case.disposition == "retained" {
                if case.target.is_some() {
                    errors.push(format!("retained case {:?} has a target", case.source));
                }
                continue;
            }
            let Some(target) = case.target else {
                errors.push(format!(
                    "{} case {:?} has no target",
                    case.disposition, case.source
                ));
                continue;
            };
            if !crate_root
                .join("fixtures")
                .join(&target)
                .join("case.toml")
                .is_file()
            {
                errors.push(format!(
                    "case {:?} has no fixture target {target:?}",
                    case.source
                ));
            }
            let suffix = format!("__{target}.snap");
            if !snapshot_names.iter().any(|name| name.ends_with(&suffix)) {
                errors.push(format!(
                    "case {:?} has no snapshot target {target:?}",
                    case.source
                ));
            }
            let _ = case.notes;
        }
    }

    assert_eq!(
        sources.len(),
        EXPECTED_BASELINE,
        "inventory errors: {errors:#?}"
    );
    assert_eq!(
        counts,
        BTreeMap::from([
            ("e2e".to_string(), 133),
            ("platform-limited".to_string(), 2),
            ("retained".to_string(), 2),
        ])
    );
    assert!(errors.is_empty(), "inventory errors: {errors:#?}");
}

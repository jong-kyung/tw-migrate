use super::*;

fn plan_value(plan: SourcePlan) -> serde_json::Value {
    let edits = |edits: Vec<Edit>| {
        edits
            .into_iter()
            .map(|edit| (edit.start, edit.end, edit.replacement))
            .collect::<Vec<_>>()
    };
    serde_json::json!({
        "edits": edits(plan.edits),
        "removableImports": edits(plan.removable_import_edits),
        "candidates": plan.candidates,
        "matches": plan.matches.into_iter().map(|matched| (
            matched.start, matched.end, format!("{:?}", matched.key),
            matched.candidate, matched.origin_candidate,
        )).collect::<Vec<_>>(),
        "moduleRefs": plan.module_refs,
        "matchedModuleRefs": plan.matched_module_refs,
        "moduleReferencesSafe": plan.module_references_safe,
        "unreferencedModule": plan.unreferenced_module,
        "warnings": plan.warnings,
    })
}

#[test]
fn module_non_consumer_proof_preserves_references_and_diagnostics() {
    for (source, unreferenced) in [
        ("export const App = () => <div className=\"a\" />;", true),
        (
            "import other from './Other.module.css'; export const App = () => <div className={other.a} />;",
            true,
        ),
        ("import a from './A.module.css';", false),
        ("import './A.module.css';", false),
        ("export { default } from './A.module.css';", false),
        ("export * from './A.module.css';", false),
        ("const a = require('./A.module.css');", false),
        ("const a = import('./A.module.css');", false),
        (
            "export const App = ({ value }) => <div className={value} />;",
            false,
        ),
    ] {
        let file = serde_json::from_value::<SourceFile>(serde_json::json!({
            "path": "/project/App.tsx", "source": source,
        }))
        .unwrap();
        with_source_file(&file, |planner| {
            for is_module in [true, false] {
                let plan = planner
                    .plan(
                        "/project/A.module.css",
                        is_module,
                        &HashMap::from([(SelectorKey::Class("a".into()), vec!["p-[13px]".into()])]),
                        &HashMap::new(),
                        &BTreeSet::new(),
                    )
                    .unwrap();
                assert_eq!(
                    plan.unreferenced_module,
                    is_module && unreferenced,
                    "{source}"
                );
            }
        });
    }
}

#[test]
fn import_inventory_keeps_normalized_targets_and_reference_kinds_separate() {
    let file: SourceFile = serde_json::from_value(serde_json::json!({
        "path": "/project/App.tsx",
        "source": "import a from './nested/../A.module.css';\nimport again from './A.module.css';\nimport './Side.module.css';\nexport { default } from './Export.module.css';\nexport * from './Star.module.css';\nfunction load() { require('./Required.module.css'); return import('./Dynamic.module.css'); }\nexport const App = () => <div className={`${a.a} ${again.a}`} />;",
    }))
    .unwrap();
    let candidates = HashMap::from([(SelectorKey::Class("a".into()), vec!["p-[8px]".into()])]);
    with_source_file(&file, |planner| {
        for name in [
            "Missing", "A", "Side", "Export", "Star", "Required", "Dynamic", "Missing", "A",
        ] {
            let plan = planner
                .plan(
                    &format!("/project/{name}.module.css"),
                    true,
                    &candidates,
                    &HashMap::new(),
                    &BTreeSet::new(),
                )
                .unwrap();
            assert_eq!(
                plan.module_references_safe,
                matches!(name, "A" | "Missing"),
                "{name}"
            );
            assert_eq!(plan.unreferenced_module, name == "Missing", "{name}");
            if name == "A" {
                assert_eq!(plan.module_refs.get("a"), Some(&2));
                assert_eq!(plan.removable_import_edits.len(), 2);
                assert!(plan.warnings.is_empty());
            } else if name != "Missing" {
                assert_eq!(plan.warnings.len(), 1, "{name}");
                assert_eq!(plan.warnings[0].code, "unsupported-css-module-reference");
            }
        }
    });
}

#[test]
fn non_consumer_proof_reuses_only_warning_free_unreferenced_module_plans() {
    let sheets = [
        ("Missing.module.css", true, false),
        ("A.module.css", true, false),
        ("Other.module.css", true, true),
        ("global.css", false, false),
        ("A.module.css", true, true),
        ("Side.module.css", true, false),
        ("Missing.module.css", true, false),
    ];
    let mut file: SourceFile = serde_json::from_value(serde_json::json!({
        "path": "/project/App.tsx",
        "source": "// 가\r\nimport a from './A.module.css';\r\nimport './Side.module.css';\r\nexport const App = ({ flag }) => <div className={flag ? a.a : 'global'} />;\r\n",
    }))
    .unwrap();
    for dynamic in [false, true] {
        if dynamic {
            file.source
                .push_str("export const Other = ({ value }) => <div className={value} />;");
        }
        let plan = |source: &SourceFilePlanner<'_>, index: usize| {
            let (name, module, preserve) = sheets[index];
            let (utility, property) = if preserve {
                ("m-[7px]", "margin")
            } else {
                ("p-[13px]", "padding")
            };
            let candidates = HashMap::from([
                (SelectorKey::Class("a".into()), vec![utility.into()]),
                (
                    SelectorKey::Class("global".into()),
                    vec!["text-[red]".into()],
                ),
            ]);
            let properties = HashMap::from([(utility.into(), BTreeSet::from([property.into()]))]);
            let mut preserved = BTreeSet::new();
            if preserve {
                preserved.insert("a".into());
            }
            plan_value(
                source
                    .plan(
                        &format!("/project/{name}"),
                        module,
                        &candidates,
                        &properties,
                        &preserved,
                    )
                    .unwrap(),
            )
        };
        let before = USAGE_VISITS.get();
        let expected = (0..sheets.len())
            .map(|index| with_source_file(&file, |source| plan(source, index)))
            .collect::<Vec<_>>();
        assert_eq!(USAGE_VISITS.get() - before, 7);
        assert_eq!(expected[0]["unreferencedModule"], !dynamic);
        assert_eq!(expected[1]["moduleRefs"]["a"], 1);
        assert!(!expected[5]["moduleReferencesSafe"].as_bool().unwrap());

        for _ in 0..2 {
            let before = USAGE_VISITS.get();
            with_source_file(&file, |source| {
                for (index, expected) in expected.iter().enumerate() {
                    assert_eq!(plan(source, index), *expected);
                }
            });
            assert_eq!(USAGE_VISITS.get() - before, if dynamic { 7 } else { 5 });
        }
    }
}

#[test]
fn file_scope_is_lazy_and_keeps_cached_errors_stylesheet_specific() {
    let candidates = HashMap::new();
    let properties = HashMap::new();
    let preserved = BTreeSet::new();
    for (path, source, parses, semantics) in [
        ("/project/App.tsx", "const = /* A.module.css */", 1, 0),
        ("/project/App.tsx", "let x; let x; /* A.module.css */", 1, 1),
        ("/project/App.unknown", "A.module.css", 0, 0),
    ] {
        for writable in [false, true] {
            let file: SourceFile = serde_json::from_value(serde_json::json!({
                "path": path, "source": source, "writable": writable,
            }))
            .unwrap();
            let before = SOURCE_BUILDS.get();
            with_source_file(&file, |planner| {
                assert_eq!(planner.file().source, source);
            });
            assert_eq!(SOURCE_BUILDS.get(), before);

            with_source_file(&file, |planner| {
                for css_path in ["/project/A.module.css", "/project/B.module.css"] {
                    let actual = planner.plan(css_path, true, &candidates, &properties, &preserved);
                    if path.ends_with(".unknown") {
                        assert!(matches!(
                            actual,
                            Err(MigrationError::UnsupportedSource { .. })
                        ));
                    } else if writable {
                        assert!(
                            matches!(actual, Err(MigrationError::SourceParse { .. }))
                                && semantics == 0
                                || matches!(actual, Err(MigrationError::SourceAnalysis { .. }))
                                    && semantics == 1
                        );
                    } else {
                        let actual = actual.unwrap();
                        assert_eq!(
                            actual.module_references_safe,
                            css_path.ends_with("B.module.css")
                        );
                        assert_eq!(
                            actual.warnings.len(),
                            usize::from(css_path.ends_with("A.module.css"))
                        );
                    }
                }
            });
            assert_eq!(
                SOURCE_BUILDS.get(),
                (before.0 + parses, before.1 + semantics)
            );
        }
    }
}

#[test]
fn file_scope_reuses_parse_and_semantic_without_changing_stylesheet_plans() {
    let sheets = [
        ("/project/A.module.css", true, "a", "p-[8px]"),
        ("/project/B.module.css", true, "b", "text-[red]"),
        ("/project/global.css", false, "global", "content-['\"x\"']"),
    ]
    .map(|(path, module, class, candidate)| {
        (
            path,
            module,
            HashMap::from([(SelectorKey::Class(class.into()), vec![candidate.into()])]),
        )
    });
    let files = ["App.tsx", "Other.jsx"].map(|name| {
        serde_json::from_value::<SourceFile>(serde_json::json!({
            "path": format!("/project/{name}"),
            "source": "// 가\r\nimport a from './A.module.css';\r\nimport b from './B.module.css';\r\nconst alias = a.a;\r\nexport const App = ({ flag, key }) => <>\r\n<div className={`${a.a} ${b.b}`} />\r\n<div className={flag ? a.a : b.b} />\r\n<div className={b[key]} />\r\n<div className=\"global\" />\r\n</>;\r\n",
        }))
        .unwrap()
    });
    let properties = HashMap::new();
    let preserved = BTreeSet::new();
    let before = SOURCE_BUILDS.get();
    let imports_before = IMPORT_BUILDS.get();
    let expected = files
        .iter()
        .map(|file| {
            sheets
                .iter()
                .map(|(path, module, candidates)| {
                    plan_value(
                        plan_batch_source_file(
                            file,
                            path,
                            *module,
                            candidates,
                            &properties,
                            &preserved,
                        )
                        .unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(SOURCE_BUILDS.get(), (before.0 + 6, before.1 + 6));
    assert_eq!(IMPORT_BUILDS.get(), imports_before + 4);
    assert!(!expected[0][0]["warnings"].as_array().unwrap().is_empty());
    assert!(!expected[0][0]["matches"].as_array().unwrap().is_empty());

    // A new scope must analyze again even for the same path and source.
    for _ in 0..2 {
        let before = SOURCE_BUILDS.get();
        let imports_before = IMPORT_BUILDS.get();
        for (file, expected) in files.iter().zip(&expected) {
            with_source_file(file, |source| {
                for ((path, module, candidates), expected) in sheets.iter().zip(expected) {
                    let actual = source
                        .plan(path, *module, candidates, &properties, &preserved)
                        .unwrap();
                    assert_eq!(plan_value(actual), *expected);
                }
            });
        }
        assert_eq!(SOURCE_BUILDS.get(), (before.0 + 2, before.1 + 2));
        assert_eq!(IMPORT_BUILDS.get(), imports_before + 2);
    }
}

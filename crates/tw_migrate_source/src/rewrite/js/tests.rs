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
        "warnings": plan.warnings,
    })
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
    assert!(!expected[0][0]["warnings"].as_array().unwrap().is_empty());
    assert!(!expected[0][0]["matches"].as_array().unwrap().is_empty());

    // A new scope must analyze again even for the same path and source.
    for _ in 0..2 {
        let before = SOURCE_BUILDS.get();
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
    }
}

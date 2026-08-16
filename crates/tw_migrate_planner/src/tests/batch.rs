use super::*;

#[test]
fn batch_ignores_an_unparseable_unwritable_file_without_a_reference() {
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n"
        }],
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }, {
            "path": "/project/coverage.js",
            "source": "<% generated: mentions other.module.css but is not JavaScript %>\n",
            "writable": false
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Card.module.css"])
    );
}

#[test]
fn batch_retains_a_module_named_by_an_unparseable_unwritable_file() {
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n"
        }],
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }, {
            "path": "/project/generated.js",
            "source": "<% template referencing Card.module.css %>\n",
            "writable": false
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |warning| warning["code"] == "unsupported-css-module-reference"
                    && warning["file"] == "/project/generated.js"
            )
    );
}

#[test]
fn batch_updates_distinct_module_references_without_losing_edits() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { padding: 13px; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { color: red; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <><div className={a.a} /><div className={b.b} /></>;\n"
        }]
    });

    let response = plan_batch(request);
    let source = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/App.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();

    assert_eq!(
        source,
        "export const App = () => <><div className=\"p-[13px]\" /><div className=\"text-[red]\" /></>;\n"
    );
    assert_eq!(response["convertedRules"], 2);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/A.module.css", "/project/B.module.css"])
    );
}

#[test]
fn batch_migrates_members_from_multiple_modules_in_one_template() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { padding: 13px; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { color: red; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);
    let source = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/App.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();

    assert_eq!(
        source,
        "export const App = () => <div className=\"p-[13px] text-[red]\" />;\n"
    );
    assert_eq!(response["convertedRules"], 2);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/A.module.css", "/project/B.module.css"])
    );
}

#[test]
fn batch_blocked_rules_are_retained_silently_and_reports_carry_rule_ids() {
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/Button.module.css",
            "cssSource": ".bad { color: red; }\n.good { padding: 13px; }\n",
            "blockedRules": [{ "start": 0, "end": 20 }]
        }],
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.bad}><i className={styles.good} /></button>;\n"
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    // The blocked rule is retained without a Rust-side warning: the
    // caller attributes candidate-compilation-failure itself.
    assert_eq!(response["warnings"], serde_json::json!([]));
    let rules = response["rules"].as_array().unwrap();
    let blocked = rules
        .iter()
        .find(|rule| rule["selector"] == ".bad")
        .unwrap();
    assert_eq!(blocked["status"], "retained");
    assert_eq!(blocked["file"], "/project/Button.module.css");
    assert_eq!(
        blocked["ruleId"],
        serde_json::json!({ "start": 0, "end": 20 })
    );
    assert_eq!(
        blocked["authoredSpan"],
        serde_json::json!({ "start": 0, "end": 20 })
    );
    let converted = rules
        .iter()
        .find(|rule| rule["selector"] == ".good")
        .unwrap();
    assert_eq!(converted["status"], "converted");
    let source = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/Button.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    assert!(source.contains("styles.bad"));
    assert!(source.contains("\"p-[13px]\""));
}

#[test]
fn batch_excludes_blocked_rules_from_cross_stylesheet_conflicts() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { padding: 8px; }\n",
                "blockedRules": [{ "start": 0, "end": 20 }]
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { padding: 16px; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);

    // The blocked rule's candidates never apply, so they must not create
    // a batch-stylesheet-conflict; the healthy sibling converts and the
    // blocked rule stays silently retained (the caller attributes
    // candidate-compilation-failure itself).
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(response["warnings"], serde_json::json!([]));
    let rules = response["rules"].as_array().unwrap();
    let blocked = rules.iter().find(|rule| rule["selector"] == ".a").unwrap();
    assert_eq!(blocked["status"], "retained");
    let converted = rules.iter().find(|rule| rule["selector"] == ".b").unwrap();
    assert_eq!(converted["status"], "converted");
}

#[test]
fn batch_retains_conflicting_members_from_multiple_modules_in_one_template() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { padding: 8px; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { padding: 16px; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    assert_eq!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
            .count(),
        2
    );
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] != "dynamic-class-name")
    );
}

#[test]
fn batch_allows_opposite_branch_utilities_from_different_modules() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { padding: 8px; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { padding: 16px; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = ({ maybe }) => <div className={maybe ? a.a : b.b} />;\n"
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 2);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/A.module.css", "/project/B.module.css"])
    );
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] != "batch-stylesheet-conflict")
    );
    assert_eq!(
        response["files"][0]["source"],
        "export const App = ({ maybe }) => <div className={maybe ? \"p-[8px]\" : \"p-[16px]\"} />;\n"
    );
}

#[test]
fn batch_retains_same_css_property_even_when_tailwind_prefix_is_ambiguous() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { color: red; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { color: blue; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    assert_eq!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
            .count(),
        2
    );
}

#[test]
fn batch_does_not_conflict_color_with_font_size() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { color: red; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { font-size: 13px; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);
    let source = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/App.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();

    assert_eq!(
        source,
        "export const App = () => <div className=\"text-[red] text-[13px]\" />;\n"
    );
    assert_eq!(response["convertedRules"], 2);
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] != "batch-stylesheet-conflict")
    );
}

#[test]
fn batch_keeps_dynamic_template_warnings() {
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/A.module.css",
            "cssSource": ".a { padding: 8px; }\n"
        }],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nexport const App = ({ active }) => <div className={`${a.a} ${active ? 'on' : 'off'}`} />;\n"
        }]
    });

    let response = plan_batch(request);

    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "dynamic-class-name")
    );
}

#[test]
fn batch_converts_a_rule_unrelated_to_a_dynamic_class_name() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".blocked { color: red; }\n.safe { padding: 13px; }\n",
        "isModule": true,
        "files": [{
            "path": "/project/App.tsx",
            "source": "import styles from './Card.module.css';\nexport const App = ({ active }) => <>\n  <div className={getClass(active, styles.blocked)} />\n  <div className={styles.safe} />\n</>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "dynamic-class-name")
    );
}

#[test]
fn batch_rebases_source_warnings_after_prior_stylesheet_edits() {
    let source = "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = ({ active }) => <>\n  <div className={a.a} />\n  <div className={pickClass(active, b.b)} />\n</>;\n";
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/A.module.css",
            "cssSource": ".a { padding: 13px; }\n",
            "isModule": true
        }, {
            "cssPath": "/project/B.module.css",
            "cssSource": ".b { color: red; }\n",
            "isModule": true
        }],
        "files": [{
            "path": "/project/App.tsx",
            "source": source
        }]
    });

    let response = plan_batch(request);
    let warnings = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning["code"] == "dynamic-class-name")
        .collect::<Vec<_>>();
    let start = source.find("{pickClass(active, b.b)}").unwrap();

    assert!(!warnings.is_empty());
    for warning in warnings {
        assert_eq!(warning["start"], start);
        assert_eq!(warning["end"], start + "{pickClass(active, b.b)}".len());
    }
}

#[test]
fn batch_blocks_only_the_conflicting_rule_for_a_shared_selector_key() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/a.css",
                "cssSource": ".a { padding: 8px; }\n.a:hover { color: red; }\n"
            },
            {
                "cssPath": "/project/b.css",
                "cssSource": ".b { padding: 16px; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "export const App = () => <div className=\"a b\" />;\n"
        }]
    });

    let response = plan_batch(request);
    let source = response["files"][0]["source"].as_str().unwrap();

    assert_eq!(
        source,
        "export const App = () => <div className=\"a b hover:text-[red]\" />;\n"
    );
    assert_eq!(
        response["candidates"],
        serde_json::json!(["hover:text-[red]"])
    );
    assert_eq!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
            .count(),
        2
    );
}

#[test]
fn batch_preserves_a_direct_module_member_when_appending_independent_candidates() {
    let file = SourceFile {
        path: "/project/App.tsx".to_string(),
        source: "import styles from './A.module.css';\nexport const App = () => <div className={styles.a} />;\n".to_string(),
        writable: true,
        html_elements: Vec::new(),
        html_stylesheets: Vec::new(),
        html_references_safe: true,
        html_script_text: String::new(),
        prior_edits: Vec::new(),
    };
    let candidates = HashMap::from([(
        SelectorKey::Class("a".to_string()),
        vec!["hover:text-[red]".to_string()],
    )]);
    let preserved = BTreeSet::from(["a".to_string()]);

    let plan = plan_batch_source_file(
        &file,
        "/project/A.module.css",
        true,
        &candidates,
        &HashMap::new(),
        &preserved,
    )
    .unwrap();
    let source = apply_edits(&file.source, plan.edits).unwrap();

    assert_eq!(
        source,
        "import styles from './A.module.css';\nexport const App = () => <div className={`${styles.a}${\" hover:text-[red]\"}`} />;\n"
    );
    assert_eq!(plan.matched_module_refs.get("a"), Some(&1));
}

#[test]
fn batch_retains_arbitrary_border_shorthand_conflicts() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { border: 1px solid red; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { border-color: blue; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    assert_eq!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
            .count(),
        2
    );
}

#[test]
fn batch_retains_mask_shorthand_conflicts() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { mask: url(a.svg); }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { mask-image: url(b.svg); }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
}

#[test]
fn batch_retains_all_reset_conflicts() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { all: unset; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { color: blue; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    assert_eq!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
            .count(),
        2
    );
}

#[test]
fn all_reset_excludes_css_wide_exceptions() {
    assert!(!css_properties_conflict("all", "--theme-color"));
    assert!(!css_properties_conflict("all", "direction"));
    assert!(!css_properties_conflict("all", "unicode-bidi"));
    assert!(css_properties_conflict("all", "color"));
}

#[test]
fn batch_retains_grid_shorthand_conflicts() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { grid: auto / 1fr; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { grid-template-columns: 2fr; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    assert_eq!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
            .count(),
        2
    );
}

#[test]
fn batch_does_not_conflict_unrelated_border_radius_and_color() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { border-radius: 13px; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { border-color: blue; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);
    let app = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/App.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();

    assert_eq!(
        app,
        "export const App = () => <div className=\"rounded-[13px] border-[blue]\" />;\n"
    );
    assert_eq!(response["convertedRules"], 2);
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] != "batch-stylesheet-conflict")
    );
}

#[test]
fn batch_converts_independent_module_rules_while_preserving_conflicting_members() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { padding: 8px; }\n.a:hover { color: red; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { padding: 16px; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);
    let app = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/App.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    let css = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/A.module.css")
        .unwrap()["source"]
        .as_str()
        .unwrap();

    assert_eq!(
        app,
        "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}${\" hover:text-[red]\"}`} />;\n"
    );
    assert_eq!(css, ".a { padding: 8px; }\n\n");
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 2);
    assert_eq!(
        response["candidates"],
        serde_json::json!(["hover:text-[red]"])
    );
}

#[test]
fn batch_converts_a_different_module_class_when_one_class_conflicts() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { padding: 8px; }\n.c { color: red; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { padding: 16px; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <><div className={`${a.a} ${b.b}`} /><div className={a.c} /></>;\n"
        }]
    });

    let response = plan_batch(request);
    let app = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/App.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    let css = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/A.module.css")
        .unwrap()["source"]
        .as_str()
        .unwrap();

    assert_eq!(
        app,
        "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <><div className={`${a.a} ${b.b}`} /><div className=\"text-[red]\" /></>;\n"
    );
    assert_eq!(css, ".a { padding: 8px; }\n\n");
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 2);
    assert_eq!(response["candidates"], serde_json::json!(["text-[red]"]));
}

#[test]
fn batch_retains_cross_stylesheet_conflicts() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/a.css",
                "cssSource": ".a { padding: 8px; }\n"
            },
            {
                "cssPath": "/project/b.css",
                "cssSource": ".b { padding: 16px; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "export const App = () => <div className=\"a b\" />;\n"
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    let conflict_files = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
        .map(|warning| warning["file"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        conflict_files,
        BTreeSet::from(["/project/a.css", "/project/b.css"])
    );
    for warning in response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
    {
        let message = warning["message"].as_str().unwrap();
        assert!(message.contains("p-[8px]"));
        assert!(message.contains("p-[16px]"));
        assert!(message.contains("conflict"));
    }
}

#[test]
fn batch_uses_candidate_specific_properties_for_font_size_conflicts() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": "@supports (display: grid) { .a { color: red; font-size: 12px; } }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": "@supports (display: grid) { .b { font-size: 13px; } }\n"
            }
        ],
        "utilityPrefix": "tw",
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);
    let messages = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
        .map(|warning| warning["message"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .all(|message| message.contains("tw:supports-[display:grid]:text-[12px]"))
    );
    assert!(
        messages
            .iter()
            .all(|message| message.contains("tw:supports-[display:grid]:text-[13px]"))
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.contains("text-[red]"))
    );
}

#[test]
fn batch_uses_candidate_specific_properties_for_color_conflicts() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { color: red; font-size: 12px; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { color: blue; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);
    let messages = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
        .map(|warning| warning["message"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .all(|message| message.contains("text-[red]"))
    );
    assert!(
        messages
            .iter()
            .all(|message| message.contains("text-[blue]"))
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.contains("text-[12px]"))
    );
}

#[test]
fn batch_merges_properties_when_candidates_deduplicate() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { color: var(--value); font-size: var(--value); }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".b { color: blue; }\n"
            },
            {
                "cssPath": "/project/C.module.css",
                "cssSource": ".c { font-size: 13px; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nimport c from './C.module.css';\nexport const App = () => <div className={`${a.a} ${b.b} ${c.c}`} />;\n"
        }]
    });

    let response = plan_batch(request);
    let message = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| {
            warning["code"] == "batch-stylesheet-conflict"
                && warning["file"] == "/project/A.module.css"
        })
        .unwrap()["message"]
        .as_str()
        .unwrap();

    assert!(message.contains("text-[var(--value)]"));
    assert!(message.contains("text-[blue]"));
    assert!(message.contains("text-[13px]"));
}

#[test]
fn batch_combines_tailwind_entry_additions() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n.a { animation: fade 1s; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": "@keyframes spin { from { rotate: 0deg; } to { rotate: 360deg; } }\n.b { animation: spin 1s; }\n"
            }
        ],
        "tailwindPath": "/project/globals.css",
        "tailwindSource": "@import \"tailwindcss\";\n",
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <><div className={a.a} /><div className={b.b} /></>;\n"
        }]
    });

    let response = plan_batch(request);
    let tailwind = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/globals.css")
        .unwrap()["source"]
        .as_str()
        .unwrap();

    assert_eq!(tailwind.matches("@keyframes tw-migrate-").count(), 2);
}

#[test]
fn batch_reference_only_consumer_prevents_module_deletion() {
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/shared/Button.module.css",
            "cssSource": ".button { padding: 13px; }\n"
        }],
        "files": [{
            "path": "/project/app/Button.tsx",
            "source": "import styles from '../shared/Button.module.css';\nexport const Button = () => <button className={styles.button} />;\n",
            "writable": false
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "reference-only-css-module-consumer")
    );
}

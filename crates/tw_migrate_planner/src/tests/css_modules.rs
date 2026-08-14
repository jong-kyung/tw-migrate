use super::*;

#[test]
fn converts_a_global_descendant_selector_to_an_arbitrary_variant() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".menu_open .child { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = () => <span className=\"child\" />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["[.menu\\_open_&]:p-[13px]"])
    );
    assert_eq!(
        response["files"][0]["source"],
        "export const Card = () => <span className=\"child [.menu\\_open_&]:p-[13px]\" />;\n"
    );
}

#[test]
fn retains_a_css_module_class_referenced_by_composes() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n.featured { composes: card; color: red; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "css-module-composes")
    );
}

#[test]
fn normalizes_spacing_shorthand_before_mapping() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { margin: 1rem; margin-left: 2rem; }\n",
        "themeTokens": { "spacing": "0.25rem" },
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["mb-4", "ml-8", "mr-4", "mt-4"])
    );
}

#[test]
fn preserves_functional_spacing_values() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { margin: calc(100% - 1rem); padding: var(--space, 1rem); }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["m-[calc(100%_-_1rem)]", "p-[var(--space,_1rem)]"])
    );
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn prefers_an_exact_custom_theme_token() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "themeTokens": { "spacing-card": "13px" },
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["candidates"], serde_json::json!(["p-card"]));
}

#[test]
fn converts_a_supported_pseudo_class_to_a_variant() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card:hover { color: red; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["hover:text-[red]"])
    );
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn adds_a_class_name_for_a_global_id() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": "#hero { height: 100vh; }\n",
        "files": [{
            "path": "/project/Hero.tsx",
            "source": "export const Hero = () => <main id=\"hero\" />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Hero = () => <main id=\"hero\" className=\"h-[100vh]\" />;\n"
    );
    assert_eq!(response["warnings"][0]["code"], "retained-global-rule");
}

#[test]
fn migrates_a_static_css_module_template() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`${styles.card} featured`} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Card = () => <div className=\"p-[13px] featured\" />;\n"
    );
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["warnings"], serde_json::json!([]));
}

#[test]
fn distinguishes_overlapping_tailwind_properties() {
    assert!(tailwind_utilities_conflict("p-[13px]", "pl-2"));
    assert!(!tailwind_utilities_conflict("ps-2", "pe-2"));
    assert!(!tailwind_utilities_conflict("rounded-t-lg", "rounded-b-lg"));
    assert!(!tailwind_utilities_conflict("text-sm", "text-red-500"));
}

#[test]
fn warns_when_a_static_template_utility_conflicts() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`${styles.card} p-2`} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(
        response["warnings"][0]["code"],
        "existing-tailwind-conflict"
    );
    assert_eq!(response["warnings"][0]["file"], "/project/Card.tsx");
}

#[test]
fn retains_a_rule_used_through_another_import_alias() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import first from './Card.module.css';\nimport second from './Card.module.css';\nconst card = first.card;\nexport const Card = () => <div className={second.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    assert!(
        response["files"][0]["source"]
            .as_str()
            .unwrap()
            .contains("first.card")
    );
}

#[test]
fn converts_references_through_every_import_alias() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import first from './Card.module.css';\nimport second from './Card.module.css';\nexport const Card = () => <><div className={first.card} /><div className={second.card} /></>;\n"
        }]
    });

    let response = plan(request);
    let source = response["files"][0]["source"].as_str().unwrap();

    assert_eq!(response["convertedRules"], 1);
    assert!(!source.contains("import "));
    assert!(!source.contains(".card"));
}

#[test]
fn retains_a_module_with_an_unclassified_import_reference() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import first from './Card.module.css';\nimport second from './Card.module.css';\nconst card = first['card'];\nexport const Card = () => <div className={second.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| { warning["code"] == "computed-css-module-reference" })
    );
}

#[test]
fn warns_at_the_computed_css_module_reference_site() {
    let source = "import styles from './Card.module.css';\nexport const name = styles['card'];\n";
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": source
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    let start = source.find("styles['card']").unwrap();
    let warning = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["code"] == "computed-css-module-reference")
        .expect("computed reference warning");
    assert_eq!(warning["file"], "/project/Card.tsx");
    assert_eq!(warning["start"], start);
    assert_eq!(warning["end"], start + "styles['card']".len());
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] != "unsupported-css-module-reference")
    );
}

#[test]
fn warns_at_an_aliased_css_module_reference_site() {
    let source = "import styles from './Card.module.css';\nconst card = styles.card;\nexport const Card = () => <div className={styles.button} />;\n";
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n.button { color: red; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": source
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    let start = source.find("card = styles.card").unwrap();
    let aliased = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning["code"] == "aliased-css-module-reference")
        .collect::<Vec<_>>();
    assert_eq!(aliased.len(), 1);
    assert_eq!(aliased[0]["file"], "/project/Card.tsx");
    assert_eq!(aliased[0]["start"], start);
    assert_eq!(aliased[0]["end"], start + "card = styles.card".len());
}

#[test]
fn warns_at_a_non_classname_css_module_reference_site() {
    let source = "import styles from './Card.module.css';\nexport const find = () => document.querySelector(`.${styles.card}`);\n";
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": source
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    let start = source.find("styles.card").unwrap();
    let warning = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["code"] == "non-classname-css-module-reference")
        .expect("non-className reference warning");
    assert_eq!(warning["file"], "/project/Card.tsx");
    assert_eq!(warning["start"], start);
    assert_eq!(warning["end"], start + "styles.card".len());
}

#[test]
fn parses_jsx_in_javascript_files() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.js",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(
        response["files"][0]["source"],
        "export const Card = () => <div className=\"p-[13px]\" />;\n"
    );
}

#[test]
fn moves_local_keyframes_to_the_tailwind_entry() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n.button { animation: fade 1s; }\n",
        "tailwindPath": "/project/globals.css",
        "tailwindSource": "@import \"tailwindcss\";\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);
    let candidate = response["candidates"][0].as_str().unwrap();
    let name = candidate
        .strip_prefix("[animation:")
        .and_then(|candidate| candidate.strip_suffix("_1s]"))
        .unwrap();
    let tailwind = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/globals.css")
        .unwrap();

    assert!(name.starts_with("tw-migrate-"));
    assert!(name.ends_with("-fade"));
    assert!(
        tailwind["source"]
            .as_str()
            .unwrap()
            .contains(&format!("@keyframes {name}"))
    );
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Button.module.css"])
    );
}

#[test]
fn an_unwritable_entry_disables_keyframe_movement() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n.button { animation: fade 1s; }\n",
        "tailwindPath": "/project/globals.css",
        "tailwindSource": "@import \"tailwindcss\";\n",
        "entryWritable": false,
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert!(
        !response["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "/project/globals.css"),
        "an unwritable entry must never be planned"
    );
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(response["rules"][0]["status"], "retained");
    assert_eq!(response["warnings"][0]["code"], "unsupported-at-rule");
}

#[test]
fn removes_an_import_after_moving_an_at_rule_only_module() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n",
        "tailwindPath": "/project/globals.css",
        "tailwindSource": "@import \"tailwindcss\";\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button>Save</button>;\n"
        }]
    });

    let response = plan(request);
    let source = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/Button.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();

    assert_eq!(
        source,
        "export const Button = () => <button>Save</button>;\n"
    );
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Button.module.css"])
    );
}

#[test]
fn rejects_conflicting_tailwind_keyframes() {
    let keyframe = KeyframePlan {
        span: 0..0,
        name: "fade".to_string(),
        migrated_name: "tw-migrate-fade".to_string(),
        source: "@keyframes tw-migrate-fade { from { opacity: 0; } }".to_string(),
    };

    assert!(
        append_keyframes(
            "@keyframes tw-migrate-fade { from { opacity: 1; } }",
            &[&keyframe]
        )
        .is_err()
    );
}

#[test]
fn rejects_ambiguous_animation_names() {
    let keyframes = HashMap::from([("linear", "tw-migrate-linear")]);

    assert_eq!(
        animation_candidate("animation", "linear 1s", &keyframes),
        None
    );
    assert_eq!(
        animation_candidate("animation-name", "linear", &keyframes),
        Some("[animation-name:tw-migrate-linear]".to_string())
    );

    let keyframes = HashMap::from([("fade_in", "tw-migrate-fade_in")]);
    assert_eq!(
        animation_candidate("animation", "fade_in 1s", &keyframes),
        Some("[animation:tw-migrate-fade\\_in_1s]".to_string())
    );
}

#[test]
fn retains_unsupported_keyframe_dependencies() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n.button { animation: fade 1s, fade 2s; }\n",
        "tailwindPath": "/project/globals.css",
        "tailwindSource": "@import \"tailwindcss\";\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| { warning["code"] == "unsupported-animation" })
    );
}

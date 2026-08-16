use super::*;

use super::vue::vue_module_request;

#[test]
fn returns_structured_font_family_probes() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": ".button { font-family: \"Open Sans\", sans-serif; }\n.plain { font-family: Arial, serif; }\n.generic { font-family: monospace; }\n.wide { font-family: inherit; }\n.runtime { font-family: var(--font-body); }\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={`${styles.button} ${styles.plain} ${styles.generic} ${styles.wide} ${styles.runtime}`}>B</button>;\n"
        }]
    });

    let response = plan(request);

    let probes = response["fontFamilyProbes"].as_array().unwrap();
    // The runtime-dependent stack emits no probe; every parsed stack
    // carries its normalized value and first-family kind.
    assert_eq!(probes.len(), 4, "{probes:?}");
    let by_value: Vec<(&str, &str, &str)> = probes
        .iter()
        .map(|probe| {
            (
                probe["value"].as_str().unwrap(),
                probe["firstFamily"]["name"].as_str().unwrap(),
                probe["firstFamily"]["kind"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(by_value.contains(&("\"Open Sans\", sans-serif", "Open Sans", "name")), "{by_value:?}");
    assert!(by_value.contains(&("\"Arial\", serif", "Arial", "name")), "{by_value:?}");
    assert!(by_value.contains(&("monospace", "monospace", "generic")), "{by_value:?}");
    assert!(by_value.contains(&("inherit", "inherit", "css-wide")), "{by_value:?}");
    for probe in probes {
        assert!(
            probe["candidate"].as_str().unwrap().starts_with("[font-family:"),
            "{probe:?}"
        );
        assert!(probe["ruleId"]["end"].as_u64().unwrap() > 0, "{probe:?}");
    }
}

#[test]
fn identical_member_candidates_do_not_conflict() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".a { padding: 8px; }\n.b { padding: 8px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`${styles.a} ${styles.b}`} />;\n"
        }]
    });

    let response = plan(request);

    // Both members share one spelling, so one emitted class serves the
    // site without a conflict.
    assert_eq!(response["convertedRules"], 2, "{response}");
    assert!(
        !response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "module-utilities-conflict")
    );
}

#[test]
fn keyframe_referencing_candidates_keep_their_spelling_under_aliases() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n.spin { animation: fade 1s; }\n.bad:hover span { color: blue; }\n",
        "tailwindPath": "/project/globals.css",
        "tailwindSource": "@import \"tailwindcss\";\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.spin} />;\n"
        }]
    });

    let baseline = plan(request.clone());
    let animation_candidate = baseline["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate.as_str().unwrap().to_string())
        .find(|candidate| candidate.starts_with("[animation:"))
        .unwrap_or_else(|| panic!("animation candidate in {baseline}"));

    let mut aliased = request;
    aliased["candidateAliases"] =
        serde_json::json!({ animation_candidate.clone(): "animate-spin" });
    let response = plan(aliased);

    // The alias would strip the migrated keyframe name that keyframe
    // movement scans for, so the candidate keeps its spelling.
    assert!(
        response["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| candidate == &serde_json::json!(animation_candidate)),
        "{response}"
    );
}

#[test]
fn variant_separated_members_with_shared_properties_do_not_conflict() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".a:hover { color: red; }\n.b:focus { color: blue; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`${styles.a} ${styles.b}`} />;\n"
        }]
    });

    let response = plan(request);

    // hover: and focus: occupy different variant slots, so the shared
    // color property never reads as a same-site conflict.
    assert_eq!(response["convertedRules"], 2, "{response}");
    assert!(
        !response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "module-utilities-conflict")
    );
}

#[test]
fn identical_candidates_across_stylesheets_do_not_conflict() {
    let request = serde_json::json!({
        "stylesheets": [
            { "cssPath": "/project/A.module.css", "cssSource": ".a { padding: 8px; }\n" },
            { "cssPath": "/project/B.module.css", "cssSource": ".b { padding: 8px; }\n" },
        ],
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const Card = () => <div className={`${a.a} ${b.b}`} />;\n"
        }]
    });

    let response = plan_batch(request);

    // Both rules produce the same spelling; one emitted class serves
    // both, so nothing is retained as a cross-stylesheet conflict.
    assert!(
        !response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "batch-stylesheet-conflict"),
        "{response}"
    );
}

#[test]
fn aliased_member_spellings_conflict_by_properties_not_prefix() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".title { font-family: \"My Font\", sans-serif; }\n.strong { font-weight: 700; }\n",
        "candidateAliases": { "[font-family:\"My_Font\",_sans-serif]": "font-my-font" },
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`${styles.title} ${styles.strong}`} />;\n"
        }]
    });

    let response = plan(request);

    // `font-my-font` carries its transferred font-family property, so
    // pairing it with the font-weight member never reads as a conflict
    // through the `font-` spelling prefix.
    assert_eq!(response["convertedRules"], 2);
    assert!(
        response["files"][0]["source"]
            .as_str()
            .unwrap()
            .contains("font-my-font font-[700]")
    );
}

#[test]
fn canonical_aliases_apply_to_global_html_rules() {
    let source = "<link rel=\"stylesheet\" href=\"./legacy.css\"><div class=\"card\"></div><main id=\"hero\">Text</main>\n";
    let class_start = source.find("class=\"card\"").unwrap() + "class=\"".len();
    let id_start = source.find("id=\"hero\"").unwrap() + "id=\"".len();
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/legacy.css",
            "cssSource": ".card { padding: 13px; }\n#hero { height: 100vh; }\n",
        }],
        "candidateAliases": { "h-[100vh]": "h-screen" },
        "files": [{
            "path": "/project/index.html",
            "source": source,
            "htmlElements": [
                {
                    "tag": "div",
                    "classAttribute": {
                        "value": "card",
                        "start": class_start,
                        "end": class_start + "card".len(),
                        "writable": true,
                    },
                },
                {
                    "tag": "main",
                    "classAttribute": {
                        "value": "",
                        "start": source.find(">Text").unwrap(),
                        "end": source.find(">Text").unwrap(),
                        "writable": true,
                        "synthetic": true,
                    },
                    "idAttribute": {
                        "value": "hero",
                        "start": id_start,
                        "end": id_start + "hero".len(),
                        "writable": true,
                    },
                },
            ],
            "htmlStylesheets": [{
                "cssPath": "/project/legacy.css",
                "variants": [],
                "direct": true,
                "analyzable": true,
            }],
            "htmlReferencesSafe": true,
        }],
    });

    let response = plan_batch(request);

    assert!(
        response["files"][0]["source"].as_str().unwrap().contains("h-screen"),
        "{response}"
    );
}

#[test]
fn canonical_aliases_apply_inside_html_context_variants() {
    let source = "<link rel=\"stylesheet\" href=\"./print.css\" media=\"print\"><div class=\"card\"></div>\n";
    let value_start = source.find("class=\"card\"").unwrap() + "class=\"".len();
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/print.css",
            "cssSource": ".card { margin-right: auto; }\n",
        }],
        "candidateAliases": { "mr-[auto]": "mr-auto" },
        "files": [{
            "path": "/project/index.html",
            "source": source,
            "htmlElements": [{
                "tag": "div",
                "classAttribute": {
                    "value": "card",
                    "start": value_start,
                    "end": value_start + "card".len(),
                    "writable": true,
                },
            }],
            "htmlStylesheets": [{
                "cssPath": "/project/print.css",
                "variants": ["twm-media-print"],
                "direct": true,
                "analyzable": true,
            }],
            "htmlReferencesSafe": true,
        }],
    });

    let response = plan_batch(request);

    // Rule-level aliases resolve before contextual variants wrap the
    // candidate, so the conditional context renders the canonical
    // spelling inside its generated variant.
    assert!(
        response["files"][0]["source"]
            .as_str()
            .unwrap()
            .contains("twm-media-print:mr-auto"),
        "{response}"
    );
}

#[test]
fn applies_candidate_aliases_to_rewrites_and_probes() {
    let source = "<template>\n  <p :class=\"$style.card\">A</p>\n</template>\n<style module>\n.card { font-family: \"My Font\", sans-serif; }\n</style>\n";
    let mut request = vue_module_request(source, &[("p", "card")]);
    request["candidateAliases"] = serde_json::json!({
        "[font-family:\"My_Font\",_sans-serif]": "font-my-font",
    });
    let response = plan_batch(request);
    // The canonical spelling carries no quote, so the rewrite that the
    // arbitrary candidate could not make now fits the attribute.
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["candidates"], serde_json::json!(["font-my-font"]));
    assert_eq!(response["candidateProbes"], serde_json::json!(["font-my-font"]));
    assert!(
        response["files"][0]["source"]
            .as_str()
            .unwrap()
            .contains("font-my-font")
    );
}

#[test]
fn aliased_candidates_keep_their_source_properties_for_conflicts() {
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/Button.module.css",
            "cssSource": "@media (width <= 700px) { .button { margin: 4px; } }\n.button { margin: 8px; }\n",
        }],
        "mediaNames": { "(width <= 700px)": "width-lte-700px" },
        "candidateAliases": { "m-[8px]": "m-2" },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan_batch(request);

    // The alias renames the base candidate, but its transferred margin
    // properties still collide with the earlier-authored media rule, so
    // the ordering-sensitive gate keeps retaining the pair.
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "batch-stylesheet-conflict")
    );
}

#[test]
fn merges_properties_when_aliases_deduplicate_candidates() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": ".button { margin-right: auto; margin-left: auto; }\n",
        "candidateAliases": { "mr-[auto]": "mx-auto", "ml-[auto]": "mx-auto" },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["candidates"], serde_json::json!(["mx-auto"]));
    assert!(
        response["files"][0]["source"]
            .as_str()
            .unwrap()
            .contains("\"mx-auto\"")
    );
}

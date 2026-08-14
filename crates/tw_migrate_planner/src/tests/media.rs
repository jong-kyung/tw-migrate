use super::*;

#[test]
fn converts_an_exact_media_breakpoint() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": "@media (min-width: 48rem) { .card { padding: 13px; } }\n",
        "themeTokens": { "breakpoint-md": "48rem" },
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = () => <div className=\"card\" />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["candidates"], serde_json::json!(["md:p-[13px]"]));
    assert_eq!(
        response["files"][0]["source"],
        "export const Card = () => <div className=\"card md:p-[13px]\" />;\n"
    );
}

#[test]
fn converts_an_exact_media_breakpoint_range() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": "@media (min-width: 48rem) and (max-width: 63.999rem) { .card { padding: 13px; } }\n",
        "themeTokens": {
            "breakpoint-md": "48rem",
            "breakpoint-lg": "64rem"
        },
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["md:max-lg:p-[13px]"])
    );
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn converts_an_unmatched_media_range_to_an_arbitrary_variant() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": "@media (min-width: 48rem) and (max-width: 60rem) { .card { padding: 13px; } }\n",
        "themeTokens": {
            "breakpoint-md": "48rem",
            "breakpoint-lg": "64rem"
        },
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["[@media_(min-width:48rem)_and_(max-width:60rem)]:p-[13px]"])
    );
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn converts_nested_media_and_supports_rules() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media (min-width: 48rem) { .button { padding: 1rem; } @supports (display: grid) { .button { display: grid; } } }\n",
        "themeTokens": { "breakpoint-md": "48rem" },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["md:p-[1rem]", "md:supports-[display:grid]:grid"])
    );
    assert_eq!(response["convertedRules"], 2);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Button.module.css"])
    );
}

#[test]
fn converts_tailwind_conditional_variants() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media (prefers-reduced-motion: reduce) { @starting-style { @container (min-width: 28rem) { .button { display: grid; } } } }\n@media (prefers-color-scheme: dark) { .button { color: white; } }\n",
        "themeTokens": { "container-md": "28rem" },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["dark:text-[white]", "motion-reduce:starting:@md:grid"])
    );
    assert_eq!(response["convertedRules"], 2);
}

#[test]
fn orders_a_base_rule_before_a_later_media_rule() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": ".button { margin: 8px; }\n@media (width <= 700px) { .button { margin: 4px; } }\n",
        "mediaNames": { "(width <= 700px)": "width-lte-700px" },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    // The media rule follows the base rule, matching Tailwind's output
    // order, so both migrate.
    assert_eq!(response["convertedRules"], 2);
    assert_eq!(
        response["candidates"],
        serde_json::json!(["m-[8px]", "width-lte-700px:m-[4px]"])
    );
}

#[test]
fn retains_a_media_rule_authored_before_its_base_rule() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media (width <= 700px) { .button { margin: 4px; } }\n.button { margin: 8px; }\n",
        "mediaNames": { "(width <= 700px)": "width-lte-700px" },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    // The authored base rule wins whenever the media condition matches,
    // but Tailwind would emit the media variant after the base utility
    // and flip the winner, so the pair is retained.
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
fn retains_overlapping_media_rules_with_unproven_order() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media (width <= 700px) { .button { margin: 4px; } }\n@media (width <= 800px) { .button { margin: 6px; } }\n",
        "mediaNames": {
            "(width <= 700px)": "width-lte-700px",
            "(width <= 800px)": "width-lte-800px"
        },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    // Both conditions match at 600px and the emitted variant order is
    // unproven, so the pair is retained.
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
}

#[test]
fn converts_mutually_exclusive_media_rules() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media (width <= 700px) { .button { margin: 4px; } }\n@media (width >= 900px) { .button { margin: 6px; } }\n",
        "mediaNames": {
            "(width <= 700px)": "width-lte-700px",
            "(width >= 900px)": "width-gte-900px"
        },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    // The width intervals are disjoint, so no ordering can change the
    // rendered result and both rules migrate.
    assert_eq!(response["convertedRules"], 2);
}

#[test]
fn gates_overlapping_arbitrary_media_variants_under_an_empty_map() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media (width <= 700px) { .button { margin: 4px; } }\n@media (width <= 800px) { .button { margin: 6px; } }\n",
        "mediaNames": {},
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    // An explicitly empty map still means extraction ran; the fallback
    // arbitrary variants overlap with an unproven order and retain.
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
}

#[test]
fn converts_media_conditions_through_supplied_names() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media screen and (width <= 768px) { .button { margin: 0; } }\n@media (min-width: 900px) { .button { padding: 1rem; } }\n",
        "mediaNames": {
            "screen": "screen",
            "(width <= 768px)": "width-lte-768px"
        },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    // The mapped compound stacks its component names; the 900px key is
    // absent from the map (a resolver fallback), so it keeps the
    // shipped arbitrary form.
    assert_eq!(
        response["candidates"],
        serde_json::json!([
            "[@media_(min-width:900px)]:p-[1rem]",
            "screen:width-lte-768px:m-[0]"
        ])
    );
    assert_eq!(response["convertedRules"], 2);
}

#[test]
fn supplied_names_override_shipped_media_conversions() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media (prefers-color-scheme: dark) { .button { color: white; } }\n@media (min-width: 48rem) { .button { padding: 1rem; } }\n@media screen, print { .button { margin: 0; } }\n",
        "themeTokens": { "breakpoint-md": "48rem" },
        "mediaNames": {
            "(prefers-color-scheme: dark)": "prefers-color-scheme-dark",
            "(width >= 48rem)": "width-gte-48rem",
            "screen, print": "screen-or-print"
        },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    // The map is the entry group's verified resolution: a redefined
    // dark and a shadowed breakpoint resolve to generated component
    // variants instead of the shipped conversions, and a whole
    // condition uses its single name.
    assert_eq!(
        response["candidates"],
        serde_json::json!([
            "prefers-color-scheme-dark:text-[white]",
            "screen-or-print:m-[0]",
            "width-gte-48rem:p-[1rem]"
        ])
    );
    assert_eq!(response["convertedRules"], 3);
}

#[test]
fn an_explicitly_empty_map_stays_authoritative() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media (min-width: 48rem) { .button { padding: 1rem; } }\n@media (prefers-color-scheme: dark) { .button { color: white; } }\n",
        "themeTokens": { "breakpoint-md": "48rem" },
        // Extraction is enabled but every condition fell back, so the
        // resolver legitimately produced an empty map. That is not the
        // same as no map: the legacy conversions must stay off.
        "mediaNames": {},
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!([
            "[@media_(min-width:48rem)]:p-[1rem]",
            "[@media_(prefers-color-scheme:dark)]:text-[white]"
        ])
    );
    assert_eq!(response["convertedRules"], 2);
}

#[test]
fn unresolved_map_keys_never_revive_legacy_conversions() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media (min-width: 48rem) { .button { padding: 1rem; } }\n@media (prefers-color-scheme: dark) { .button { color: white; } }\n",
        "themeTokens": { "breakpoint-md": "48rem" },
        // The map is supplied but omits both keys: the resolver sent
        // them to the arbitrary fallback because their readable and
        // digest names were unavailable in a project that shadows md
        // and dark. The legacy conversions must not revive the exact
        // names the resolver rejected.
        "mediaNames": { "(width <= 600px)": "width-lte-600px" },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!([
            "[@media_(min-width:48rem)]:p-[1rem]",
            "[@media_(prefers-color-scheme:dark)]:text-[white]"
        ])
    );
    assert_eq!(response["convertedRules"], 2);
}

#[test]
fn escapes_literal_underscores_in_arbitrary_candidates() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@supports (font-tech(color_colrv1)) { .button { --font-key: Open_Sans; } }\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["supports-[font-tech(color\\_colrv1)]:[--font-key:Open\\_Sans]"])
    );
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn encodes_quoted_values_and_urls_into_arbitrary_candidates() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": ".button { background-image: url(\"a_b.png\"); font-family: \"My Font\", sans-serif; content: \"a_b\"; width: calc(min(100%, 50vw)); }\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!([
            "[background-image:url(\"a_b.png\")]",
            "[content:\"a\\_b\"]",
            "[font-family:\"My_Font\",_sans-serif]",
            "w-[calc(min(100%,_50vw))]"
        ])
    );
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn encodes_grid_line_names_into_arbitrary_candidates() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": ".button { grid-template-columns: [full-start] 1fr; }\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["[grid-template-columns:[full-start]_1fr]"])
    );
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn retains_unrepresentable_values_with_an_unsupported_value_warning() {
    // Tailwind preserves url() bodies verbatim (underscores are not
    // decoded back to spaces there), so a space inside url() cannot be
    // represented in a class attribute.
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": ".button { background-image: url(\"a b.png\"); }\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    let warnings = response["warnings"].as_array().unwrap();
    assert!(!warnings.is_empty());
    assert!(
        warnings
            .iter()
            .any(|warning| warning["code"] == "unsupported-value")
    );
}

#[test]
fn converts_conditions_nested_inside_style_rules() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": ".button { opacity: 1; @starting-style { opacity: 0; } @media (prefers-reduced-motion: reduce) { display: none; } }\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!([
            "motion-reduce:hidden",
            "opacity-[1]",
            "starting:opacity-[0]"
        ])
    );
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn moves_global_definition_at_rules_to_the_tailwind_entry() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@property --progress { syntax: \"<number>\"; inherits: false; initial-value: 0; }\n.button { display: grid; }\n",
        "tailwindPath": "/project/globals.css",
        "tailwindSource": "@import \"tailwindcss\";\n/* @property --progress { syntax: \"<number>\"; inherits: false; initial-value: 0; } */\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);
    let tailwind = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/globals.css")
        .unwrap();

    assert_eq!(
        tailwind["source"]
            .as_str()
            .unwrap()
            .matches("@property --progress")
            .count(),
        2
    );
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Button.module.css"])
    );
}

#[test]
fn retains_global_definition_at_rules_with_urls() {
    let request = serde_json::json!({
        "cssPath": "/project/components/Button.module.css",
        "cssSource": "@font-face { font-family: Custom; src: url('./custom.woff2'); }\n.button { display: grid; }\n",
        "tailwindPath": "/project/globals.css",
        "tailwindSource": "@import \"tailwindcss\";\n",
        "files": [{
            "path": "/project/components/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| { warning["code"] == "unsupported-at-rule" })
    );
}

#[test]
fn moves_global_at_rules_with_stable_urls() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@font-face { font-family: Custom; src: url('./fonts/custom.woff2'); }\n@page { margin: 2cm; }\n.button { display: grid; }\n",
        "tailwindPath": "/project/globals.css",
        "tailwindSource": "@import \"tailwindcss\";\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);
    let tailwind = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/globals.css")
        .unwrap()["source"]
        .as_str()
        .unwrap();

    assert!(tailwind.contains("url('./fonts/custom.woff2')"));
    assert!(tailwind.contains("@page"));
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Button.module.css"])
    );
}

#[test]
fn converts_named_container_queries_to_arbitrary_variants() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media (min-width: 48rem) { .button { padding: 1rem; } @container card_grid (min-width: 20rem) { .button { display: grid; } } }\n",
        "themeTokens": { "breakpoint-md": "48rem" },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!([
            "md:[@container_card\\_grid_(min-width:20rem)]:grid",
            "md:p-[1rem]"
        ])
    );
    assert_eq!(response["convertedRules"], 2);
}

#[test]
fn retains_unsupported_nested_at_rules() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": "@media (min-width: 48rem) { @layer components { .button { display: grid; } } }\n",
        "themeTokens": { "breakpoint-md": "48rem" },
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert!(response["files"].as_array().unwrap().is_empty());
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| { warning["code"] == "unsupported-nested-at-rule" })
    );
}

fn retained_at_rule_warning(css_source: &str) -> Vec<String> {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": css_source,
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });
    let response = plan(request);
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning["code"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn retains_an_unconvertible_media_query() {
    let codes = retained_at_rule_warning(
        "@media /* screens */ (min-width: 48rem) { .button { padding: 13px; } }\n",
    );
    assert!(codes.contains(&"unsupported-media-query".to_string()));
}

#[test]
fn retains_an_unconvertible_supports_query() {
    let codes =
        retained_at_rule_warning("@supports (content: \"x\") { .button { padding: 13px; } }\n");
    assert!(codes.contains(&"unsupported-supports-query".to_string()));
}

#[test]
fn retains_an_unconvertible_container_query() {
    let codes = retained_at_rule_warning(
        "@container /* card */ (min-width: 20rem) { .button { padding: 13px; } }\n",
    );
    assert!(codes.contains(&"unsupported-container-query".to_string()));
}

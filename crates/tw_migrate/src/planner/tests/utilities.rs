use super::*;

#[test]
fn rewrites_the_target_selector_even_when_its_name_recurs() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".a:not(.abc) { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = () => <div className='a' />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["[&:not(.abc)]:p-[13px]"])
    );
}

#[test]
fn rewrites_the_last_compound_of_a_descendant_selector_by_span() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".foo .a:not(.abc) { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = () => <div className='a' />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["[.foo_&:not(.abc)]:p-[13px]"])
    );
}

#[test]
fn keeps_only_the_last_duplicate_declaration() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { color: red; color: blue; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["candidates"], serde_json::json!(["text-[blue]"]));
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn arbitrary_properties_conflict_with_named_utilities() {
    assert!(tailwind_utilities_conflict("[display:block]", "hidden"));
    assert!(tailwind_utilities_conflict("[padding:8px]", "p-2"));
    assert!(tailwind_utilities_conflict("[margin-top:4px]", "mt-2"));
    assert!(tailwind_utilities_conflict("[width:10rem]", "w-4"));
    assert!(tailwind_utilities_conflict("md:[display:block]", "md:flex"));
    assert!(!tailwind_utilities_conflict("[display:block]", "p-2"));
    assert!(!tailwind_utilities_conflict("[opacity:1]", "hidden"));
    assert!(!tailwind_utilities_conflict("[display:block]", "sm:hidden"));
}

#[test]
fn tier1_arbitrary_properties_conflict_with_named_utilities() {
    assert!(tailwind_utilities_conflict("[position:absolute]", "static"));
    assert!(tailwind_utilities_conflict("[top:0]", "top-[0]"));
    assert!(tailwind_utilities_conflict("[inset:0]", "left-2"));
    assert!(tailwind_utilities_conflict(
        "[overflow:hidden]",
        "overflow-x-auto"
    ));
    assert!(tailwind_utilities_conflict(
        "[overflow-y:auto]",
        "overflow-hidden"
    ));
    assert!(tailwind_utilities_conflict("[z-index:30]", "z-30"));
    assert!(tailwind_utilities_conflict("[opacity:0.5]", "opacity-50"));
    assert!(tailwind_utilities_conflict(
        "[font-weight:700]",
        "font-bold"
    ));
    assert!(tailwind_utilities_conflict(
        "[line-height:1.15]",
        "leading-tight"
    ));
    assert!(tailwind_utilities_conflict(
        "[letter-spacing:0.2em]",
        "tracking-wide"
    ));
    assert!(tailwind_utilities_conflict(
        "[text-align:center]",
        "text-left"
    ));
    assert!(tailwind_utilities_conflict(
        "[flex-direction:row]",
        "flex-col"
    ));
    assert!(tailwind_utilities_conflict(
        "[align-items:center]",
        "items-start"
    ));
    assert!(tailwind_utilities_conflict(
        "[justify-content:center]",
        "justify-between"
    ));
    assert!(tailwind_utilities_conflict(
        "[flex-wrap:wrap]",
        "flex-nowrap"
    ));
    assert!(tailwind_utilities_conflict(
        "[border-width:2px]",
        "border-2"
    ));
    assert!(tailwind_utilities_conflict(
        "[border-style:solid]",
        "border-dashed"
    ));
    assert!(tailwind_utilities_conflict(
        "[border-color:red]",
        "border-red-500"
    ));
    assert!(tailwind_utilities_conflict("[min-width:1rem]", "min-w-4"));
    assert!(tailwind_utilities_conflict("[max-height:1rem]", "max-h-4"));
    assert!(!tailwind_utilities_conflict(
        "[overflow-x:auto]",
        "overflow-y-hidden"
    ));
    assert!(!tailwind_utilities_conflict(
        "[border-width:2px]",
        "border-dashed"
    ));
    assert!(!tailwind_utilities_conflict("[top:0]", "left-2"));
    assert!(!tailwind_utilities_conflict("[z-index:30]", "opacity-50"));
}

#[test]
fn maps_tier1_families_to_exact_or_arbitrary_candidates() {
    let tokens = HashMap::from(
        [
            ("spacing", "0.25rem"),
            ("leading-tight", "1.25"),
            ("tracking-wide", "0.025em"),
            ("font-weight-normal", "400"),
            ("font-weight-bold", "700"),
            ("container-sm", "24rem"),
            ("color-brand", "#123456"),
        ]
        .map(|(name, value)| (name.to_string(), value.to_string())),
    );
    let cases = [
        ("position", "absolute", "absolute"),
        ("position", "sticky", "sticky"),
        ("position", "inherit", "[position:inherit]"),
        ("z-index", "30", "z-30"),
        ("z-index", "auto", "z-auto"),
        ("z-index", "-1", "z-[-1]"),
        ("opacity", "50%", "opacity-50"),
        ("opacity", "0.5", "opacity-[0.5]"),
        ("font-weight", "700", "font-bold"),
        ("font-weight", "bold", "font-bold"),
        ("font-weight", "550", "font-[550]"),
        ("font-weight", "lighter", "[font-weight:lighter]"),
        ("line-height", "1.25", "leading-tight"),
        ("line-height", "1.15", "leading-[1.15]"),
        ("letter-spacing", "0.025em", "tracking-wide"),
        ("letter-spacing", "0.2em", "tracking-[0.2em]"),
        ("text-align", "center", "text-center"),
        ("text-align", "start", "[text-align:start]"),
        ("flex-direction", "column", "flex-col"),
        ("flex-direction", "inherit", "[flex-direction:inherit]"),
        ("align-items", "flex-start", "items-start"),
        ("align-items", "start", "[align-items:start]"),
        ("justify-content", "space-between", "justify-between"),
        ("justify-content", "left", "[justify-content:left]"),
        ("flex-wrap", "wrap", "flex-wrap"),
        ("flex-wrap", "inherit", "[flex-wrap:inherit]"),
        ("border-width", "1px", "border"),
        ("border-width", "2px", "border-2"),
        ("border-width", "thin", "[border-width:thin]"),
        ("border-color", "#123456", "border-brand"),
        ("border-color", "red", "border-[red]"),
        ("border-style", "dashed", "border-dashed"),
        ("border-style", "groove", "[border-style:groove]"),
        ("min-width", "24rem", "min-w-sm"),
        ("min-width", "0.5rem", "min-w-2"),
        ("min-width", "13px", "min-w-[13px]"),
        ("max-width", "24rem", "max-w-sm"),
        ("max-height", "0.5rem", "max-h-2"),
        ("max-height", "13px", "max-h-[13px]"),
        ("min-height", "13px", "min-h-[13px]"),
    ];
    for (property, value, expected) in cases {
        assert_eq!(
            declaration_to_candidate(property, value, &tokens).as_deref(),
            Ok(expected),
            "{property}: {value}"
        );
    }
}

#[test]
fn normalizes_inset_shorthand_before_mapping() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { inset: 0; left: 2rem; }\n",
        "themeTokens": { "spacing": "0.25rem" },
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["bottom-[0]", "left-8", "right-[0]", "top-[0]"])
    );
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn normalizes_overflow_shorthand_before_mapping() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { overflow: hidden; overflow-x: auto; }\n.note { overflow: hidden auto; }\n",
        "files": [
            {
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.note} /></div>;\n"
            }
        ]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!([
            "overflow-x-auto",
            "overflow-x-hidden",
            "overflow-y-auto",
            "overflow-y-hidden"
        ])
    );
    assert_eq!(response["convertedRules"], 2);
}

#[test]
fn collapses_an_equal_overflow_pair_into_the_shorthand_utility() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { overflow-x: hidden; overflow-y: hidden; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["overflow-hidden"])
    );
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn maps_tier1_static_keywords_in_a_rule() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { position: absolute; text-align: center; overflow: auto; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["candidates"],
        serde_json::json!(["absolute", "overflow-auto", "text-center"])
    );
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn retains_a_module_referenced_by_another_stylesheet() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "cssDependents": ["/project/Consumer.module.css"],
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
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
            .any(
                |warning| warning["code"] == "unsupported-css-module-reference"
                    && warning["file"] == "/project/Consumer.module.css"
            )
    );
}

#[test]
fn retains_rules_when_combined_template_members_conflict() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".a { padding: 8px; }\n.b { padding: 4px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`${styles.a} ${styles.b}`} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(response["files"], serde_json::json!([]));
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "module-utilities-conflict")
    );
}

#[test]
fn leaves_a_template_without_module_members_untouched() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`marketing-card`}><span className={styles.card} /></div>;\n"
        }]
    });

    let response = plan(request);
    let source = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/Card.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();

    assert!(source.contains("className={`marketing-card`}"));
    assert!(source.contains("className=\"p-[13px]\""));
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn retains_a_module_required_from_commonjs() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }, {
            "path": "/project/legacy.cjs",
            "source": "const styles = require('./Card.module.css');\nmodule.exports = styles;\n"
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
            .any(|warning| warning["code"] == "unsupported-css-module-reference")
    );
}

#[test]
fn retains_a_module_loaded_with_dynamic_import() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }, {
            "path": "/project/lazy.ts",
            "source": "export const load = () => import('./Card.module.css');\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
}

#[test]
fn warns_and_retains_reexported_css_modules() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }, {
            "path": "/project/index.ts",
            "source": "export { default as cardStyles } from './Card.module.css';\n"
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
            .any(|warning| warning["code"] == "unsupported-css-module-reference")
    );
}

#[test]
fn retains_repeated_selector_rules_with_overlapping_utilities() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 8px; }\n.card { padding: 4px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    assert_eq!(response["files"], serde_json::json!([]));
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] == "unsupported-overlap")
    );
}

use super::*;

fn warning_message(response: &serde_json::Value, code: &str) -> String {
    response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["code"] == code)
        .unwrap_or_else(|| panic!("missing warning {code}: {:?}", response["warnings"]))["message"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn retains_an_unproven_module_relationship_with_a_site_hint() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card > .title { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\nexport const Loose = () => <span className={styles.title} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    let message = warning_message(&response, "unproven-css-module-relationship");
    assert!(message.contains("/project/Card.tsx"), "{message}");
}

#[test]
fn retains_a_module_relationship_behind_a_conditional_return() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card .title { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nfunction Title(props) {\n  if (props.compact) {\n    return <span className={styles.title} />;\n  }\n  return <span className={styles.title} />;\n}\nexport const Card = () => <div className={styles.card}><Title /></div>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["retainedRules"], 1);
    let message = warning_message(&response, "unproven-css-module-relationship");
    assert!(message.contains("conditional-return"), "{message}");
}

#[test]
fn retains_a_module_relationship_used_inside_an_export_class() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card .title { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\nexport class Legacy {\n  render() {\n    return <span className={styles.title} />;\n  }\n}\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    let message = warning_message(&response, "unproven-css-module-relationship");
    assert!(message.contains("dynamic-content-boundary"), "{message}");
}

#[test]
fn retains_a_module_relationship_behind_a_hoc() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card .title { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nimport { withTheme } from './theme';\nfunction Title() {\n  return <span className={styles.title} />;\n}\nconst Fancy = withTheme(Title);\nexport const Card = () => <div className={styles.card}><Fancy /></div>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["retainedRules"], 1);
    let message = warning_message(&response, "unproven-css-module-relationship");
    assert!(message.contains("hoc-or-dynamic-component"), "{message}");
}

#[test]
fn batch_retains_a_proven_relationship_with_a_reference_only_target_usage() {
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card > .title { padding: 13px; }\n"
        }],
        "files": [
            {
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\n"
            },
            {
                "path": "/project/Extra.tsx",
                "source": "import styles from './Card.module.css';\nexport const Extra = () => <div className={styles.card}><span className={styles.title} /></div>;\n",
                "writable": false
            }
        ]
    });

    let response = plan_batch(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    let message = warning_message(&response, "unproven-css-module-relationship");
    assert!(message.contains("/project/Extra.tsx"), "{message}");
}

#[test]
fn converts_a_proven_child_relationship_in_the_same_file() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { display: flex; }\n.card > .title { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title}>t</span></div>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 2);
    assert_eq!(response["retainedRules"], 0);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Card.module.css"])
    );
    assert_eq!(
        response["files"][0]["source"],
        "export const Card = () => <div className=\"flex\"><span className=\"p-[13px]\">t</span></div>;\n"
    );
}

#[test]
fn converts_a_proven_relationship_through_an_imported_component() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { display: flex; }\n.card .title { padding: 13px; }\n",
        "files": [
            {
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nimport Title from './Title';\nexport const Card = () => <div className={styles.card}><Title /></div>;\n"
            },
            {
                "path": "/project/Title.tsx",
                "source": "import styles from './Card.module.css';\nexport default function Title() {\n  return <h1 className={styles.title} />;\n}\n"
            }
        ]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 2);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Card.module.css"])
    );
    let title = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/Title.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    assert!(title.contains("className=\"p-[13px]\""), "{title}");
    assert!(!title.contains("Card.module.css"), "{title}");
}

#[test]
fn converts_a_target_pseudo_state_on_a_proven_relationship() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { display: flex; }\n.card .title:hover { color: red; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 2);
    assert_eq!(
        response["candidates"],
        serde_json::json!(["flex", "hover:text-[red]"])
    );
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Card.module.css"])
    );
}

#[test]
fn converts_a_proven_three_compound_chain() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { display: flex; }\n.card > .list { margin: 1px; }\n.card .list > .item { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><ul className={styles.list}><li className={styles.item} /></ul></div>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 3);
    assert_eq!(response["retainedRules"], 0);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Card.module.css"])
    );
    assert_eq!(
        response["files"][0]["source"],
        "export const Card = () => <div className=\"flex\"><ul className=\"m-[1px]\"><li className=\"p-[13px]\" /></ul></div>;\n"
    );
}

#[test]
fn retains_an_ancestor_state_relationship() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card:hover .title { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    let message = warning_message(&response, "unproven-css-module-relationship");
    assert!(message.contains("Ancestor-state"), "{message}");
}

#[test]
fn batch_proves_relationships_against_the_request_snapshot() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { padding: 13px; }\n"
            },
            {
                "cssPath": "/project/B.module.css",
                "cssSource": ".card { display: flex; }\n.card > .title { color: red; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={a.a}><div className={b.card}><span className={b.title} /></div></div>;\n"
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 3);
    assert_eq!(response["retainedRules"], 0);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/A.module.css", "/project/B.module.css"])
    );
    assert_eq!(
        response["files"][0]["source"],
        "export const App = () => <div className=\"p-[13px]\"><div className=\"flex\"><span className=\"text-[red]\" /></div></div>;\n"
    );
}

#[test]
fn batch_keeps_the_module_when_a_sibling_relationship_is_unproven() {
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { display: flex; }\n.card > .title { padding: 13px; }\n.card .loose { margin: 1px; }\n"
        }],
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\nexport const Loose = () => <i className={styles.loose} />;\n"
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 2);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    let card = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/Card.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    assert!(
        card.contains("import styles from './Card.module.css'"),
        "{card}"
    );
    assert!(card.contains("className={styles.card}"), "{card}");
    assert!(card.contains("className={styles.loose}"), "{card}");
    assert!(card.contains("className=\"p-[13px]\""), "{card}");
    let css = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/Card.module.css")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    assert!(!css.contains(".title"), "{css}");
    assert!(css.contains(".card {"), "{css}");
    assert!(css.contains(".loose"), "{css}");
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "unproven-css-module-relationship")
    );
}

#[test]
fn batch_unused_reference_only_import_prevents_module_deletion() {
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/shared/Button.module.css",
            "cssSource": ".button { padding: 13px; }\n"
        }],
        "files": [
            {
                "path": "/project/shared/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button} />;\n"
            },
            {
                "path": "/project/app/Unused.tsx",
                "source": "import styles from '../shared/Button.module.css';\nexport const unused = true;\n",
                "writable": false
            }
        ]
    });

    let response = plan_batch(request);

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

#[test]
fn retains_an_unreferenced_module_rule_with_unresolved_selector_target() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".unused { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["retainedRules"], 1);
    assert_eq!(
        warning_message(&response, "unresolved-selector-target"),
        "No exclusively supported className references were found."
    );
}

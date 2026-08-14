use super::*;

#[test]
fn parses_indented_sass_with_explicit_module_metadata() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.sass",
        "cssSource": ".button\n  padding: 13px\n",
        "syntax": "sass",
        "isModule": true,
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.sass';\nexport const Button = () => <button className={styles.button} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Button.module.sass"])
    );
}

#[test]
fn retains_scss_values_that_require_semantic_evaluation() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.scss",
        "cssSource": "$space: 13px;\n.button { padding: $space; }\n",
        "syntax": "scss",
        "isModule": true,
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.scss';\nexport const Button = () => <button className={styles.button} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["candidates"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| { warning["code"] == "unsupported-declaration" })
    );
}

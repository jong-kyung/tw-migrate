use super::*;

#[test]
fn appends_a_global_class_and_retains_the_rule() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = () => <div className='card' />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(
        response["files"][0]["source"],
        "export const Card = () => <div className=\"card p-[13px]\" />;\n"
    );
    assert_eq!(response["warnings"][0]["code"], "retained-global-rule");
}

#[test]
fn ignores_side_effect_imports_for_global_css() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import './global.css';\nexport const Card = () => <div className='card' />;\n"
        }]
    });

    let response = plan(request);

    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| { warning["code"] == "retained-global-rule" })
    );
}

#[test]
fn does_not_duplicate_a_dynamic_global_class_name() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": "#hero { height: 100vh; }\n",
        "files": [{
            "path": "/project/Hero.tsx",
            "source": "export const Hero = () => <main id=\"hero\" className={getClass()} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["files"], serde_json::json!([]));
    let codes = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning["code"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(codes, ["dynamic-class-name", "retained-global-rule"]);
}

#[test]
fn does_not_flag_module_members_as_dynamic_for_global_css() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    let codes = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning["code"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(codes, ["retained-global-rule"]);
}

#[test]
fn migrates_a_global_expression_string_literal_class_name() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = () => <div className={'card'} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Card = () => <div className=\"card p-[13px]\" />;\n"
    );
    assert_eq!(response["warnings"][0]["code"], "retained-global-rule");
}

#[test]
fn migrates_a_global_static_template_class_name() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n.featured { margin: 7px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = () => <div className={`card featured`} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Card = () => <div className=\"card featured p-[13px] m-[7px]\" />;\n"
    );
}

#[test]
fn migrates_both_branches_of_a_global_conditional_class_name() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".a { padding: 13px; }\n.b { margin: 7px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = ({ maybe }) => <div className={maybe ? 'a' : 'b'} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Card = ({ maybe }) => <div className={maybe ? 'a p-[13px]' : 'b m-[7px]'} />;\n"
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
fn migrates_the_right_operand_of_global_logical_expressions() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = ({ active, preferred, chosen }) => <div>\n<span className={active && 'card'} />\n<span className={preferred || 'card'} />\n<span className={chosen ?? 'card'} />\n</div>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Card = ({ active, preferred, chosen }) => <div>\n<span className={active && 'card p-[13px]'} />\n<span className={preferred || 'card p-[13px]'} />\n<span className={chosen ?? 'card p-[13px]'} />\n</div>;\n"
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
fn warns_on_an_unsupported_identifier_result_leaf() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = ({ active }) => <div className={active ? 'card' : active} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Card = ({ active }) => <div className={active ? 'card p-[13px]' : active} />;\n"
    );
    let dynamic = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["code"] == "dynamic-class-name")
        .unwrap();
    assert_eq!(dynamic["file"], "/project/Card.tsx");
}

#[test]
fn keeps_a_logical_left_operand_opaque() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".active { color: red; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.active && 'card'} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(response["files"], serde_json::json!([]));
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] != "dynamic-class-name")
    );
}

#[test]
fn converts_only_the_final_operand_of_chained_logicals() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".legacy { padding: 13px; }\n.base { margin: 7px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = ({ variant }) => <div className={variant || 'legacy' || 'base'} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Card = ({ variant }) => <div className={variant || 'legacy' || 'base m-[7px]'} />;\n"
    );
}

#[test]
fn migrates_a_static_template_result_leaf() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = ({ ready }) => <div className={ready ? `card` : ''} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Card = ({ ready }) => <div className={ready ? \"card p-[13px]\" : ''} />;\n"
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
fn warns_on_a_shadowed_undefined_result_leaf() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = (maybe, undefined) => <div className={maybe ? 'card' : undefined} />;\n"
        }]
    });

    let response = plan(request);

    // A local binding named `undefined` can hold a runtime class value,
    // so only the unbound global counts as a warning-free no-op leaf.
    assert_eq!(
        response["files"][0]["source"],
        "export const Card = (maybe, undefined) => <div className={maybe ? 'card p-[13px]' : undefined} />;\n"
    );
    let dynamic = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["code"] == "dynamic-class-name")
        .unwrap();
    assert_eq!(dynamic["file"], "/project/Card.tsx");
}

#[test]
fn keeps_an_emptied_module_and_import_for_a_dangling_member_leaf() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = ({ maybe }) => <div className={maybe ? styles.card : styles.missing} />;\n"
        }]
    });

    let response = plan(request);

    // `styles.missing` has no rule, so nothing retains the stylesheet,
    // but deleting it would leave the source importing a missing file.
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    let files = response["files"].as_array().unwrap();
    let migrated = files
        .iter()
        .find(|file| file["path"] == "/project/Card.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    assert!(migrated.contains("import styles from './Card.module.css';"));
    assert!(migrated.contains("className={maybe ? \"p-[13px]\" : styles.missing}"));
    let emptied = files
        .iter()
        .find(|file| file["path"] == "/project/Card.module.css")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    assert!(emptied.trim().is_empty());
}

#[test]
fn keeps_an_emptied_module_and_import_for_a_dangling_static_member() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <><div className={styles.card} /><span className={styles.missing} /></>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    let files = response["files"].as_array().unwrap();
    let migrated = files
        .iter()
        .find(|file| file["path"] == "/project/Card.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    assert!(migrated.contains("import styles from './Card.module.css';"));
    assert!(migrated.contains("className={styles.missing}"));
}

#[test]
fn warns_once_on_a_foreign_member_leaf_beside_a_module_branch() {
    let request = serde_json::json!({
        "stylesheets": [
            {
                "cssPath": "/project/Card.module.css",
                "cssSource": ".card { padding: 13px; }\n"
            },
            {
                "cssPath": "/project/global.css",
                "cssSource": ".base { margin: 7px; }\n"
            }
        ],
        "files": [{
            "path": "/project/App.tsx",
            "source": "import styles from './Card.module.css';\nexport const App = ({ maybe, ...rest }) => <div className={maybe ? styles.card : rest.className} />;\n"
        }]
    });

    let response = plan_batch(request);

    // The module pass keeps foreign members silent so another module's
    // binding never draws a spurious warning from this stylesheet's plan;
    // the global pass owns the dynamic-class-name diagnostic for the
    // opaque member leaf.
    assert_eq!(response["convertedRules"], 1);
    let migrated = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/App.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    assert!(migrated.contains("className={maybe ? \"p-[13px]\" : rest.className}"));
    assert_eq!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "dynamic-class-name")
            .count(),
        1
    );
}

#[test]
fn treats_nullish_leaves_as_warning_free_noops() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = ({ a, b, c, d }) => <div>\n<span className={a ? 'card' : null} />\n<span className={b ? 'card' : undefined} />\n<span className={c ? 'card' : false} />\n<span className={d ? 'card' : ''} />\n</div>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Card = ({ a, b, c, d }) => <div>\n<span className={a ? 'card p-[13px]' : null} />\n<span className={b ? 'card p-[13px]' : undefined} />\n<span className={c ? 'card p-[13px]' : false} />\n<span className={d ? 'card p-[13px]' : ''} />\n</div>;\n"
    );
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] == "retained-global-rule")
    );
}

#[test]
fn migrates_nested_and_wrapped_expression_results() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".a { padding: 13px; }\n.b { margin: 7px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = ({ maybe, ready }: { maybe: boolean, ready: boolean }) => <div className={(maybe ? ('a' as string) : (ready && 'b'))} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Card = ({ maybe, ready }: { maybe: boolean, ready: boolean }) => <div className={(maybe ? ('a p-[13px]' as string) : (ready && 'b m-[7px]'))} />;\n"
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
fn a_second_run_over_a_migrated_conditional_expression_is_a_no_op() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".a { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = ({ maybe }) => <div className={maybe ? 'a p-[13px]' : 'b'} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["files"], serde_json::json!([]));
}

#[test]
fn migrates_conditional_css_module_branches() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".a { padding: 13px; }\n.b { margin: 7px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = ({ maybe }) => <div className={maybe ? styles.a : styles.b} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 2);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Card.module.css"])
    );
    assert_eq!(
        response["files"][0]["source"],
        "export const Card = ({ maybe }) => <div className={maybe ? \"p-[13px]\" : \"m-[7px]\"} />;\n"
    );
}

#[test]
fn allows_overlapping_utilities_in_opposite_conditional_branches() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".a { padding: 8px; }\n.b { padding: 4px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = ({ maybe }) => <div className={maybe ? styles.a : styles.b} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 2);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Card.module.css"])
    );
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] != "module-utilities-conflict")
    );
}

#[test]
fn keeps_an_opaque_module_condition_and_its_rule() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".enabled { color: red; }\n.a { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.enabled ? styles.a : null} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    let source = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/Card.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    assert!(source.contains("import styles from './Card.module.css';"));
    assert!(source.contains("className={styles.enabled ? \"p-[13px]\" : null}"));
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["code"] != "dynamic-class-name")
    );
}

#[test]
fn permits_partial_conversion_beside_an_unsupported_branch() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".a { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = ({ maybe }) => <div className={maybe ? styles.a : getClass()} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Card.module.css"])
    );
    assert_eq!(
        response["files"][0]["source"],
        "export const Card = ({ maybe }) => <div className={maybe ? \"p-[13px]\" : getClass()} />;\n"
    );
    let dynamic = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["code"] == "dynamic-class-name")
        .unwrap();
    assert_eq!(dynamic["file"], "/project/Card.tsx");
}

#[test]
fn warns_when_a_conditional_leaf_utility_conflicts_with_its_own_classes() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = ({ maybe }) => <div className={maybe ? `${styles.card} p-2` : null} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(
        response["warnings"][0]["code"],
        "existing-tailwind-conflict"
    );
    let source = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/Card.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    assert!(source.contains("className={maybe ? \"p-[13px] p-2\" : null}"));
}

#[test]
fn keeps_a_preserved_module_member_inside_its_conditional_branch() {
    let css = ".a { padding: 8px; }\n.a:hover { color: red; }\n";
    let blocked_end = ".a { padding: 8px; }".len();
    let source = "import styles from './A.module.css';\nexport const App = ({ maybe }) => <div className={maybe ? styles.a : null} />;\n";
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/A.module.css",
            "cssSource": css,
            "blockedRules": [{ "start": 0, "end": blocked_end }]
        }],
        "files": [{
            "path": "/project/App.tsx",
            "source": source
        }]
    });

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 1);
    let migrated = response["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/project/App.tsx")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    // The candidate joins the member inside the branch; hoisting it to a
    // static class attribute would apply it unconditionally.
    assert!(migrated.contains("className={maybe ? `${styles.a}${\" hover:text-[red]\"}` : null}"));

    let second = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/A.module.css",
            "cssSource": ".a { padding: 8px; }\n",
            "blockedRules": [{ "start": 0, "end": blocked_end }]
        }],
        "files": [{
            "path": "/project/App.tsx",
            "source": migrated
        }]
    });
    let second = plan_batch(second);
    assert_eq!(second["files"], serde_json::json!([]));
}

#[test]
fn a_second_run_over_a_partially_migrated_module_expression_is_a_no_op() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".enabled { color: red; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.enabled ? \"p-[13px]\" : null} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
}

#[test]
fn retains_unsupported_result_forms_in_expressions() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = ({ a, b, c, d }) => <div>\n<span className={a ? ['card'] : 'card'} />\n<span className={b ? { card: true } : 'card'} />\n<span className={c ? clsx('card') : 'card'} />\n<span className={d ? `card-${d}` : 'card'} />\n</div>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "dynamic-class-name")
            .count(),
        4
    );
    assert_eq!(
        response["files"][0]["source"],
        "export const Card = ({ a, b, c, d }) => <div>\n<span className={a ? ['card'] : 'card p-[13px]'} />\n<span className={b ? { card: true } : 'card p-[13px]'} />\n<span className={c ? clsx('card') : 'card p-[13px]'} />\n<span className={d ? `card-${d}` : 'card p-[13px]'} />\n</div>;\n"
    );
}

#[test]
fn quotes_a_global_candidate_containing_double_quotes() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": "#hero { content: \"\\\"\"; }\n",
        "files": [{
            "path": "/project/Hero.tsx",
            "source": "export const Hero = () => <main id=\"hero\" />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(
        response["files"][0]["source"],
        "export const Hero = () => <main id=\"hero\" className='[content:\"\\\"\"]' />;\n"
    );
}

#[test]
fn a_second_run_over_a_migrated_global_expression_literal_is_a_no_op() {
    let request = serde_json::json!({
        "cssPath": "/project/global.css",
        "cssSource": ".card { padding: 13px; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "export const Card = () => <div className=\"card p-[13px]\" />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["files"], serde_json::json!([]));
}

#[test]
fn keeps_a_module_reference_when_a_sibling_rule_is_unsupported() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n.card::before { content: 'x'; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["files"], serde_json::json!([]));
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
}

#[test]
fn keeps_a_module_import_when_any_rule_is_retained() {
    let request = serde_json::json!({
        "cssPath": "/project/Card.module.css",
        "cssSource": ".card { padding: 13px; }\n.other { display: grid; }\n",
        "files": [{
            "path": "/project/Card.tsx",
            "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
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

    assert!(source.contains("import styles from './Card.module.css'"));
    assert!(source.contains("className=\"p-[13px]\""));
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
}

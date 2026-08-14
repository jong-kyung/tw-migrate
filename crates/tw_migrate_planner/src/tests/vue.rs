use super::*;

#[test]
fn plans_a_direct_css_module_padding_migration() {
    let request = serde_json::json!({
        "cssPath": "/project/Button.module.css",
        "cssSource": ".button { padding: 13px; }\n",
        "files": [{
            "path": "/project/Button.tsx",
            "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        }]
    });

    let response = plan(request);

    assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 0);
    assert_eq!(
        response["files"][0]["source"],
        "export const Button = () => <button className=\"p-[13px]\">Save</button>;\n"
    );
    assert_eq!(
        response["deletedFiles"],
        serde_json::json!(["/project/Button.module.css"])
    );
}

fn vue_module_request(source: &str, bindings: &[(&str, &str)]) -> serde_json::Value {
    let content_start = source.find("<style module>").unwrap() + "<style module>".len();
    let content_end = source.find("</style>").unwrap();
    let elements = bindings
        .iter()
        .map(|(tag, name)| {
            let binding = format!(":class=\"$style.{name}\"");
            let attr_start = source.find(&binding).unwrap();
            serde_json::json!({
                "tag": tag,
                "moduleBinding": {
                    "name": name,
                    "start": attr_start - 1,
                    "end": attr_start + binding.len(),
                },
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/Card.vue",
            "cssSource": source,
            "isModule": true,
            "syntax": "css",
            "vueModule": true,
            "vueBlocks": [{
                "outerStart": source.find("<style module>").unwrap(),
                "outerEnd": content_end + "</style>".len(),
                "contentStart": content_start,
                "contentEnd": content_end,
            }],
        }],
        "files": [{
            "path": "/project/Card.vue",
            "source": source,
            "htmlElements": elements,
            "htmlStylesheets": [{
                "cssPath": "/project/Card.vue",
                "variants": [],
                "direct": true,
                "analyzable": true,
            }],
            "htmlReferencesSafe": true,
        }],
    })
}

#[test]
fn vue_module_bindings_rewrite_and_delete_the_emptied_block() {
    let source = "<template>\n  <p :class=\"$style.card\">A</p>\n</template>\n<style module>\n.card { padding: 13px; }\n</style>\n";
    let request = vue_module_request(source, &[("p", "card")]);
    let response = plan_batch(request);
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(
        response["files"][0]["source"],
        "<template>\n  <p class=\"p-[13px]\">A</p>\n</template>\n"
    );
}

#[test]
fn vue_module_rules_ignore_same_named_corpus_classes_but_respect_site_tags() {
    let source = "<template>\n  <p :class=\"$style.card\">A</p>\n</template>\n<style module>\n.card { padding: 13px; }\n</style>\n";
    // A corpus `.card` cannot match the hashed runtime class.
    let mut hashed = vue_module_request(source, &[("p", "card")]);
    hashed["stylesheets"][0]["vueShadowCss"] = serde_json::json!([".card { padding: 20px; }"]);
    let response = plan_batch(hashed);
    assert_eq!(response["convertedRules"], 1);

    // A corpus type selector still reaches the binding's element.
    let mut typed = vue_module_request(source, &[("p", "card")]);
    typed["stylesheets"][0]["vueShadowCss"] = serde_json::json!(["p { padding: 20px; }"]);
    let response = plan_batch(typed);
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["warnings"][0]["code"], "shadowed-scoped-rule");
}

#[test]
fn vue_module_rewrites_withhold_quote_bearing_candidates() {
    let source = "<template>\n  <p :class=\"$style.card\">A</p>\n</template>\n<style module>\n.card { font-family: \"My Font\", sans-serif; }\n</style>\n";
    let request = vue_module_request(source, &[("p", "card")]);
    let response = plan_batch(request);
    // The double-quoted rewritten attribute cannot hold the candidate.
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(response["files"], serde_json::json!([]));
}

fn vue_batch_request(
    source: &str,
    is_module: bool,
    vue_retention: Option<&str>,
) -> serde_json::Value {
    let class_sites = ["card", "note"]
        .iter()
        .filter_map(|class| {
            let value_start = source.find(&format!("class=\"{class}\""))? + "class=\"".len();
            Some(serde_json::json!({
                "tag": "p",
                "classAttribute": {
                    "value": class,
                    "start": value_start,
                    "end": value_start + class.len(),
                },
            }))
        })
        .collect::<Vec<_>>();
    let outer_start = source.find("<style scoped>").unwrap();
    let content_start = outer_start + "<style scoped>".len();
    let content_end = source.find("</style>").unwrap();
    serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/Card.vue",
            "cssSource": source,
            "isModule": is_module,
            "syntax": "css",
            "vueBlocks": [{
                "outerStart": outer_start,
                "outerEnd": content_end + "</style>".len(),
                "contentStart": content_start,
                "contentEnd": content_end,
            }],
            "vueRetention": vue_retention,
        }],
        "files": [{
            "path": "/project/Card.vue",
            "source": source,
            "htmlElements": class_sites,
            "htmlStylesheets": [{
                "cssPath": "/project/Card.vue",
                "variants": [],
                "direct": true,
                "analyzable": true,
            }],
            "htmlReferencesSafe": true,
        }],
    })
}

#[test]
fn vue_closed_sfc_migrates_template_and_removes_the_emptied_scoped_block() {
    let source = "<template>\n  <p class=\"card\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n";
    let request = vue_batch_request(source, true, None);

    let response = plan_batch(request);

    assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 0);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(response["unlinkedFiles"], serde_json::json!([]));
    assert_eq!(response["files"].as_array().unwrap().len(), 1);
    assert_eq!(response["files"][0]["path"], "/project/Card.vue");
    assert_eq!(
        response["files"][0]["source"],
        "<template>\n  <p class=\"card p-[13px]\">A</p>\n  <p class=\"note\">B</p>\n</template>\n"
    );
}

#[test]
fn vue_shadowed_scoped_rule_is_retained_without_template_edits() {
    let source = "<template>\n  <p class=\"card\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n";
    let mut request = vue_batch_request(source, true, None);
    request["stylesheets"][0]["vueShadowCss"] =
        serde_json::json!(["div.card { padding: 20px; } .cardio { top: 0; }"]);

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(response["files"], serde_json::json!([]));
    let warning = &response["warnings"][0];
    assert_eq!(warning["code"], "shadowed-scoped-rule");
    assert_eq!(warning["file"], "/project/Card.vue");

    // A different class like `.cardio` must not shadow `.card`.
    let mut clear = vue_batch_request(source, true, None);
    clear["stylesheets"][0]["vueShadowCss"] = serde_json::json!([".cardio { top: 0; }"]);
    let response = plan_batch(clear);
    assert_eq!(response["convertedRules"], 1);
}

#[test]
fn vue_cooccurring_site_class_shadow_retains_the_rule() {
    let source = "<template>\n  <p class=\"card foo\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n";
    let value_start = source.find("card foo").unwrap();
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/Card.vue",
            "cssSource": source,
            "isModule": true,
            "syntax": "css",
            "vueBlocks": [{
                "outerStart": source.find("<style scoped>").unwrap(),
                "outerEnd": source.find("</style>").unwrap() + "</style>".len(),
                "contentStart": source.find("<style scoped>").unwrap() + "<style scoped>".len(),
                "contentEnd": source.find("</style>").unwrap(),
            }],
            "vueShadowCss": [".foo { padding: 20px; }"],
        }],
        "files": [{
            "path": "/project/Card.vue",
            "source": source,
            "htmlElements": [{
                "tag": "p",
                "classAttribute": {
                    "value": "card foo",
                    "start": value_start,
                    "end": value_start + "card foo".len(),
                },
            }],
            "htmlStylesheets": [{
                "cssPath": "/project/Card.vue",
                "variants": [],
                "direct": true,
                "analyzable": true,
            }],
            "htmlReferencesSafe": true,
        }],
    });

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["warnings"][0]["code"], "shadowed-scoped-rule");
}

#[test]
fn vue_retained_sibling_rule_shadows_cooccurring_site_classes() {
    let source = "<template>\n  <p class=\"card legacy\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n.legacy { padding: 20px !important; }\n.card { padding: 13px; }\n.note { margin: 3px; }\n</style>\n";
    let value_start = source.find("card legacy").unwrap();
    let note_start = source.find("\"note\"").unwrap() + 1;
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/Card.vue",
            "cssSource": source,
            "isModule": true,
            "syntax": "css",
            "vueBlocks": [{
                "outerStart": source.find("<style scoped>").unwrap(),
                "outerEnd": source.find("</style>").unwrap() + "</style>".len(),
                "contentStart": source.find("<style scoped>").unwrap() + "<style scoped>".len(),
                "contentEnd": source.find("</style>").unwrap(),
            }],
        }],
        "files": [{
            "path": "/project/Card.vue",
            "source": source,
            "htmlElements": [{
                "tag": "p",
                "classAttribute": {
                    "value": "card legacy",
                    "start": value_start,
                    "end": value_start + "card legacy".len(),
                },
            }, {
                "tag": "p",
                "classAttribute": {
                    "value": "note",
                    "start": note_start,
                    "end": note_start + "note".len(),
                },
            }],
            "htmlStylesheets": [{
                "cssPath": "/project/Card.vue",
                "variants": [],
                "direct": true,
                "analyzable": true,
            }],
            "htmlReferencesSafe": true,
        }],
    });

    let response = plan_batch(request);

    // `.legacy` retains on `!important`; deleting `.card` would expose
    // it on the shared element, so `.card` retains as shadowed while the
    // unrelated `.note` still converts.
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 2);
    let codes = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning["code"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"shadowed-scoped-rule".to_string()));
    assert!(codes.contains(&"unsupported-important".to_string()));
}

#[test]
fn vue_quote_blocked_rule_shadows_cooccurring_sibling() {
    let source = "<template>\n  <p class=\"bad good\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n.bad { font-family: \"My Font\"; }\n.good { font-family: Arial; }\n</style>\n";
    let value_start = source.find("bad good").unwrap();
    let request = serde_json::json!({
        "stylesheets": [{
            "cssPath": "/project/Card.vue",
            "cssSource": source,
            "isModule": true,
            "syntax": "css",
            "vueBlocks": [{
                "outerStart": source.find("<style scoped>").unwrap(),
                "outerEnd": source.find("</style>").unwrap() + "</style>".len(),
                "contentStart": source.find("<style scoped>").unwrap() + "<style scoped>".len(),
                "contentEnd": source.find("</style>").unwrap(),
            }],
        }],
        "files": [{
            "path": "/project/Card.vue",
            "source": source,
            "htmlElements": [{
                "tag": "p",
                "classAttribute": {
                    "value": "bad good",
                    "start": value_start,
                    "end": value_start + "bad good".len(),
                },
            }],
            "htmlStylesheets": [{
                "cssPath": "/project/Card.vue",
                "variants": [],
                "direct": true,
                "analyzable": true,
            }],
            "htmlReferencesSafe": true,
        }],
    });

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    assert_eq!(response["files"], serde_json::json!([]));
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "shadowed-scoped-rule")
    );
}

#[test]
fn vue_retained_unbounded_target_shadows_sibling_rules() {
    let source = "<template>\n  <div class=\"ancestor\"><p class=\"card\">A</p></div>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n.ancestor > * { padding: 20px !important; }\n.card { padding: 13px; }\n</style>\n";
    let request = vue_batch_request(source, true, None);

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 2);
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "shadowed-scoped-rule")
    );
}

#[test]
fn vue_retained_conditional_with_unbounded_target_shadows_sibling_rules() {
    let source = "<template>\n  <div class=\"ancestor\"><p class=\"card\">A</p></div>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n@supports (content: \"x\") { .ancestor > * { padding: 20px; } }\n.card { padding: 13px; }\n</style>\n";
    let request = vue_batch_request(source, true, None);

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 0);
    assert!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "shadowed-scoped-rule")
    );
}

#[test]
fn vue_module_shadow_indexes_only_global_selectors() {
    let source = "<template>\n  <p class=\"card\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n";
    // A localized module class cannot match template elements.
    let mut localized = vue_batch_request(source, true, None);
    localized["stylesheets"][0]["vueShadowModuleCss"] =
        serde_json::json!([".card { padding: 20px; }"]);
    let response = plan_batch(localized);
    assert_eq!(response["convertedRules"], 1);

    // A bare type selector in a module stays global and shadows the site.
    let mut typed = vue_batch_request(source, true, None);
    typed["stylesheets"][0]["vueShadowModuleCss"] = serde_json::json!(["p { padding: 20px; }"]);
    let response = plan_batch(typed);
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["warnings"][0]["code"], "shadowed-scoped-rule");
}

#[test]
fn vue_retained_keyframes_do_not_shadow_sibling_rules() {
    let source = "<template>\n  <p class=\"card\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n@keyframes spin { from { opacity: 0; } to { opacity: 1; } }\n.card { padding: 13px; }\n</style>\n";
    let request = vue_batch_request(source, true, None);
    let response = plan_batch(request);
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 1);
}

#[test]
fn vue_retained_definition_at_rules_do_not_shadow_sibling_rules() {
    let source = "<template>\n  <p class=\"card\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n@property --accent { syntax: \"<color>\"; inherits: false; initial-value: red; }\n.card { padding: 13px; }\n</style>\n";
    let request = vue_batch_request(source, true, None);
    let response = plan_batch(request);
    assert_eq!(response["convertedRules"], 1);
    assert_eq!(response["retainedRules"], 1);
}

#[test]
fn vue_module_global_escapes_are_indexed_or_retained_conservatively() {
    let source = "<template>\n  <p class=\"card\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n";
    // An unrelated `:global` escape must not disable the whole index.
    let mut unrelated = vue_batch_request(source, true, None);
    unrelated["stylesheets"][0]["vueShadowModuleCss"] =
        serde_json::json!([":global(.unrelated) { padding: 20px; }"]);
    let response = plan_batch(unrelated);
    assert_eq!(response["convertedRules"], 1);

    // A matching `:global` escape shadows precisely.
    let mut matching = vue_batch_request(source, true, None);
    matching["stylesheets"][0]["vueShadowModuleCss"] =
        serde_json::json!([":global(.card) { padding: 20px; }"]);
    let response = plan_batch(matching);
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["warnings"][0]["code"], "shadowed-scoped-rule");

    // Selector-mode `:global` is not represented by a pseudo-class
    // argument, so conservatively retain rather than treating `.card` as
    // a localized module class.
    let mut selector_mode = vue_batch_request(source, true, None);
    selector_mode["stylesheets"][0]["vueShadowModuleCss"] =
        serde_json::json!([":global .card { padding: 20px; }"]);
    let response = plan_batch(selector_mode);
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["warnings"][0]["code"], "shadowed-scoped-rule");
}

#[test]
fn vue_v_bind_declarations_are_never_converted() {
    let source = "<template>\n  <p class=\"card\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n.card { color: v-bind(theme); }\n</style>\n";
    let request = vue_batch_request(source, true, None);
    let response = plan_batch(request);
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(response["warnings"][0]["code"], "unsupported-value");
}

#[test]
fn vue_type_selector_shadow_matches_by_site_tag() {
    let source = "<template>\n  <p class=\"card\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n";
    // A type selector for another tag cannot match the `p` sites.
    let mut clear = vue_batch_request(source, true, None);
    clear["stylesheets"][0]["vueShadowCss"] = serde_json::json!(["article { padding: 20px; }"]);
    let response = plan_batch(clear);
    assert_eq!(response["convertedRules"], 1);

    // A `p` type selector reaches the rule's site, so the rule retains.
    let mut shadowed = vue_batch_request(source, true, None);
    shadowed["stylesheets"][0]["vueShadowCss"] = serde_json::json!(["p { padding: 20px; }"]);
    let response = plan_batch(shadowed);
    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["warnings"][0]["code"], "shadowed-scoped-rule");

    // An unparseable piece is unverifiable and retains everything.
    let mut opaque = vue_batch_request(source, true, None);
    opaque["stylesheets"][0]["vueShadowCss"] = serde_json::json!(["$name: card;"]);
    let response = plan_batch(opaque);
    assert_eq!(response["convertedRules"], 0);
}

#[test]
fn vue_unverifiable_shadow_corpus_retains_every_closed_rule() {
    let source = "<template>\n  <p class=\"card\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n";
    let mut request = vue_batch_request(source, true, None);
    request["stylesheets"][0]["vueShadowUnverifiable"] = serde_json::json!(true);

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(response["warnings"][0]["code"], "shadowed-scoped-rule");
}

#[test]
fn vue_preserves_conditionals_that_were_already_empty() {
    let source = "<template>\n  <p class=\"card\">A</p>\n  <p class=\"note\">B</p>\n</template>\n<style scoped>\n@media print {\n}\n.card { padding: 13px; }\n</style>\n";
    let request = vue_batch_request(source, true, None);

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 1);
    let migrated = response["files"][0]["source"].as_str().unwrap();
    assert!(migrated.contains("@media print {"));
    assert!(migrated.contains("<style scoped>"));
    assert!(!migrated.contains(".card {"));
}

#[test]
fn vue_open_sfc_appends_utilities_and_retains_the_scoped_rule() {
    let source = "<template>\n  <p class=\"card\">A</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n";
    let request = vue_batch_request(source, false, Some("open-root-fallthrough"));

    let response = plan_batch(request);

    assert_eq!(response["convertedRules"], 0);
    assert_eq!(response["retainedRules"], 1);
    assert_eq!(response["deletedFiles"], serde_json::json!([]));
    assert_eq!(
        response["files"][0]["source"],
        "<template>\n  <p class=\"card p-[13px]\">A</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n"
    );
    let warning = response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["code"] == "open-root-fallthrough")
        .expect("open-root-fallthrough warning");
    assert_eq!(warning["file"], "/project/Card.vue");
    let rule_start = source.find(".card {").unwrap();
    assert_eq!(warning["start"], rule_start);
}

use serde_json::{Value, json};

fn collect(request: Value) -> Value {
    let response = super::collect_media_conditions_json(&request.to_string()).unwrap();
    serde_json::from_str(&response).unwrap()
}

fn rem_tokens() -> Value {
    json!({ "breakpoint-md": "48rem", "breakpoint-lg": "64rem" })
}

#[test]
fn whole_matching_queries_are_collected_for_verification() {
    let response = collect(json!({
        "stylesheets": [{
            "cssPath": "card.css",
            "cssSource": "@media print { .card { margin: 0; } }\n\
                @media (prefers-color-scheme: dark) { .card { color: white; } }\n\
                @media (min-width: 48rem) { .card { padding: 1rem; } }",
        }],
        "themeTokens": rem_tokens(),
    }));
    let components = response["components"].as_array().unwrap();
    // Nothing is converted whole: built-in and breakpoint names may
    // both be shadowed, so every match must reach the resolver's
    // effective-expansion verification.
    assert_eq!(components.len(), 3);
    assert_eq!(components[0]["key"], "(prefers-color-scheme: dark)");
    assert_eq!(components[0]["builtin"], "dark");
    assert_eq!(components[1]["key"], "(width >= 48rem)");
    assert_eq!(components[1]["breakpoint"], "md");
    assert_eq!(components[2]["key"], "print");
    assert_eq!(components[2]["builtin"], "print");
}

#[test]
fn legacy_pairs_preserve_inclusive_upper_bounds_exactly() {
    let response = collect(json!({
        "stylesheets": [{
            "cssPath": "card.css",
            "cssSource": "@media (min-width: 48rem) and (max-width: 63.999rem) { .card { margin: 0; } }",
        }],
        "themeTokens": rem_tokens(),
    }));
    let components = response["components"].as_array().unwrap();
    assert_eq!(components.len(), 2);
    assert_eq!(components[0]["key"], "(width <= 63.999rem)");
    // The shipped epsilon approximation onto max-lg does not carry into
    // extraction; the inclusive bound keeps its exact meaning.
    assert_eq!(components[0]["breakpoint"], Value::Null);
    assert_eq!(components[0]["readableName"], "width-lte-63p999rem");
    assert_eq!(components[1]["key"], "(width >= 48rem)");
    assert_eq!(components[1]["breakpoint"], "md");
}

#[test]
fn non_css_whitespace_in_the_prelude_stays_conservative() {
    // `\u{a0}screen` is an exotic, match-nothing media type; collecting
    // it as `screen` would broaden the rule to screen media. U+00A0 is
    // not CSS whitespace, and is_ascii_whitespace matches exactly the
    // CSS set, so it survives trimming as identifier content and stays
    // unrepresentable.
    let response = collect(json!({
        "stylesheets": [{
            "cssPath": "card.css",
            "cssSource": "@media \u{a0}screen { .card { margin: 0; } }",
        }],
        "themeTokens": rem_tokens(),
    }));
    assert_eq!(response["components"].as_array().unwrap().len(), 0);

    // U+000B is rejected by the CSS parser in a prelude outright, so
    // such a stylesheet fails parsing before collection ever sees it
    // and can never be collected as an ordinary `screen` condition.
    let error = super::collect_media_conditions_json(
        &json!({
            "stylesheets": [{
                "cssPath": "card.css",
                "cssSource": "@media \u{b}screen { .card { margin: 0; } }",
            }],
        })
        .to_string(),
    )
    .unwrap_err();
    assert!(
        error.to_string().starts_with("Failed to parse card.css"),
        "{error}"
    );
}

#[test]
fn decomposes_compounds_and_reports_matches_per_component() {
    let response = collect(json!({
        "stylesheets": [{
            "cssPath": "card.css",
            "cssSource": "@media screen and (prefers-color-scheme: dark) { .card { color: white; } }\n\
                @media (48rem <= width < 64rem) { .card { margin: 0; } }",
        }],
        "themeTokens": rem_tokens(),
    }));
    let components = response["components"].as_array().unwrap();
    let by_key: std::collections::HashMap<_, _> = components
        .iter()
        .map(|component| (component["key"].as_str().unwrap(), component))
        .collect();
    assert_eq!(components.len(), 4);
    assert_eq!(by_key["(prefers-color-scheme: dark)"]["builtin"], "dark");
    assert_eq!(by_key["screen"]["builtin"], Value::Null);
    assert_eq!(by_key["screen"]["readableName"], "screen");
    assert_eq!(by_key["(width >= 48rem)"]["breakpoint"], "md");
    assert_eq!(by_key["(width < 64rem)"]["breakpoint"], "max-lg");
}

#[test]
fn components_deduplicate_across_conditions_and_stylesheets() {
    let response = collect(json!({
        "stylesheets": [
            {
                "cssPath": "a.css",
                "cssSource": ".card { @media screen and (width <= 768px) { margin: 0; } }",
            },
            {
                "cssPath": "b.css",
                "cssSource": "@media screen and (hover: hover) { .other { margin: 1px; } }\n\
                    @media (max-width: 768px) { .other { color: red; } }",
            },
        ],
        "themeTokens": rem_tokens(),
    }));
    let components = response["components"].as_array().unwrap();
    let keys: Vec<_> = components
        .iter()
        .map(|component| component["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["(hover: hover)", "(width <= 768px)", "screen"]);
    let screen = &components[2];
    assert_eq!(screen["cssPath"], "a.css");
    assert_eq!(screen["order"], 1);
}

#[test]
fn whole_conditions_report_one_unit() {
    let response = collect(json!({
        "stylesheets": [{
            "cssPath": "card.css",
            "cssSource": "@media screen, print { .card { margin: 0; } }\n\
                @media (min-width: calc(100vw - 2rem)) { .card { color: red; } }",
        }],
        "themeTokens": rem_tokens(),
    }));
    let components = response["components"].as_array().unwrap();
    assert_eq!(components.len(), 2);
    assert_eq!(components[0]["key"], "(width >= calc(100vw - 2rem))");
    assert_eq!(components[0]["whole"], false);
    assert_eq!(components[0]["readableName"], Value::Null);
    assert_eq!(components[1]["key"], "screen, print");
    assert_eq!(components[1]["whole"], true);
    assert_eq!(components[1]["readableName"], "screen-or-print");
}

#[test]
fn equality_queries_never_reuse_breakpoints() {
    let response = collect(json!({
        "stylesheets": [{
            "cssPath": "card.css",
            "cssSource": "@media (width = 48rem) { .card { margin: 0; } }",
        }],
        "themeTokens": rem_tokens(),
    }));
    let components = response["components"].as_array().unwrap();
    assert_eq!(components.len(), 1);
    assert_eq!(components[0]["key"], "(width = 48rem)");
    // A single-width condition must not broaden into md or max-md.
    assert_eq!(components[0]["breakpoint"], Value::Null);
    assert_eq!(components[0]["readableName"], "width-eq-48rem");
}

#[test]
fn collects_conditions_from_vue_style_blocks() {
    let sfc = "<template><div class=\"card\"></div></template>\n\
            <style scoped>@media (min-width: 52rem) { .card { margin: 0; } }</style>";
    let content_start = sfc.find("<style scoped>").unwrap() + "<style scoped>".len();
    let content_end = sfc.find("</style>").unwrap();
    let response = collect(json!({
        "stylesheets": [{
            "cssPath": "Card.vue",
            "cssSource": sfc,
            "vueBlocks": [{ "contentStart": content_start, "contentEnd": content_end }],
        }],
        "themeTokens": rem_tokens(),
    }));
    let components = response["components"].as_array().unwrap();
    assert_eq!(components.len(), 1);
    assert_eq!(components[0]["key"], "(width >= 52rem)");
    assert_eq!(components[0]["cssPath"], "Card.vue");
}

#[test]
fn parses_authored_custom_variant_reservations() {
    let response = collect(json!({
        "stylesheets": [],
        "themeTokens": rem_tokens(),
        "tailwindSources": [{
            "path": "app.css",
            "source": "@custom-variant width-lte-768px {\n  @media (width <= 768px) {\n    @slot;\n  }\n}\n\
                @custom-variant hocus (&:hover, &:focus);\n\
                @custom-variant both {\n  @media screen and (width <= 768px) {\n    @slot;\n  }\n}",
        }],
    }));
    let variants = response["authoredVariants"].as_array().unwrap();
    assert_eq!(variants.len(), 3);
    assert_eq!(variants[0]["name"], "both");
    assert_eq!(variants[0]["mediaQueryKey"], Value::Null);
    assert_eq!(variants[1]["name"], "hocus");
    assert_eq!(variants[1]["mediaQueryKey"], Value::Null);
    assert_eq!(variants[2]["name"], "width-lte-768px");
    assert_eq!(variants[2]["mediaQueryKey"], "(width <= 768px)");
}

#[test]
fn output_is_deterministic() {
    let request = json!({
        "stylesheets": [{
            "cssPath": "card.css",
            "cssSource": "@media (width <= 900px) { .a { margin: 0; } }\n\
                @media (width <= 768px) { .b { margin: 0; } }",
        }],
        "themeTokens": rem_tokens(),
    });
    assert_eq!(collect(request.clone()), collect(request));
}

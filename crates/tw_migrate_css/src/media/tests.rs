use super::*;

fn components(query: &str) -> Vec<MediaComponent> {
    match parse_media_condition(query).expect(query) {
        ParsedMediaCondition::Components(components) => components,
        ParsedMediaCondition::Whole(_) => panic!("expected components for {query}"),
    }
}

fn whole(query: &str) -> MediaComponent {
    match parse_media_condition(query).expect(query) {
        ParsedMediaCondition::Whole(component) => component,
        ParsedMediaCondition::Components(_) => panic!("expected whole for {query}"),
    }
}

fn keys(query: &str) -> Vec<String> {
    components(query)
        .into_iter()
        .map(|component| component.key)
        .collect()
}

fn names(query: &str) -> Vec<Option<String>> {
    components(query)
        .into_iter()
        .map(|component| component.readable_name)
        .collect()
}

#[test]
fn decomposes_and_joined_conditions_in_authored_order() {
    assert_eq!(
        keys("screen and (width <= 768px)"),
        ["screen", "(width <= 768px)"]
    );
    assert_eq!(
        names("screen and (width <= 768px)"),
        [
            Some("screen".to_string()),
            Some("width-lte-768px".to_string())
        ]
    );
    assert_eq!(
        keys("screen and (prefers-color-scheme: dark) and (hover: hover)"),
        ["screen", "(prefers-color-scheme: dark)", "(hover: hover)"]
    );
}

#[test]
fn double_ranges_decompose_into_shared_bounds() {
    assert_eq!(
        keys("(48rem <= width < 60rem)"),
        ["(width >= 48rem)", "(width < 60rem)"]
    );
    assert_eq!(
        names("(48rem <= width < 60rem)"),
        [
            Some("width-gte-48rem".to_string()),
            Some("width-lt-60rem".to_string())
        ]
    );
    assert_eq!(keys("(min-width: 52rem)"), ["(width >= 52rem)"]);
    assert_eq!(
        keys("(60rem > width >= 48rem)"),
        ["(width >= 48rem)", "(width < 60rem)"]
    );
}

#[test]
fn width_bounds_expose_breakpoint_matching_shape() {
    let bounds: Vec<_> = components("(48rem <= width < 60rem)")
        .into_iter()
        .map(|component| component.width_bound.expect("bound"))
        .collect();
    assert!(bounds[0].lower && bounds[0].inclusive);
    assert_eq!(bounds[0].value, "48rem");
    assert!(!bounds[1].lower && !bounds[1].inclusive);
    assert_eq!(bounds[1].value, "60rem");
    assert!(components("(hover: hover)")[0].width_bound.is_none());
}

#[test]
fn builtin_lookup_text_is_compact() {
    let compound = components("screen and (prefers-color-scheme: dark)");
    assert_eq!(compound[0].builtin_query.as_deref(), Some("screen"));
    assert_eq!(
        compound[1].builtin_query.as_deref(),
        Some("(prefers-color-scheme:dark)")
    );
    assert_eq!(
        components("print")[0].builtin_query.as_deref(),
        Some("print")
    );
}

#[test]
fn modifiers_stay_attached_to_their_component() {
    let only = components("only screen and (color)");
    assert_eq!(only[0].key, "only screen");
    assert_eq!(only[0].readable_name.as_deref(), Some("only-screen"));
    assert!(only[0].builtin_query.is_none());

    let negated_type = components("not screen");
    assert_eq!(negated_type[0].key, "not screen");
    assert_eq!(negated_type[0].readable_name.as_deref(), Some("not-screen"));

    let negated_feature = components("not (hover)");
    assert_eq!(negated_feature[0].key, "not (hover)");
    assert_eq!(
        negated_feature[0].readable_name.as_deref(),
        Some("not-hover")
    );
    assert!(negated_feature[0].builtin_query.is_none());
}

#[test]
fn non_decomposable_conditions_keep_one_whole_key() {
    let comma = whole("screen, print");
    assert_eq!(comma.key, "screen, print");
    assert_eq!(comma.readable_name.as_deref(), Some("screen-or-print"));

    let or_joined = whole("(color) or (hover)");
    assert_eq!(or_joined.key, "(color) or (hover)");
    assert_eq!(or_joined.readable_name.as_deref(), Some("color-or-hover"));

    let negated = whole("not screen and (color)");
    assert_eq!(negated.key, "not screen and (color)");
    assert_eq!(
        negated.readable_name.as_deref(),
        Some("not-screen-and-color")
    );
}

#[test]
fn unclean_values_lose_only_their_readable_name() {
    let calc = components("screen and (min-width: calc(100vw - 2rem))");
    assert_eq!(calc[0].readable_name.as_deref(), Some("screen"));
    assert_eq!(calc[1].key, "(width >= calc(100vw - 2rem))");
    assert!(calc[1].readable_name.is_none());

    let env = components("(min-width: env(MyInset))");
    assert_eq!(env[0].key, "(width >= env(MyInset))");
    assert!(env[0].readable_name.is_none());

    let long_value = format!("(min-width: {}rem)", "1".repeat(60));
    assert!(components(&long_value)[0].readable_name.is_none());
}

#[test]
fn values_fold_case_and_whitespace_only() {
    // No numeric canonicalization: only case and ASCII whitespace fold,
    // and spellings the parser cannot prove identical keep distinct
    // keys. The worst outcome is one duplicate definition per spelling.
    assert_eq!(keys("(min-width: 52REM)"), ["(width >= 52rem)"]);
    for query in [
        "(min-width: +52rem)",
        "(min-width: 052rem)",
        "(min-width: 5.2e1rem)",
    ] {
        assert_ne!(keys(query), ["(width >= 52rem)"], "{query}");
    }
    assert_eq!(keys("(min-width: 47.5rem)"), ["(width >= 47.5rem)"]);
    assert_eq!(keys("(color: 1.0)"), ["(color: 1.0)"]);
    assert_ne!(keys("(min-width: 48rem)"), keys("(min-width: 768px)"));
}

#[test]
fn case_and_whitespace_rules_are_css_exact() {
    assert_eq!(
        keys("SCREEN AND (MIN-WIDTH: 52rem)"),
        keys("screen and (min-width: 52rem)")
    );
    assert_eq!(
        keys("(orientation:\u{a0}landscape)"),
        ["(orientation: \u{a0}landscape)"]
    );
    assert!(parse_media_condition("(\u{a0}orientation: landscape)").is_none());
    assert_eq!(
        keys("(orientation:\u{b}landscape)"),
        ["(orientation: \u{b}landscape)"]
    );
    // Comment placement can be significant inside function values such
    // as calc(), so commented preludes are rejected wholesale rather
    // than rewritten.
    for query in [
        "screen/**/and (color)",
        "(min-width:/* tablet */52rem)",
        "(min-width: calc(1px/**/+/**/2px))",
        "(min-width: /* 52rem)",
    ] {
        assert!(parse_media_condition(query).is_none(), "{query}");
    }
}

#[test]
fn unrepresentable_conditions_are_rejected() {
    for query in [
        "",
        "(--narrow)",
        "((min-width: 5em) and (max-width: 10em))",
        "layer and (min-width: 5em)",
        "or",
        "only (min-width: 5em)",
        "not(color)",
        "screen and(color)",
        "(color) or (hover) and (pointer: fine)",
        "screen or (color)",
        "not (color) or (hover)",
        "(width < 5em < 10em < 20em)",
        "{ }",
    ] {
        assert!(parse_media_condition(query).is_none(), "{query}");
    }
}

#[test]
fn equality_bounds_never_match_breakpoints() {
    let component = &components("(width = 48rem)")[0];
    assert_eq!(component.key, "(width = 48rem)");
    assert!(component.width_bound.is_none());
    assert_eq!(component.readable_name.as_deref(), Some("width-eq-48rem"));
}

#[test]
fn underscore_identifiers_are_representable_with_digest_names() {
    let media_type = &components("foo_bar")[0];
    assert_eq!(media_type.key, "foo_bar");
    assert!(media_type.readable_name.is_none());
    let feature = &components("(foo_bar: baz)")[0];
    assert_eq!(feature.key, "(foo_bar: baz)");
    assert!(feature.readable_name.is_none());
}

#[test]
fn vendor_prefixed_features_are_representable() {
    let component = &components("(-webkit-min-device-pixel-ratio: 2)")[0];
    assert_eq!(component.key, "(-webkit-min-device-pixel-ratio: 2)");
    assert_eq!(
        component.readable_name.as_deref(),
        Some("webkit-min-device-pixel-ratio-2")
    );
}

#[test]
fn digests_are_sixteen_hex_and_key_dependent() {
    let digest = condition_digest("(width >= 52rem)");
    assert_eq!(digest.len(), 16);
    assert!(
        digest
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    );
    assert_eq!(digest, condition_digest("(width >= 52rem)"));
    assert_ne!(digest, condition_digest("(width >= 60rem)"));
}

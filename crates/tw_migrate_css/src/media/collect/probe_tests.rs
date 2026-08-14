#[test]
fn extracts_single_media_wrapper_keys() {
    let probe = |css: &str| -> Option<String> {
        serde_json::from_str(&super::media_probe_key_json(css).unwrap()).unwrap()
    };
    assert_eq!(
        probe("@media (width >= 48rem) { .x { --tw-p: 1; } }"),
        Some("(width >= 48rem)".to_string())
    );
    assert_eq!(
        probe("@media (prefers-color-scheme: dark) { .x { --tw-p: 1; } }"),
        Some("(prefers-color-scheme: dark)".to_string())
    );
    assert_eq!(
        probe("@media screen, print { .x { --tw-p: 1; } }"),
        Some("screen, print".to_string())
    );
    // Escaped candidate classes stay a single bare class selector.
    assert_eq!(
        probe("@media (width >= 48rem) { .md\\:\\[--tw-probe\\:1\\] { --tw-p: 1; } }"),
        Some("(width >= 48rem)".to_string())
    );
    // A selector-based expansion, the shadowed-variant shape, never
    // yields a key.
    assert_eq!(probe(".dark .x { --tw-p: 1; }"), None);
    // Stacked output with two media levels is not a single unit.
    assert_eq!(
        probe("@media screen { @media (width <= 768px) { .x { --tw-p: 1; } } }"),
        None
    );
    // A variant that combines the media condition with a qualifying
    // selector re-scopes the utility, so the media key alone does not
    // describe it.
    assert_eq!(
        probe("@media (prefers-color-scheme: dark) { .x:where(.dark *) { --tw-p: 1; } }"),
        None
    );
    assert_eq!(
        probe("@media (prefers-color-scheme: dark) { .dark .x { --tw-p: 1; } }"),
        None
    );
    assert_eq!(
        probe("@media (prefers-color-scheme: dark) { .x.dark { --tw-p: 1; } }"),
        None
    );
    assert_eq!(
        probe("@media screen { .x { --tw-p: 1; } .y { --tw-p: 1; } }"),
        None
    );
}

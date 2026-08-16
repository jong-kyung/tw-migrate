use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
};

use oxc_css_parser::{Syntax, ast::Statement};
use serde::{Deserialize, Serialize};
use tw_migrate_error::{MigrationError, MigrationResult};

use tw_migrate_css::{
    KeyframePlan, MediaComponent, ParseOptions, ParsedCss, ParsedMediaCondition, RulePlan,
    SelectorKey, append_global_at_rules, append_keyframes, css_property_sets_conflict,
    index_shadow_selectors, is_conditional, parse_css, parse_css_rules, parse_dimension,
    parse_media_condition, tailwind_utilities_conflict, tailwind_utility_parts,
    tailwind_variants_match, validate_css, variant_segments,
};

mod batch;
mod consumer;
mod edit;
mod request;
mod response;
mod rule;
mod source_map;
mod stylesheet;
mod vue;

pub use batch::plan_batch_json;
#[cfg(test)]
use batch::plan_json;
use consumer::plan_consumer_file;
use edit::{
    apply_edits, collect_empty_conditionals, remove_empty_conditionals, validate_stylesheet,
};
use request::{BatchPlanRequest, BatchStylesheet, PlanRequest};
use response::{FontFamilyProbeReport, FontFamilyReport, PlanResponse, PlannedFile, RuleReport};
use rule::{CandidateMaps, RuleConflicts, RuleId, RuleOrigin, rule_id};
pub use source_map::decode_source_map_json;
use source_map::{SourceMapping, map_rule_spans, mentions_word};
use stylesheet::{
    batch_stylesheet_request, candidate_map_for_request, candidate_property_union,
    is_stylesheet_module, plan_request,
};
use tw_migrate_css::StylesheetSyntax;
use tw_migrate_source as jsx_graph;
use tw_migrate_source::{Edit, HtmlElement, SourceFile, Warning, original_offset};
use tw_migrate_source::{
    SourcePlan, candidates_fit_attribute, element_classes, element_has_context, element_ids,
    element_tag, opaque_reference_plan, plan_batch_source_file, plan_html_file,
    plan_vue_module_file, rebase_span, shift_offset, validate_js,
};
use vue::{
    VueBlock, finish_vue_stylesheet, is_vue_path, mask_vue_source, rebase_vue_blocks,
    rule_site_reachable, stamp_in_file_shadow, vue_retention_warning,
};

/// Every warning code the migration can emit, including reason strings that
/// flow through `rule.warning` and the JS-side `candidate-compilation-failure`
/// stamped in index.js. Adding or removing a code requires updating this list
/// and the README warning table; `tests::warning_codes_are_pinned_to_the_readme`
/// enforces both.
#[cfg(test)]
const WARNING_CODES: &[&str] = &[
    "aliased-css-module-reference",
    "batch-stylesheet-conflict",
    "candidate-compilation-failure",
    "component-class-target",
    "computed-css-module-reference",
    "cross-package-stylesheet-link",
    "css-module-composes",
    "dynamic-class-name",
    "dynamic-html-attribute",
    "dynamic-template-class",
    "existing-tailwind-conflict",
    "inferred-preprocessor-source",
    "media-query-definition-fallback",
    "module-utilities-conflict",
    "non-classname-css-module-reference",
    "open-root-fallthrough",
    "preprocessor-style-block",
    "rebuild-required",
    "reference-only-css-module-consumer",
    "retained-global-rule",
    "shadowed-scoped-rule",
    "shared-preprocessor-source",
    "unproven-css-module-relationship",
    "unproven-script-reference",
    "unproven-shared-entry-flow",
    "unproven-source-map",
    "unresolved-selector-target",
    "unscoped-style-block",
    "unsupported-animation",
    "unsupported-at-rule",
    "unsupported-container-query",
    "unsupported-css-module-reference",
    "unsupported-declaration",
    "unsupported-html-base",
    "unsupported-html-stylesheet-link",
    "unsupported-important",
    "unsupported-link-media",
    "unsupported-media-query",
    "unsupported-nested-at-rule",
    "unsupported-overlap",
    "unsupported-rule-content",
    "unsupported-selector",
    "unsupported-sfc-block",
    "unsupported-starting-style",
    "unsupported-supports-query",
    "unsupported-value",
    "unsupported-vue-version",
];

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use super::{SourceFile, apply_edits, plan_batch_json, plan_json};
    use tw_migrate_css::{
        KeyframePlan, SelectorKey, animation_candidate, append_keyframes, css_properties_conflict,
        declaration_to_candidate, tailwind_utilities_conflict,
    };
    use tw_migrate_source::plan_batch_source_file;

    fn plan(request: serde_json::Value) -> serde_json::Value {
        serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap()
    }

    fn plan_batch(request: serde_json::Value) -> serde_json::Value {
        serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap()
    }

    mod batch;
    mod canonicalize;
    mod css_modules;
    mod expressions;
    mod media;
    mod preprocessors;
    mod relationships;
    mod utilities;
    mod vue;

    #[test]
    fn warning_codes_are_pinned_to_the_readme() {
        let readme = include_str!("../../../README.md");
        let documented = readme
            .lines()
            .filter_map(|line| line.strip_prefix("| `")?.split('`').next())
            .collect::<Vec<_>>();
        assert_eq!(
            documented,
            super::WARNING_CODES,
            "the README warning table must list exactly the emitted codes, sorted"
        );

        // Strip the canonical list itself so it cannot satisfy its own check.
        let planner = include_str!("lib.rs");
        let const_start = planner.find("const WARNING_CODES").unwrap();
        let const_end = const_start + planner[const_start..].find("];").unwrap();
        // Scan every crate source and every repo src/ TypeScript file,
        // recursively so a new module or subdirectory cannot silently escape
        // the pinning check.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sources = format!("{}\n{}", &planner[..const_start], &planner[const_end..]);
        for (dir, extension) in [
            (manifest.join("src"), "rs"),
            (manifest.join("../tw_migrate/src"), "rs"),
            (manifest.join("../tw_migrate_css/src"), "rs"),
            (manifest.join("../tw_migrate_source/src"), "rs"),
            (manifest.join("../../src"), "ts"),
        ] {
            let mut pending = vec![dir];
            while let Some(directory) = pending.pop() {
                for entry in std::fs::read_dir(directory).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        pending.push(path);
                    } else if path.extension().is_some_and(|ext| ext == extension)
                        && path != manifest.join("src/lib.rs")
                    {
                        sources.push('\n');
                        sources.push_str(&std::fs::read_to_string(path).unwrap());
                    }
                }
            }
        }
        for code in super::WARNING_CODES {
            assert!(
                sources.contains(&format!("\"{code}\"")) || sources.contains(&format!("'{code}'")),
                "documented warning code `{code}` no longer appears in the sources"
            );
        }

        // Every directly constructed warning code must be documented, whether
        // stamped as a `code:` field, passed positionally to `htmlWarning`, or
        // supplied as the first argument to Rust's Warning constructor.
        // Reason strings flowing through `rule.warning` are covered by the
        // check above plus the comment on WARNING_CODES. The patterns are
        // built at runtime so this test's own source cannot match them.
        let field_sites = format!("{}: ", "code");
        let helper_sites = format!("{}(", "htmlWarning");
        let rust_warning_sites = format!("{}::{}(", "Warning", "new");
        for pattern in [
            field_sites.as_str(),
            helper_sites.as_str(),
            rust_warning_sites.as_str(),
        ] {
            for site in sources.split(pattern).skip(1) {
                let site = site.trim_start();
                let Some(quote) = site.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
                    continue;
                };
                let code = site[1..].split(quote).next().unwrap();
                assert!(
                    super::WARNING_CODES.contains(&code),
                    "emitted warning code `{code}` is missing from WARNING_CODES and the README"
                );
            }
        }
    }
}

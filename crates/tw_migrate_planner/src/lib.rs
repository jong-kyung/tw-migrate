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
}

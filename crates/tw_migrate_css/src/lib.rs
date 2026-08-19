mod animations;
mod arbitrary;
mod at_rules;
mod css_directives;
mod fonts;
mod media;
mod plan;
mod stylesheet_analysis;
mod syntax;
mod theme;
mod utilities;

pub use animations::{KeyframePlan, animation_candidate, append_keyframes};
pub use at_rules::{GlobalAtRulePlan, append_global_at_rules, is_conditional, parse_css};
pub use css_directives::collect_css_directives_json;
pub use fonts::{FontFamilyProbe, font_family_stack_json};
pub use media::{
    MediaComponent, ParsedMediaCondition, collect_media_conditions_json, media_probe_key_json,
    parse_media_condition,
};
pub use plan::{
    ParseOptions, ParsedCss, Relation, RulePlan, SelectorKey, ShadowIndex, index_shadow_selectors,
    parse_css_rules,
};
pub use stylesheet_analysis::{
    collect_custom_property_mentions, compiled_shape_json, stylesheet_analysis_json,
};
pub use syntax::StylesheetSyntax;
pub use theme::parse_dimension;
pub use utilities::{
    css_properties_conflict, css_property_sets_conflict, declaration_to_candidate, tailwind_utilities_conflict,
    tailwind_utility_parts, tailwind_variants_match, utility_conflict, variant_segments,
};

pub fn validate_css(source: &str) -> tw_migrate_error::MigrationResult<()> {
    let allocator = oxc_css_parser::Allocator::default();
    parse_css(&allocator, source, oxc_css_parser::Syntax::Css)
        .map(|_| ())
        .map_err(
            |error| tw_migrate_error::MigrationError::EditedStylesheetParse {
                message: format!("Edited stylesheet no longer parses: {error}"),
            },
        )
}

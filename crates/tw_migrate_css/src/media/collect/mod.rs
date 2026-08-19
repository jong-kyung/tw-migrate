use std::collections::{HashMap, HashSet};

use oxc_css_parser::ast::{AtRule, ComplexSelectorChild, QualifiedRule, SimpleSelector, Statement};
use serde::{Deserialize, Serialize};
use tw_migrate_error::{MigrationError, MigrationResult};

use crate::{
    at_rules::{at_rule_query, builtin_media_variant, parse_css},
    syntax::StylesheetSyntax,
};

use super::{
    ParsedMediaCondition, WidthBound, collapse_whitespace, condition_digest, parse_media_condition,
};

/// One stylesheet supplied to the collection pass. Preprocessor stylesheets
/// pass their compiled CSS as `analysis_source`, matching the planner.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectionStylesheet {
    css_path: String,
    css_source: String,
    #[serde(default)]
    analysis_source: Option<String>,
    #[serde(default)]
    syntax: StylesheetSyntax,
    /// Vue SFC entries: `css_source` is the whole `.vue` file and each block
    /// names its style contents, mirroring the planner's Vue entries.
    #[serde(default)]
    vue_blocks: Vec<CollectionVueBlock>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectionVueBlock {
    content_start: usize,
    content_end: usize,
    #[serde(default)]
    syntax: StylesheetSyntax,
    #[serde(default)]
    analysis_source: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TailwindSource {
    path: String,
    source: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaCollectionRequest {
    stylesheets: Vec<CollectionStylesheet>,
    #[serde(default)]
    theme_tokens: HashMap<String, String>,
    /// Project-owned stylesheet sources retained from the Tailwind entry
    /// import graph, parsed for authored `@custom-variant` reservations.
    #[serde(default)]
    tailwind_sources: Vec<TailwindSource>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaCollectionResponse {
    components: Vec<CollectedComponent>,
    authored_variants: Vec<AuthoredVariant>,
}

/// One deduplicated generated-variant unit: a component of a decomposed
/// condition, or one whole non-decomposable condition.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectedComponent {
    key: String,
    /// True for a whole non-decomposable condition.
    whole: bool,
    readable_name: Option<String>,
    digest: String,
    /// A built-in variant candidate; reuse still requires the TypeScript
    /// layer to verify the loaded design system's effective expansion.
    builtin: Option<String>,
    /// An existing project breakpoint variant such as `md` or `max-lg`,
    /// matched by exact semantic value.
    breakpoint: Option<String>,
    css_path: String,
    /// Scan position of the first occurrence of this key, in request
    /// stylesheet order. A deterministic tiebreaker only: cascade ordering
    /// authority belongs to the rewrite pass, which recomputes order from
    /// the rules that actually survive into the migration plan, because a
    /// retained early occurrence must not fix registration order for the
    /// migrated uses.
    order: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthoredVariant {
    name: String,
    definition: String,
    /// The normalized condition key when the definition is exactly one
    /// `@media` wrapper around `@slot`; `None` for opaque definitions.
    media_query_key: Option<String>,
    path: String,
}

/// Collect every generated-variant unit from the request stylesheets.
/// Conditions the current planner already converts through a built-in
/// variant or an exact existing breakpoint are skipped whole; every other
/// representable condition is decomposed, and each deduplicated component
/// reports its built-in candidate, existing-breakpoint match, readable
/// name, and digest. Also returns authored custom-variant reservations
/// parsed from the supplied Tailwind sources.
pub fn collect_media_conditions_json(request: &str) -> MigrationResult<String> {
    let request: MediaCollectionRequest =
        serde_json::from_str(request).map_err(|error| MigrationError::InvalidRequest {
            message: format!("Invalid request: {error}"),
        })?;

    let mut components: Vec<CollectedComponent> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut order = 0usize;
    for stylesheet in &request.stylesheets {
        // A Vue SFC entry carries the whole `.vue` file as `css_source` and
        // names its style contents through blocks; other entries parse one
        // source, preferring compiled preprocessor output.
        let mut units: Vec<(&str, StylesheetSyntax)> = Vec::new();
        if stylesheet.vue_blocks.is_empty() {
            match &stylesheet.analysis_source {
                Some(analysis) => units.push((analysis.as_str(), StylesheetSyntax::default())),
                None => units.push((stylesheet.css_source.as_str(), stylesheet.syntax)),
            }
        } else {
            for block in &stylesheet.vue_blocks {
                match &block.analysis_source {
                    Some(analysis) => units.push((analysis.as_str(), StylesheetSyntax::default())),
                    None => {
                        let source = &stylesheet.css_source;
                        if block.content_start > block.content_end
                            || block.content_end > source.len()
                            || !source.is_char_boundary(block.content_start)
                            || !source.is_char_boundary(block.content_end)
                        {
                            return Err(MigrationError::Invariant {
                                message: format!(
                                    "Invalid Vue style block span in {}",
                                    stylesheet.css_path
                                ),
                            });
                        }
                        units.push((
                            &source[block.content_start..block.content_end],
                            block.syntax,
                        ));
                    }
                }
            }
        }
        for (source, syntax) in units {
            let allocator = oxc_css_parser::Allocator::default();
            let parsed =
                parse_css(&allocator, source, syntax.parser_syntax()).map_err(|error| {
                    MigrationError::AuthoredStylesheetParse {
                        message: format!("Failed to parse {}: {error}", stylesheet.css_path),
                    }
                })?;
            let mut at_rules = Vec::new();
            walk_media_at_rules(&parsed.statements, &mut |at_rule| at_rules.push(at_rule));
            for at_rule in at_rules {
                if at_rule.block.is_none() {
                    continue;
                }
                // Nothing is converted whole: built-in and breakpoint names
                // may both be shadowed, so every match must reach the
                // resolver's effective-expansion verification through the
                // decomposed components.
                let Some(query) = at_rule_query(at_rule, source, "media") else {
                    continue;
                };
                let Some(condition) = parse_media_condition(query) else {
                    continue;
                };
                let (units, whole) = match condition {
                    ParsedMediaCondition::Components(list) => (list, false),
                    ParsedMediaCondition::Whole(component) => (vec![component], true),
                };
                for component in units {
                    order += 1;
                    if seen.contains(&component.key) {
                        continue;
                    }
                    seen.insert(component.key.clone());
                    let builtin = component
                        .builtin_query
                        .as_deref()
                        .and_then(builtin_media_variant)
                        .map(str::to_string);
                    let breakpoint = component.width_bound.as_ref().and_then(|bound| {
                        breakpoint_variant_for_bound(bound, &request.theme_tokens)
                    });
                    components.push(CollectedComponent {
                        digest: condition_digest(&component.key),
                        key: component.key,
                        whole,
                        readable_name: component.readable_name,
                        builtin,
                        breakpoint,
                        css_path: stylesheet.css_path.clone(),
                        order,
                    });
                }
            }
        }
    }
    components.sort_by(|left, right| left.key.cmp(&right.key));

    let mut authored_variants = Vec::new();
    for tailwind_source in &request.tailwind_sources {
        collect_authored_variants(
            &tailwind_source.path,
            &tailwind_source.source,
            &mut authored_variants,
        )?;
    }
    authored_variants.sort_by(|left: &AuthoredVariant, right: &AuthoredVariant| {
        left.name.cmp(&right.name).then(left.path.cmp(&right.path))
    });

    serde_json::to_string(&MediaCollectionResponse {
        components,
        authored_variants,
    })
    .map_err(|error| MigrationError::Serialization {
        message: error.to_string(),
    })
}

/// The existing breakpoint variant for a width bound: an inclusive lower
/// bound whose value semantically equals a breakpoint uses the breakpoint
/// name, and an exclusive upper bound uses `max-<name>`. Values compare as
/// exact canonical dimensions, never through f64 rounding, and no unit
/// conversion is attempted.
fn breakpoint_variant_for_bound(
    bound: &WidthBound,
    theme_tokens: &HashMap<String, String>,
) -> Option<String> {
    if bound.lower != bound.inclusive {
        // Exclusive lower and inclusive upper bounds have no exact
        // breakpoint variant form.
        return None;
    }
    // Values compare as case-insensitively identical text with no numeric
    // parsing: a token spelled `48REM` proves `48rem`, while `48.0rem` does
    // not and simply yields a generated variant with identical meaning.
    let name = theme_tokens
        .iter()
        .filter_map(|(name, token_value)| {
            let name = name.strip_prefix("breakpoint-")?;
            token_value
                .trim()
                .eq_ignore_ascii_case(&bound.value)
                .then_some(name)
        })
        .min()?;
    Some(if bound.lower {
        name.to_string()
    } else {
        format!("max-{name}")
    })
}

/// Visit every `@media` at-rule in document order, at any nesting depth.
fn walk_media_at_rules<'a, 'b>(
    statements: &'a [Statement<'b>],
    visit: &mut impl FnMut(&'a AtRule<'b>),
) {
    for statement in statements {
        match statement {
            Statement::AtRule(at_rule) => {
                if at_rule.name.name == "media" {
                    visit(at_rule);
                }
                if let Some(block) = &at_rule.block {
                    walk_media_at_rules(&block.statements, visit);
                }
            }
            Statement::QualifiedRule(rule) => {
                walk_media_at_rules(&rule.block.statements, visit);
            }
            _ => {}
        }
    }
}

fn collect_authored_variants(
    path: &str,
    source: &str,
    variants: &mut Vec<AuthoredVariant>,
) -> MigrationResult<()> {
    let allocator = oxc_css_parser::Allocator::default();
    let parsed = parse_css(&allocator, source, oxc_css_parser::Syntax::Css).map_err(|error| {
        MigrationError::AuthoredStylesheetParse {
            message: format!("Failed to parse {path}: {error}"),
        }
    })?;
    let mut at_rules = Vec::new();
    walk_custom_variant_at_rules(&parsed.statements, &mut |at_rule| at_rules.push(at_rule));
    for at_rule in at_rules {
        let prelude_end = at_rule
            .block
            .as_ref()
            .map_or(at_rule.span.end, |block| block.span.start);
        let prelude = source[at_rule.span.start..prelude_end]
            .trim()
            .trim_start_matches('@')
            .trim_start_matches("custom-variant")
            .trim();
        let Some(name) = prelude.split_whitespace().next() else {
            continue;
        };
        variants.push(AuthoredVariant {
            name: name.trim_end_matches(';').to_string(),
            definition: collapse_whitespace(&source[at_rule.span.start..at_rule.span.end]),
            media_query_key: authored_media_wrapper_key(at_rule, source),
            path: path.to_string(),
        });
    }
    Ok(())
}

fn walk_custom_variant_at_rules<'a, 'b>(
    statements: &'a [Statement<'b>],
    visit: &mut impl FnMut(&'a AtRule<'b>),
) {
    for statement in statements {
        let Statement::AtRule(at_rule) = statement else {
            continue;
        };
        if at_rule.name.name == "custom-variant" {
            visit(at_rule);
        } else if let Some(block) = &at_rule.block {
            walk_custom_variant_at_rules(&block.statements, visit);
        }
    }
}

/// The normalized component or whole key of an authored definition shaped
/// exactly as one `@media` block whose only statement is `@slot`, when the
/// wrapped condition is a single generated-variant unit. This is the shape
/// content-identity adoption compares against.
fn authored_media_wrapper_key(at_rule: &AtRule<'_>, source: &str) -> Option<String> {
    let block = at_rule.block.as_ref()?;
    let [Statement::AtRule(media)] = block.statements.as_slice() else {
        return None;
    };
    if media.name.name != "media" {
        return None;
    }
    let media_block = media.block.as_ref()?;
    let [Statement::AtRule(slot)] = media_block.statements.as_slice() else {
        return None;
    };
    if slot.name.name != "slot" || slot.block.is_some() {
        return None;
    }
    single_unit_key(at_rule_query(media, source, "media")?)
}

/// The normalized key of a query that is exactly one generated-variant unit:
/// a whole-condition component, or a component list with a single entry.
fn single_unit_key(query: &str) -> Option<String> {
    match parse_media_condition(query)? {
        ParsedMediaCondition::Whole(component) => Some(component.key),
        ParsedMediaCondition::Components(components) => {
            let [component] = components.as_slice() else {
                return None;
            };
            Some(component.key.clone())
        }
    }
}

/// The normalized generated-variant key of a compiled probe's output: the
/// CSS must be exactly one top-level `@media` block, and its query must
/// normalize to a single generated-variant unit. This is the shape the
/// effective-expansion proof compares against a component or whole key;
/// selector-based expansions and anything else return `None`.
pub fn media_probe_key_json(css: &str) -> MigrationResult<String> {
    let allocator = oxc_css_parser::Allocator::default();
    let parsed = parse_css(&allocator, css, oxc_css_parser::Syntax::Css).map_err(|error| {
        MigrationError::AuthoredStylesheetParse {
            message: format!("Failed to parse probe CSS: {error}"),
        }
    })?;
    let key = (|| {
        let mut media_query: Option<&str> = None;
        for statement in &parsed.statements {
            let Statement::AtRule(at_rule) = statement else {
                // A top-level style rule means the expansion is not a pure
                // media wrapper.
                return None;
            };
            let block = at_rule.block.as_ref()?;
            // The wrapper must contain exactly the probe rule with its
            // selector untouched: a nested at-rule means more than one media
            // level, and any qualified or combined selector means the variant
            // re-scopes the utility beyond the media condition, so reusing it
            // for a plain condition would change when the style applies.
            let pure_wrapper = match block.statements.as_slice() {
                [Statement::QualifiedRule(rule)] => is_bare_class_rule(rule),
                _ => false,
            };
            if at_rule.name.name != "media" || media_query.is_some() || !pure_wrapper {
                return None;
            }
            media_query = at_rule_query(at_rule, css, "media");
        }
        single_unit_key(media_query?)
    })();
    serde_json::to_string(&key).map_err(|error| MigrationError::Serialization {
        message: error.to_string(),
    })
}

/// True when the rule's selector is exactly one bare class: the compiled
/// probe candidate itself, with no combinators, qualifying selectors, or
/// pseudo-classes added by the variant.
fn is_bare_class_rule(rule: &QualifiedRule<'_>) -> bool {
    let [selector] = rule.selector.selectors.as_slice() else {
        return false;
    };
    let [ComplexSelectorChild::CompoundSelector(compound)] = selector.children.as_slice() else {
        return false;
    };
    matches!(compound.children.as_slice(), [SimpleSelector::Class(_)])
}

#[cfg(test)]
mod collection_tests;

#[cfg(test)]
mod probe_tests;

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use oxc_css_parser::{
    Parser as CssParser, Syntax,
    ast::{AtRule, Statement, Stylesheet},
};

use crate::theme::{exact_theme_token, parse_dimension};

pub(crate) struct GlobalAtRulePlan {
    pub(crate) span: Range<usize>,
    pub(crate) source: String,
}

pub(crate) fn global_at_rule_plan(at_rule: &AtRule<'_>, source: &str) -> Option<GlobalAtRulePlan> {
    if !matches!(
        at_rule.name.name,
        "color-profile"
            | "counter-style"
            | "font-face"
            | "font-feature-values"
            | "font-palette-values"
            | "position-try"
            | "property"
            | "view-transition"
    ) {
        return None;
    }
    let raw = &source[at_rule.span.start..at_rule.span.end];
    if raw.to_ascii_lowercase().contains("url(") {
        return None;
    }
    Some(GlobalAtRulePlan {
        span: at_rule.span.start..at_rule.span.end,
        source: raw.to_string(),
    })
}

pub(crate) fn append_global_at_rules(
    source: &str,
    at_rules: &[&GlobalAtRulePlan],
) -> Result<String, String> {
    let allocator = oxc_css_parser::Allocator::default();
    let mut parser = CssParser::new(&allocator, source, Syntax::Css);
    let stylesheet = parser
        .parse::<Stylesheet>()
        .map_err(|error| format!("Failed to parse Tailwind CSS: {error:?}"))?;
    let mut existing = HashSet::new();
    collect_at_rule_sources(&stylesheet.statements, source, &mut existing);

    let mut output = source.to_string();
    for at_rule in at_rules {
        if existing.contains(at_rule.source.trim()) {
            continue;
        }
        if !output.ends_with('\n') {
            output.push('\n');
        }
        if !output.ends_with("\n\n") {
            output.push('\n');
        }
        output.push_str(at_rule.source.trim());
        output.push('\n');
    }
    Ok(output)
}

fn collect_at_rule_sources<'a>(
    statements: &[Statement<'_>],
    source: &'a str,
    at_rules: &mut HashSet<&'a str>,
) {
    for statement in statements {
        let Statement::AtRule(at_rule) = statement else {
            continue;
        };
        at_rules.insert(source[at_rule.span.start..at_rule.span.end].trim());
        if let Some(block) = &at_rule.block {
            collect_at_rule_sources(&block.statements, source, at_rules);
        }
    }
}

pub(crate) fn is_conditional(name: &str) -> bool {
    matches!(name, "media" | "supports" | "container" | "starting-style")
}

pub(crate) fn conditional_variant(
    at_rule: &AtRule<'_>,
    source: &str,
    theme_tokens: &HashMap<String, String>,
) -> Option<String> {
    match at_rule.name.name {
        "media" => media_variant(at_rule, source, theme_tokens),
        "supports" => supports_variant(at_rule, source),
        "container" => container_variant(at_rule, source, theme_tokens),
        "starting-style" => Some("starting".to_string()),
        _ => None,
    }
}

pub(crate) fn unsupported_warning(name: &str) -> &'static str {
    match name {
        "media" => "unsupported-media-query",
        "supports" => "unsupported-supports-query",
        "container" => "unsupported-container-query",
        "starting-style" => "unsupported-starting-style",
        _ => "unsupported-at-rule",
    }
}

fn media_variant(
    at_rule: &AtRule<'_>,
    source: &str,
    theme_tokens: &HashMap<String, String>,
) -> Option<String> {
    media_breakpoint_variant(at_rule, source, theme_tokens)
        .or_else(|| media_feature_variant(at_rule, source))
}

fn media_feature_variant(at_rule: &AtRule<'_>, source: &str) -> Option<String> {
    let query = at_rule_query(at_rule, source, "media")?;
    let normalized = query
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    let variant = match normalized.as_str() {
        "print" => "print",
        "(prefers-color-scheme:dark)" => "dark",
        "(prefers-reduced-motion:reduce)" => "motion-reduce",
        "(prefers-reduced-motion:no-preference)" => "motion-safe",
        "(prefers-contrast:more)" => "contrast-more",
        "(prefers-contrast:less)" => "contrast-less",
        "(forced-colors:active)" => "forced-colors",
        "(forced-colors:none)" => "not-forced-colors",
        "(inverted-colors:inverted)" => "inverted-colors",
        "(orientation:portrait)" => "portrait",
        "(orientation:landscape)" => "landscape",
        "(pointer:fine)" => "pointer-fine",
        "(pointer:coarse)" => "pointer-coarse",
        "(pointer:none)" => "pointer-none",
        "(any-pointer:fine)" => "any-pointer-fine",
        "(any-pointer:coarse)" => "any-pointer-coarse",
        "(any-pointer:none)" => "any-pointer-none",
        "(scripting:none)" => "noscript",
        _ => return None,
    };
    Some(variant.to_string())
}

fn media_breakpoint_variant(
    at_rule: &AtRule<'_>,
    source: &str,
    theme_tokens: &HashMap<String, String>,
) -> Option<String> {
    let query = &source[at_rule.span.start..at_rule.block.as_ref()?.span.start];
    let (min_width, max_width) = width_media_query(query)?;
    let min_variant = exact_theme_token("breakpoint", min_width, theme_tokens)?;
    let Some(max_width) = max_width else {
        return Some(min_variant);
    };
    let (min_number, min_unit) = parse_dimension(min_width)?;
    let (max_number, max_unit) = parse_dimension(max_width)?;
    if min_unit != max_unit || min_number > max_number {
        return None;
    }
    let max_variant = breakpoint_after_legacy_max(max_number, max_unit, theme_tokens)?;
    Some(format!("{min_variant}:max-{max_variant}"))
}

fn container_variant(
    at_rule: &AtRule<'_>,
    source: &str,
    theme_tokens: &HashMap<String, String>,
) -> Option<String> {
    let query = at_rule_query(at_rule, source, "container")?;
    let (min_width, max_width) = width_query(query)?;
    if max_width.is_some() {
        return None;
    }
    Some(
        match exact_theme_token("container", min_width, theme_tokens) {
            Some(name) => format!("@{name}"),
            None => format!("@min-[{min_width}]"),
        },
    )
}

fn supports_variant(at_rule: &AtRule<'_>, source: &str) -> Option<String> {
    let query = at_rule_query(at_rule, source, "supports")?;
    if query.is_empty()
        || query.contains(['[', ']', ';', '{', '}', '"', '\'', '\\'])
        || query.contains("/*")
    {
        return None;
    }
    let condition = strip_outer_parentheses(query).unwrap_or(query);
    let condition = condition
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
        .replace(":_", ":")
        .replace("_:", ":")
        .replace("(_", "(")
        .replace("_)", ")");
    (!condition.is_empty()).then(|| format!("supports-[{condition}]"))
}

fn at_rule_query<'a>(at_rule: &AtRule<'_>, source: &'a str, name: &str) -> Option<&'a str> {
    source[at_rule.span.start..at_rule.block.as_ref()?.span.start]
        .trim()
        .strip_prefix(&format!("@{name}"))
        .map(str::trim)
}

fn width_media_query(query: &str) -> Option<(&str, Option<&str>)> {
    width_query(query.trim().strip_prefix("@media")?.trim())
}

fn width_query(query: &str) -> Option<(&str, Option<&str>)> {
    let ((first_name, first_value), rest) = width_condition(query)?;
    let rest = rest.trim();
    if rest.is_empty() {
        return (first_name == "min-width").then_some((first_value, None));
    }

    let rest = rest.strip_prefix("and")?.trim();
    let ((second_name, second_value), rest) = width_condition(rest)?;
    if !rest.trim().is_empty() {
        return None;
    }
    match (first_name, second_name) {
        ("min-width", "max-width") => Some((first_value, Some(second_value))),
        ("max-width", "min-width") => Some((second_value, Some(first_value))),
        _ => None,
    }
}

fn width_condition(input: &str) -> Option<((&str, &str), &str)> {
    let input = input.strip_prefix('(')?;
    let end = input.find(')')?;
    let (name, value) = input[..end].split_once(':')?;
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(((name.trim(), value), &input[end + 1..]))
}

fn breakpoint_after_legacy_max(
    value_number: f64,
    value_unit: &str,
    theme_tokens: &HashMap<String, String>,
) -> Option<String> {
    theme_tokens
        .iter()
        .filter_map(|(name, token_value)| {
            let name = name.strip_prefix("breakpoint-")?;
            let (breakpoint_number, breakpoint_unit) = parse_dimension(token_value.trim())?;
            (breakpoint_unit == value_unit
                && (breakpoint_number - value_number - 0.001).abs() < 1e-9)
                .then_some(name)
        })
        .min()
        .map(str::to_string)
}

fn strip_outer_parentheses(value: &str) -> Option<&str> {
    if !value.starts_with('(') || !value.ends_with(')') {
        return None;
    }
    let mut depth = 0usize;
    for (index, character) in value.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 && index != value.len() - 1 {
                    return None;
                }
            }
            _ => {}
        }
    }
    (depth == 0).then(|| value[1..value.len() - 1].trim())
}

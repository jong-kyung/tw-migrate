use std::{collections::HashMap, ops::Range};

use crate::arbitrary::encode as arbitrary_value;

use oxc_css_parser::{
    Parser as CssParser, Syntax,
    ast::{AtRule, AtRulePrelude, InterpolableIdent, KeyframesName, Statement, Stylesheet},
};

pub(crate) struct KeyframePlan {
    pub(crate) span: Range<usize>,
    pub(crate) name: String,
    pub(crate) migrated_name: String,
    pub(crate) source: String,
}

pub(crate) fn keyframe_plan(
    at_rule: &AtRule<'_>,
    path: &str,
    source: &str,
) -> Option<KeyframePlan> {
    if at_rule.name.name != "keyframes" || at_rule.block.is_none() {
        return None;
    }
    let Some(AtRulePrelude::Keyframes(KeyframesName::Ident(InterpolableIdent::Literal(name)))) =
        &at_rule.prelude
    else {
        return None;
    };
    let raw = &source[at_rule.span.start..at_rule.span.end];
    if raw.to_ascii_lowercase().contains("url(") {
        return None;
    }

    let migrated_name = format!("tw-migrate-{:x}-{}", stable_hash(path), name.name);
    let mut migrated_source = raw.to_string();
    migrated_source.replace_range(
        name.span.start - at_rule.span.start..name.span.end - at_rule.span.start,
        &migrated_name,
    );
    Some(KeyframePlan {
        span: at_rule.span.start..at_rule.span.end,
        name: name.name.to_string(),
        migrated_name,
        source: migrated_source,
    })
}

pub(crate) fn animation_candidate(
    property: &str,
    value: &str,
    keyframes: &HashMap<&str, &str>,
) -> Option<String> {
    if value.contains(',') {
        return None;
    }
    let parts = value.split_whitespace().collect::<Vec<_>>();
    if property == "animation-name" && parts.len() != 1 {
        return None;
    }
    let names = parts
        .iter()
        .copied()
        .filter(|part| keyframes.contains_key(part))
        .collect::<Vec<_>>();
    let [name] = names.as_slice() else {
        return None;
    };
    if property == "animation" && is_animation_keyword(name) {
        return None;
    }
    let migrated_value = parts
        .into_iter()
        .map(|part| keyframes.get(part).copied().unwrap_or(part))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("[{property}:{}]", arbitrary_value(&migrated_value)))
}

pub(crate) fn append_keyframes(
    source: &str,
    keyframes: &[&KeyframePlan],
) -> Result<String, String> {
    let allocator = oxc_css_parser::Allocator::default();
    let mut parser = CssParser::new(&allocator, source, Syntax::Css);
    let stylesheet = parser
        .parse::<Stylesheet>()
        .map_err(|error| format!("Failed to parse Tailwind CSS: {error:?}"))?;
    let mut existing = HashMap::new();
    collect_keyframes(&stylesheet.statements, source, &mut existing);

    let mut output = source.to_string();
    for keyframe in keyframes {
        if let Some(current) = existing.get(&keyframe.migrated_name) {
            if current.trim() != keyframe.source.trim() {
                return Err(format!(
                    "Tailwind CSS already defines a different @keyframes {}",
                    keyframe.migrated_name
                ));
            }
            continue;
        }
        if !output.ends_with('\n') {
            output.push('\n');
        }
        if !output.ends_with("\n\n") {
            output.push('\n');
        }
        output.push_str(keyframe.source.trim());
        output.push('\n');
    }
    Ok(output)
}

fn collect_keyframes(
    statements: &[Statement<'_>],
    source: &str,
    keyframes: &mut HashMap<String, String>,
) {
    for statement in statements {
        let Statement::AtRule(at_rule) = statement else {
            continue;
        };
        if at_rule.name.name == "keyframes"
            && let Some(AtRulePrelude::Keyframes(KeyframesName::Ident(InterpolableIdent::Literal(
                name,
            )))) = &at_rule.prelude
        {
            keyframes.insert(
                name.name.to_string(),
                source[at_rule.span.start..at_rule.span.end].to_string(),
            );
        }
        if let Some(block) = &at_rule.block {
            collect_keyframes(&block.statements, source, keyframes);
        }
    }
}

fn stable_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn is_animation_keyword(value: &str) -> bool {
    matches!(
        value,
        "none"
            | "linear"
            | "ease"
            | "ease-in"
            | "ease-out"
            | "ease-in-out"
            | "step-start"
            | "step-end"
            | "infinite"
            | "normal"
            | "reverse"
            | "alternate"
            | "alternate-reverse"
            | "forwards"
            | "backwards"
            | "both"
            | "running"
            | "paused"
            | "initial"
            | "inherit"
            | "unset"
            | "revert"
            | "revert-layer"
    )
}

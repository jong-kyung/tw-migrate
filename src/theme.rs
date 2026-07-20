use std::collections::HashMap;

pub(crate) fn exact_theme_token(
    namespace: &str,
    value: &str,
    theme_tokens: &HashMap<String, String>,
) -> Option<String> {
    let token_prefix = format!("{namespace}-");
    let mut named = theme_tokens
        .iter()
        .filter(|(name, token_value)| {
            name.starts_with(&token_prefix) && token_value.trim() == value
        })
        .map(|(name, _)| name[token_prefix.len()..].to_string())
        .collect::<Vec<_>>();
    named.sort();
    if let Some(name) = named.into_iter().next() {
        return Some(name);
    }

    if namespace == "spacing"
        && let Some(base) = theme_tokens.get("spacing")
        && let (Some((value_number, value_unit)), Some((base_number, base_unit))) =
            (parse_dimension(value), parse_dimension(base))
        && value_unit == base_unit
        && base_number != 0.0
    {
        let multiplier = value_number / base_number;
        if multiplier.is_finite() && multiplier >= 0.0 {
            return Some(format_number(multiplier));
        }
    }
    None
}

pub(crate) fn parse_dimension(value: &str) -> Option<(f64, &str)> {
    let split = value
        .char_indices()
        .find(|(_, character)| !character.is_ascii_digit() && !matches!(character, '.' | '-'))
        .map(|(index, _)| index)?;
    let (number, unit) = value.split_at(split);
    Some((number.parse().ok()?, unit))
}

fn format_number(number: f64) -> String {
    if number.fract() == 0.0 {
        format!("{number:.0}")
    } else {
        number.to_string()
    }
}

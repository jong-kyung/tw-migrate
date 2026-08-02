use std::collections::HashMap;

pub(crate) fn exact_theme_token(
    namespace: &str,
    value: &str,
    theme_tokens: &HashMap<String, String>,
) -> Option<String> {
    let token_prefix = format!("{namespace}-");
    if let Some(name) = theme_tokens
        .iter()
        .filter(|(name, token_value)| {
            name.starts_with(&token_prefix) && token_value.trim() == value
        })
        .map(|(name, _)| &name[token_prefix.len()..])
        .min()
    {
        return Some(name.to_string());
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
            // f64 Display already renders integral values without a trailing
            // ".0" ("2", not "2.0").
            return Some(multiplier.to_string());
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


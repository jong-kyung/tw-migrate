//! Media query condition keys, classification, and generated names.
//!
//! This module normalizes `@media` conditions into deterministic condition
//! keys, classifies simple minimum-width queries, and derives generated
//! breakpoint and custom-variant names. It performs no unit conversion and
//! never merges conditions whose equivalence it cannot prove: `48rem` and
//! `768px` stay distinct keys even when a typical browser renders them at the
//! same width.

// The native collection pass added in the next change consumes this module;
// until then only the in-module tests exercise it.
#![allow(dead_code)]

/// The longest generated name emitted without a digest suffix.
const MAX_NAME_LENGTH: usize = 48;

/// A parsed, normalized media condition.
pub(crate) struct MediaCondition {
    /// Canonical normalized condition used for deduplication and naming.
    pub(crate) key: String,
    /// The lower-bound value when the whole condition is one simple
    /// minimum-width query: `(min-width: 52rem)` or `(width >= 52rem)` with
    /// no modifier, media type, upper bound, list, or additional feature.
    pub(crate) simple_min_width: Option<SimpleMinWidth>,
    /// Preferred readable `@custom-variant` name derived from the condition.
    pub(crate) preferred_custom_name: String,
}

pub(crate) struct SimpleMinWidth {
    pub(crate) value: String,
    pub(crate) number: f64,
    pub(crate) unit: String,
}

/// Parse and normalize a media query condition (the text after `@media`).
/// Returns `None` when the condition cannot be represented safely: unknown
/// syntax, nested grouping, custom-media references, or characters that a
/// generated definition cannot carry.
pub(crate) fn parse_media_condition(query: &str) -> Option<MediaCondition> {
    // Only ASCII whitespace is insignificant in CSS; other whitespace code
    // points are identifier content and must survive untouched.
    let query = trim_css_whitespace(query);
    if query.is_empty()
        || query.contains(['{', '}', ';', '"', '\'', '\\', '[', ']'])
        || query.contains("/*")
    {
        return None;
    }
    let branches = split_top_level(query, ',')?;
    let mut parsed = Vec::with_capacity(branches.len());
    for branch in &branches {
        parsed.push(parse_branch(branch)?);
    }

    let key = parsed
        .iter()
        .map(Branch::render)
        .collect::<Vec<_>>()
        .join(", ");
    let preferred_custom_name = valid_variant_name(
        &parsed
            .iter()
            .map(Branch::name_tokens)
            .collect::<Vec<_>>()
            .join("-or-"),
    );
    let simple_min_width = match parsed.as_slice() {
        [branch] => branch.simple_min_width(),
        _ => None,
    };
    Some(MediaCondition {
        key,
        simple_min_width,
        preferred_custom_name,
    })
}

/// The generated theme-variable stem for a simple minimum-width value:
/// `52rem` becomes `min-52rem`, `47.5rem` becomes `min-47p5rem`. The full
/// theme variable is `--breakpoint-<stem>` and the variant is `<stem>:`.
pub(crate) fn breakpoint_name(value: &str) -> String {
    format!("min-{}", sanitize_value(value))
}

/// A short stable digest of a condition key, used for collision suffixes.
pub(crate) fn condition_digest(key: &str) -> String {
    // FNV-1a, 64-bit, rendered as the low 32 bits. Stable across platforms
    // and runs; not cryptographic, which collision handling does not need.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", hash as u32)
}

/// Append the digest suffix a colliding or over-long name receives, keeping
/// the readable prefix inside the fixed length limit.
pub(crate) fn digest_suffixed_name(name: &str, key: &str) -> String {
    let digest = condition_digest(key);
    let budget = MAX_NAME_LENGTH - digest.len() - 1;
    let mut prefix = name;
    if prefix.len() > budget {
        let mut end = budget;
        while !prefix.is_char_boundary(end) {
            end -= 1;
        }
        prefix = &prefix[..end];
    }
    format!("{}-{digest}", prefix.trim_end_matches('-'))
}

/// Enforce the length limit on a preferred name.
pub(crate) fn limit_name(name: &str, key: &str) -> String {
    if name.len() <= MAX_NAME_LENGTH {
        return name.to_string();
    }
    digest_suffixed_name(name, key)
}

struct Branch {
    modifier: Option<&'static str>,
    media_type: Option<String>,
    conditions: Vec<Condition>,
}

enum Condition {
    /// `(feature: value)`, after legacy width normalization.
    Plain { feature: String, value: String },
    /// `(feature)`.
    Boolean { feature: String },
    /// `(feature op value)`, always feature-first.
    Range {
        feature: String,
        op: Comparison,
        value: String,
    },
    /// `(low lowOp feature highOp high)`, kept in authored ascending form.
    DoubleRange {
        low: String,
        low_op: Comparison,
        feature: String,
        high_op: Comparison,
        high: String,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Comparison {
    Lt,
    Lte,
    Gt,
    Gte,
    Eq,
}

impl Comparison {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "<" => Self::Lt,
            "<=" => Self::Lte,
            ">" => Self::Gt,
            ">=" => Self::Gte,
            "=" => Self::Eq,
            _ => return None,
        })
    }

    fn symbol(self) -> &'static str {
        match self {
            Self::Lt => "<",
            Self::Lte => "<=",
            Self::Gt => ">",
            Self::Gte => ">=",
            Self::Eq => "=",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Lt => "lt",
            Self::Lte => "lte",
            Self::Gt => "gt",
            Self::Gte => "gte",
            Self::Eq => "eq",
        }
    }

    /// The comparison read from the feature's side: `52rem <= width` means
    /// `width >= 52rem`.
    fn flipped(self) -> Self {
        match self {
            Self::Lt => Self::Gt,
            Self::Lte => Self::Gte,
            Self::Gt => Self::Lt,
            Self::Gte => Self::Lte,
            Self::Eq => Self::Eq,
        }
    }
}

impl Branch {
    fn render(&self) -> String {
        let mut parts = Vec::new();
        if let Some(modifier) = self.modifier {
            parts.push(modifier.to_string());
        }
        if let Some(media_type) = &self.media_type {
            parts.push(media_type.clone());
        }
        for (index, condition) in self.conditions.iter().enumerate() {
            if index > 0 || self.media_type.is_some() {
                parts.push("and".to_string());
            }
            parts.push(condition.render());
        }
        parts.join(" ")
    }

    fn name_tokens(&self) -> String {
        let mut parts = Vec::new();
        if let Some(modifier) = self.modifier {
            parts.push(modifier.to_string());
        }
        if let Some(media_type) = &self.media_type {
            parts.push(media_type.clone());
        }
        for (index, condition) in self.conditions.iter().enumerate() {
            if index > 0 {
                parts.push("and".to_string());
            }
            parts.push(condition.name_tokens());
        }
        parts.join("-")
    }

    fn simple_min_width(&self) -> Option<SimpleMinWidth> {
        if self.modifier.is_some() || self.media_type.is_some() {
            return None;
        }
        let [
            Condition::Range {
                feature,
                op: Comparison::Gte,
                value,
            },
        ] = self.conditions.as_slice()
        else {
            return None;
        };
        if feature != "width" {
            return None;
        }
        let (number, unit) = parse_css_dimension(value)?;
        if !number.is_finite() || number < 0.0 {
            return None;
        }
        Some(SimpleMinWidth {
            value: value.clone(),
            number,
            unit,
        })
    }
}

impl Condition {
    fn render(&self) -> String {
        match self {
            Self::Plain { feature, value } => format!("({feature}: {value})"),
            Self::Boolean { feature } => format!("({feature})"),
            Self::Range { feature, op, value } => {
                format!("({feature} {} {value})", op.symbol())
            }
            Self::DoubleRange {
                low,
                low_op,
                feature,
                high_op,
                high,
            } => format!(
                "({low} {} {feature} {} {high})",
                low_op.symbol(),
                high_op.symbol()
            ),
        }
    }

    fn name_tokens(&self) -> String {
        match self {
            Self::Plain { feature, value } => {
                format!("{feature}-{}", sanitize_value(value))
            }
            Self::Boolean { feature } => feature.clone(),
            Self::Range { feature, op, value } => {
                format!("{feature}-{}-{}", op.name(), sanitize_value(value))
            }
            Self::DoubleRange {
                low,
                low_op,
                feature,
                high_op,
                high,
            } => format!(
                "{feature}-{}-{}-{}-{}",
                low_op.flipped().name(),
                sanitize_value(low),
                high_op.name(),
                sanitize_value(high)
            ),
        }
    }
}

fn parse_branch(branch: &str) -> Option<Branch> {
    let tokens = tokenize(branch)?;
    let mut tokens = tokens.as_slice();
    let mut modifier = None;
    let mut media_type = None;

    if let [Token::Word(word), rest @ ..] = tokens {
        if word.eq_ignore_ascii_case("not") {
            modifier = Some("not");
            tokens = rest;
        } else if word.eq_ignore_ascii_case("only") {
            modifier = Some("only");
            tokens = rest;
        }
    }
    if let [Token::Word(word), rest @ ..] = tokens {
        let lowered = word.to_ascii_lowercase();
        // A media type is any CSS identifier except the reserved keywords;
        // an unknown type such as `tv` is preserved verbatim, keeping its
        // authored match-nothing behavior.
        if matches!(lowered.as_str(), "not" | "and" | "only" | "or" | "layer")
            || !is_feature_ident(&lowered)
        {
            return None;
        }
        media_type = Some(lowered);
        tokens = rest;
        match tokens {
            [] => {}
            [Token::Word(and), rest @ ..]
                if and.eq_ignore_ascii_case("and") && !rest.is_empty() =>
            {
                tokens = rest;
            }
            _ => return None,
        }
    } else if modifier == Some("only") {
        // `only` requires a media type.
        return None;
    }

    let mut conditions = Vec::new();
    loop {
        match tokens {
            [] => break,
            [Token::Group(content), rest @ ..] => {
                conditions.push(parse_condition(content)?);
                tokens = match rest {
                    [Token::Word(and), next @ ..]
                        if and.eq_ignore_ascii_case("and") && !next.is_empty() =>
                    {
                        next
                    }
                    [] => rest,
                    _ => return None,
                };
            }
            _ => return None,
        }
    }
    if media_type.is_none() && conditions.is_empty() {
        return None;
    }
    if modifier == Some("not") && media_type.is_none() && conditions.len() != 1 {
        // `not (a) and (b)` is ambiguous without grouping; the parser only
        // accepts the single-condition MQ4 form.
        return None;
    }
    Some(Branch {
        modifier,
        media_type,
        conditions,
    })
}

fn parse_condition(content: &str) -> Option<Condition> {
    let content = collapse_whitespace(content);
    if content.is_empty() || content.starts_with("--") {
        // Custom-media references depend on a definition the generated
        // variant cannot carry.
        return None;
    }
    if content.starts_with('(') {
        // Nested grouping inside a condition is not representable.
        return None;
    }
    if let Some((feature, value)) = split_top_level_once(&content, ':') {
        let feature = trim_css_whitespace(feature).to_ascii_lowercase();
        let value = normalize_value(value);
        if !is_feature_ident(&feature) || value.is_empty() {
            return None;
        }
        return Some(match feature.strip_prefix("min-") {
            Some("width") => Condition::Range {
                feature: "width".to_string(),
                op: Comparison::Gte,
                value,
            },
            _ => match feature.strip_prefix("max-") {
                Some("width") => Condition::Range {
                    feature: "width".to_string(),
                    op: Comparison::Lte,
                    value,
                },
                _ => Condition::Plain { feature, value },
            },
        });
    }

    let parts = split_comparisons(&content)?;
    match parts.as_slice() {
        [ComparisonPart::Trailing(operand)] => {
            let feature = trim_css_whitespace(operand).to_ascii_lowercase();
            is_feature_ident(&feature).then_some(Condition::Boolean { feature })
        }
        [
            ComparisonPart::Pair(left, op),
            ComparisonPart::Trailing(right),
        ] => {
            let left_ident = trim_css_whitespace(left).to_ascii_lowercase();
            let right_ident = trim_css_whitespace(right).to_ascii_lowercase();
            if is_feature_ident(&left_ident) && !trim_css_whitespace(right).is_empty() {
                Some(Condition::Range {
                    feature: left_ident,
                    op: *op,
                    value: normalize_value(right),
                })
            } else if is_feature_ident(&right_ident) && !trim_css_whitespace(left).is_empty() {
                Some(Condition::Range {
                    feature: right_ident,
                    op: op.flipped(),
                    value: normalize_value(left),
                })
            } else {
                None
            }
        }
        [
            ComparisonPart::Pair(low, low_op),
            ComparisonPart::Pair(feature, high_op),
            ComparisonPart::Trailing(high),
        ] => {
            let feature = trim_css_whitespace(feature).to_ascii_lowercase();
            let low = normalize_value(low);
            let high = normalize_value(high);
            if !is_feature_ident(&feature) || low.is_empty() || high.is_empty() {
                return None;
            }
            match (low_op, high_op) {
                (Comparison::Lt | Comparison::Lte, Comparison::Lt | Comparison::Lte) => {
                    Some(Condition::DoubleRange {
                        low,
                        low_op: *low_op,
                        feature,
                        high_op: *high_op,
                        high,
                    })
                }
                // A descending chain such as `(60rem > width >= 48rem)` is
                // provably the ascending `(48rem <= width < 60rem)`.
                (Comparison::Gt | Comparison::Gte, Comparison::Gt | Comparison::Gte) => {
                    Some(Condition::DoubleRange {
                        low: high,
                        low_op: high_op.flipped(),
                        feature,
                        high_op: low_op.flipped(),
                        high: low,
                    })
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Collapse ASCII whitespace in a media-feature value and fold case only
/// when every code point is provably case-insensitive: plain keywords,
/// dimensions, and ratios. Values carrying functions or other tokens, such
/// as `env(MyInset)`, keep their authored case. Dimension values also
/// canonicalize provably equivalent spellings: `+52rem`, `052rem`, and
/// `5.2e1rem` all render as `52rem`, so one semantic condition receives one
/// key. No unit conversion is attempted.
fn normalize_value(value: &str) -> String {
    let collapsed = collapse_whitespace(value);
    // A syntactically valid number that overflows f64, such as `1e999px`,
    // must keep its authored spelling: serializing Rust's `inf` would turn
    // a hugely true bound into an identifier that makes the query false.
    if let Some((number, unit)) = parse_css_dimension(&collapsed)
        && number.is_finite()
    {
        return format!("{number}{unit}");
    }
    // Unitless numbers and ratios canonicalize the same provable way:
    // `1.50` renders as `1.5`, and `16 / 9` as `16/9`.
    if let Some(number) = parse_css_number(&collapsed)
        && number.is_finite()
    {
        return format!("{number}");
    }
    if let Some(ratio) = canonical_ratio(&collapsed) {
        return ratio;
    }
    if collapsed.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, ' ' | '.' | '%' | '+' | '-' | '/')
    }) {
        collapsed.to_ascii_lowercase()
    } else {
        collapsed
    }
}

/// Trim only CSS whitespace; other whitespace code points are identifier
/// content and must stay part of the token they touch.
fn trim_css_whitespace(text: &str) -> &str {
    text.trim_matches(|character: char| character.is_ascii_whitespace())
}

enum Token<'a> {
    Word(&'a str),
    /// The content between one balanced pair of top-level parentheses.
    Group(&'a str),
}

fn tokenize(branch: &str) -> Option<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let bytes = branch.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => index += 1,
            b'(' => {
                let mut depth = 1usize;
                let start = index + 1;
                let mut end = start;
                while end < bytes.len() && depth > 0 {
                    match bytes[end] {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    end += 1;
                }
                if depth != 0 {
                    return None;
                }
                tokens.push(Token::Group(&branch[start..end - 1]));
                index = end;
            }
            b')' => return None,
            _ => {
                let start = index;
                while index < bytes.len()
                    && !bytes[index].is_ascii_whitespace()
                    && bytes[index] != b'('
                    && bytes[index] != b')'
                {
                    index += 1;
                }
                // A word running straight into `(` is a CSS function token,
                // as in `not(color)`; splitting it into a keyword and a
                // condition would turn an always-false general-enclosed
                // query into a live condition.
                if bytes.get(index) == Some(&b'(') {
                    return None;
                }
                tokens.push(Token::Word(&branch[start..index]));
            }
        }
    }
    Some(tokens)
}

/// Split on a separator at parenthesis depth zero. Returns `None` when the
/// parentheses are unbalanced.
fn split_top_level(text: &str, separator: char) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1)?,
            _ if character == separator && depth == 0 => {
                parts.push(&text[start..index]);
                start = index + separator.len_utf8();
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    parts.push(&text[start..]);
    Some(parts)
}

fn split_top_level_once(text: &str, separator: char) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (index, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1)?,
            _ if character == separator && depth == 0 => {
                return Some((&text[..index], &text[index + separator.len_utf8()..]));
            }
            _ => {}
        }
    }
    None
}

/// Split `a <= b < c` into operand/operator pairs at depth zero, ending with
/// the trailing operand.
fn split_comparisons(text: &str) -> Option<Vec<ComparisonPart<'_>>> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                depth = depth.checked_sub(1)?;
                index += 1;
            }
            b'<' | b'>' | b'=' if depth == 0 => {
                let operator_end = if bytes.get(index + 1) == Some(&b'=') {
                    index + 2
                } else {
                    index + 1
                };
                let op = Comparison::parse(&text[index..operator_end])?;
                parts.push(ComparisonPart::Pair(&text[start..index], op));
                start = operator_end;
                index = operator_end;
            }
            _ => index += 1,
        }
    }
    parts.push(ComparisonPart::Trailing(&text[start..]));
    // Reject `a < b < c < d` and longer chains.
    if parts.len() > 3 {
        return None;
    }
    Some(parts)
}

enum ComparisonPart<'a> {
    Pair(&'a str, Comparison),
    Trailing(&'a str),
}

/// Split a CSS dimension using the complete CSS number grammar: optional
/// sign, integer and fraction digits, and an optional exponent, followed by
/// an alphabetic unit. The unit folds to lowercase because CSS units are
/// case-insensitive.
fn parse_css_dimension(value: &str) -> Option<(f64, String)> {
    let index = scan_css_number(value.as_bytes())?;
    let unit = &value[index..];
    if unit.is_empty()
        || !unit
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        return None;
    }
    Some((value[..index].parse().ok()?, unit.to_ascii_lowercase()))
}

/// The byte length of the leading CSS number in `bytes`: optional sign,
/// integer and fraction digits, and an optional exponent.
fn scan_css_number(bytes: &[u8]) -> Option<usize> {
    let mut index = 0;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        index += 1;
    }
    let integer_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    let mut has_digits = index > integer_start;
    if bytes.get(index) == Some(&b'.') {
        let fraction_start = index + 1;
        let mut cursor = fraction_start;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == fraction_start {
            return None;
        }
        has_digits = true;
        index = cursor;
    }
    if !has_digits {
        return None;
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        let mut cursor = index + 1;
        if matches!(bytes.get(cursor), Some(b'+' | b'-')) {
            cursor += 1;
        }
        let exponent_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor > exponent_start {
            index = cursor;
        }
    }
    Some(index)
}

/// A complete CSS number with nothing following it, used for unitless
/// values and ratio components.
fn parse_css_number(text: &str) -> Option<f64> {
    let end = scan_css_number(text.as_bytes())?;
    if end != text.len() {
        return None;
    }
    text.parse().ok()
}

/// The canonical form of a `<number> / <number>` ratio value, or `None`
/// when the text is not a plain finite ratio.
fn canonical_ratio(text: &str) -> Option<String> {
    let (left, right) = split_top_level_once(text, '/')?;
    let left = parse_css_number(trim_css_whitespace(left))?;
    let right = parse_css_number(trim_css_whitespace(right))?;
    (left.is_finite() && right.is_finite()).then(|| format!("{left}/{right}"))
}

/// Collapse runs of ASCII whitespace only; other whitespace code points are
/// identifier content to the CSS tokenizer and must not be rewritten.
fn collapse_whitespace(text: &str) -> String {
    text.split(|character: char| character.is_ascii_whitespace())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_feature_ident(text: &str) -> bool {
    // A single leading hyphen admits vendor-prefixed feature names such as
    // `-webkit-min-device-pixel-ratio`; `--*` custom-media references are
    // rejected before this check ever runs.
    let text = text.strip_prefix('-').unwrap_or(text);
    !text.is_empty()
        && text.starts_with(|character: char| character.is_ascii_lowercase())
        && text.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

/// Encode a condition value as a variant-identifier fragment: `47.5rem`
/// becomes `47p5rem`, `33%` becomes `33pct`, and any other unsupported
/// character becomes a hyphen.
fn sanitize_value(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    for character in value.to_ascii_lowercase().chars() {
        match character {
            'a'..='z' | '0'..='9' => sanitized.push(character),
            '.' => sanitized.push('p'),
            '%' => sanitized.push_str("pct"),
            _ => {
                if !sanitized.ends_with('-') {
                    sanitized.push('-');
                }
            }
        }
    }
    sanitized.trim_matches('-').to_string()
}

/// Names must be valid Tailwind variant identifiers; a name that would start
/// with something other than a lowercase letter gains a stable prefix.
fn valid_variant_name(name: &str) -> String {
    let name = name.trim_matches('-').to_string();
    if name.starts_with(|character: char| character.is_ascii_lowercase()) {
        name
    } else {
        format!("mq-{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(query: &str) -> MediaCondition {
        parse_media_condition(query).expect(query)
    }

    #[test]
    fn legacy_min_width_and_range_form_share_one_key() {
        let legacy = parsed("(min-width: 52rem)");
        let range = parsed("(width >= 52rem)");
        let flipped = parsed("(52rem <= width)");
        assert_eq!(legacy.key, "(width >= 52rem)");
        assert_eq!(legacy.key, range.key);
        assert_eq!(legacy.key, flipped.key);
    }

    #[test]
    fn classifies_simple_minimum_width_queries() {
        let condition = parsed("(min-width: 52rem)");
        let simple = condition.simple_min_width.expect("simple");
        assert_eq!(simple.value, "52rem");
        assert_eq!(simple.unit, "rem");
        assert!((simple.number - 52.0).abs() < f64::EPSILON);
    }

    #[test]
    fn media_type_disqualifies_simple_classification() {
        let condition = parsed("screen and (min-width: 52rem)");
        assert!(condition.simple_min_width.is_none());
        assert_eq!(condition.key, "screen and (width >= 52rem)");
        assert_eq!(condition.preferred_custom_name, "screen-width-gte-52rem");
    }

    #[test]
    fn upper_bounds_lists_and_extra_features_are_not_simple() {
        for query in [
            "(min-width: 52rem) and (max-width: 60rem)",
            "(min-width: 52rem), print",
            "(min-width: 52rem) and (hover: hover)",
            "not (min-width: 52rem)",
            "(max-width: 52rem)",
            "(min-width: 0)",
        ] {
            assert!(parsed(query).simple_min_width.is_none(), "{query}");
        }
    }

    #[test]
    fn preserves_media_type_and_inclusive_boundary() {
        let condition = parsed("screen and (width <= 768px)");
        assert_eq!(condition.key, "screen and (width <= 768px)");
        assert_eq!(condition.preferred_custom_name, "screen-width-lte-768px");
    }

    #[test]
    fn double_range_reads_operators_from_the_feature_side() {
        let condition = parsed("(48rem <= width < 60rem)");
        assert_eq!(condition.key, "(48rem <= width < 60rem)");
        assert_eq!(condition.preferred_custom_name, "width-gte-48rem-lt-60rem");
    }

    #[test]
    fn compound_feature_conditions_join_with_and() {
        let condition = parsed("(hover: hover) and (pointer: fine)");
        assert_eq!(condition.key, "(hover: hover) and (pointer: fine)");
        assert_eq!(
            condition.preferred_custom_name,
            "hover-hover-and-pointer-fine"
        );
    }

    #[test]
    fn inclusive_and_exclusive_operators_stay_distinct() {
        let inclusive = parsed("(width <= 768px)");
        let exclusive = parsed("(width < 768px)");
        assert_ne!(inclusive.key, exclusive.key);
        assert_eq!(inclusive.preferred_custom_name, "width-lte-768px");
        assert_eq!(exclusive.preferred_custom_name, "width-lt-768px");
    }

    #[test]
    fn decimal_values_produce_stable_identifiers() {
        assert_eq!(breakpoint_name("47.5rem"), "min-47p5rem");
        assert_eq!(breakpoint_name("52rem"), "min-52rem");
        assert_eq!(breakpoint_name("768px"), "min-768px");
        let condition = parsed("(width <= 47.5rem)");
        assert_eq!(condition.preferred_custom_name, "width-lte-47p5rem");
    }

    #[test]
    fn unit_differences_never_share_a_key() {
        assert_ne!(
            parsed("(min-width: 48rem)").key,
            parsed("(min-width: 768px)").key
        );
    }

    #[test]
    fn normalization_is_case_insensitive_and_whitespace_stable() {
        assert_eq!(
            parsed("SCREEN AND (MIN-WIDTH:  52rem)").key,
            parsed("screen and (min-width: 52rem)").key
        );
    }

    #[test]
    fn branch_order_is_preserved() {
        assert_ne!(parsed("screen, print").key, parsed("print, screen").key);
        assert_eq!(
            parsed("screen, print").preferred_custom_name,
            "screen-or-print"
        );
    }

    #[test]
    fn unrepresentable_conditions_are_rejected() {
        for query in [
            "",
            "(--narrow)",
            "((min-width: 5em) and (max-width: 10em))",
            "(min-width: 5em) or (max-width: 10em)",
            "layer and (min-width: 5em)",
            "or",
            "only (min-width: 5em)",
            "(width < 5em < 10em < 20em)",
            "{ }",
        ] {
            assert!(parse_media_condition(query).is_none(), "{query}");
        }
    }

    #[test]
    fn calc_values_stay_unsimplified() {
        let condition = parsed("(min-width: calc(100vw - 2rem))");
        assert_eq!(condition.key, "(width >= calc(100vw - 2rem))");
        assert!(condition.simple_min_width.is_none());
    }

    #[test]
    fn over_long_names_keep_a_readable_prefix_and_digest() {
        let long = "a".repeat(80);
        let limited = limit_name(&long, "(key)");
        assert!(limited.len() <= MAX_NAME_LENGTH);
        assert!(limited.starts_with("aaaa"));
        assert!(limited.contains('-'));
        assert_eq!(limited, limit_name(&long, "(key)"));
        assert_ne!(limit_name(&long, "(key)"), limit_name(&long, "(other)"));
    }

    #[test]
    fn parses_the_full_css_number_grammar() {
        let exponent = parsed("(min-width: 1e3px)");
        let simple = exponent.simple_min_width.expect("exponent");
        assert_eq!(simple.unit, "px");
        assert!((simple.number - 1000.0).abs() < f64::EPSILON);
        assert!(parsed("(min-width: +52rem)").simple_min_width.is_some());
        let cased = parsed("(min-width: 52REM)");
        assert_eq!(cased.key, "(width >= 52rem)");
        assert_eq!(cased.simple_min_width.expect("cased").unit, "rem");
    }

    #[test]
    fn preserves_case_sensitive_value_tokens() {
        let condition = parsed("(min-width: env(MyInset))");
        assert_eq!(condition.key, "(width >= env(MyInset))");
        assert!(condition.simple_min_width.is_none());
    }

    #[test]
    fn normalizes_descending_ranges_to_the_ascending_form() {
        let descending = parsed("(60rem > width >= 48rem)");
        assert_eq!(descending.key, "(48rem <= width < 60rem)");
        assert_eq!(descending.key, parsed("(48rem <= width < 60rem)").key);
        assert!(parse_media_condition("(60rem > width <= 48rem)").is_none());
    }

    #[test]
    fn non_ascii_whitespace_stays_in_the_condition() {
        let condition = parsed("(orientation:\u{a0}landscape)");
        assert_eq!(condition.key, "(orientation: \u{a0}landscape)");
        assert_ne!(condition.key, parsed("(orientation: landscape)").key);
    }

    #[test]
    fn non_css_whitespace_around_a_feature_rejects_the_condition() {
        // A U+00A0 attached to the feature name is identifier content; the
        // authored condition names an unknown feature and stays false, so it
        // must not become a live `(orientation: landscape)`.
        assert!(parse_media_condition("(\u{a0}orientation: landscape)").is_none());
        assert!(parse_media_condition("(orientation\u{a0}: landscape)").is_none());
    }

    #[test]
    fn equivalent_dimension_spellings_share_one_key() {
        let canonical = parsed("(min-width: 52rem)");
        for query in [
            "(min-width: +52rem)",
            "(min-width: 052rem)",
            "(min-width: 5.2e1rem)",
        ] {
            assert_eq!(parsed(query).key, canonical.key, "{query}");
        }
        assert_eq!(canonical.simple_min_width.expect("simple").value, "52rem");
        assert_eq!(parsed("(width <= 47.50rem)").key, "(width <= 47.5rem)");
    }

    #[test]
    fn custom_media_types_are_preserved_verbatim() {
        let condition = parsed("tv and (color)");
        assert_eq!(condition.key, "tv and (color)");
        assert_eq!(condition.preferred_custom_name, "tv-color");
        assert_eq!(parsed("only projection").key, "only projection");
    }

    #[test]
    fn overflowing_dimensions_keep_their_authored_spelling() {
        let condition = parsed("(max-width: 1e999px)");
        assert_eq!(condition.key, "(width <= 1e999px)");
        assert!(parsed("(min-width: 1e999rem)").simple_min_width.is_none());
    }

    #[test]
    fn equivalent_ratio_spellings_share_one_key() {
        let canonical = parsed("(aspect-ratio: 16/9)");
        for query in [
            "(aspect-ratio: 16 / 9)",
            "(aspect-ratio: 16.0/9)",
            "(aspect-ratio: +16/09)",
        ] {
            assert_eq!(parsed(query).key, canonical.key, "{query}");
        }
        assert_eq!(canonical.key, "(aspect-ratio: 16/9)");
        assert_ne!(parsed("(aspect-ratio: 16/10)").key, canonical.key);
        assert_eq!(parsed("(aspect-ratio: 1.50)").key, "(aspect-ratio: 1.5)");
    }

    #[test]
    fn vertical_tab_is_identifier_content_not_whitespace() {
        // Rust's is_ascii_whitespace matches exactly the CSS whitespace set
        // (space, tab, LF, FF, CR); U+000B stays in the condition.
        let condition = parsed("(orientation:\u{b}landscape)");
        assert_eq!(condition.key, "(orientation: \u{b}landscape)");
        assert_ne!(condition.key, parsed("(orientation: landscape)").key);
    }

    #[test]
    fn function_tokens_are_not_keyword_condition_pairs() {
        for query in ["not(color)", "screen and(color)", "only screen and(color)"] {
            assert!(parse_media_condition(query).is_none(), "{query}");
        }
    }

    #[test]
    fn vendor_prefixed_features_are_representable() {
        let condition = parsed("(-webkit-min-device-pixel-ratio: 2)");
        assert_eq!(condition.key, "(-webkit-min-device-pixel-ratio: 2)");
        assert_eq!(
            condition.preferred_custom_name,
            "webkit-min-device-pixel-ratio-2"
        );
        assert!(parse_media_condition("(--narrow)").is_none());
    }

    #[test]
    fn digests_are_stable_and_key_dependent() {
        assert_eq!(condition_digest("(width >= 52rem)").len(), 8);
        assert_eq!(
            condition_digest("(width >= 52rem)"),
            condition_digest("(width >= 52rem)")
        );
        assert_ne!(
            condition_digest("(width >= 52rem)"),
            condition_digest("(width >= 60rem)")
        );
    }
}

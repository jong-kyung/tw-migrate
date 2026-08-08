//! Media query decomposition, condition keys, and generated names.
//!
//! This module normalizes `@media` conditions and decomposes `and`-joined
//! single-branch conditions into components, each of which becomes one
//! stacked variant. Comma lists, `or`-joined conditions, and negations
//! spanning more than one part cannot decompose, because variant stacking
//! nests `@media` blocks and nesting is exactly conjunction; they keep one
//! whole key. It performs no unit conversion and never merges conditions
//! whose equivalence it cannot prove: `48rem` and `768px` stay distinct
//! keys even when a typical browser renders them at the same width.

use std::collections::{HashMap, HashSet};

use oxc_css_parser::ast::{AtRule, Statement};
use serde::{Deserialize, Serialize};

use crate::{
    at_rules::{
        at_rule_query, builtin_media_variant, media_breakpoint_variant, media_feature_variant,
        parse_css,
    },
    planner::StylesheetSyntax,
};

use std::hash::{DefaultHasher, Hasher};

/// The longest readable generated name; anything longer takes the digest.
const MAX_NAME_LENGTH: usize = 48;

/// One media condition after normalization and decomposition.
pub(crate) enum ParsedMediaCondition {
    /// An `and`-joined single branch, split into components in authored
    /// order. Consumers stack the component variants.
    Components(Vec<MediaComponent>),
    /// A comma list, `or`-joined condition, or multi-part negation, kept as
    /// one whole variant.
    Whole(MediaComponent),
}

/// One generated-variant unit: a component of a decomposed condition, or a
/// complete non-decomposable condition.
pub(crate) struct MediaComponent {
    /// Canonical condition text; the generated definition wraps
    /// `@media <key>`.
    pub(crate) key: String,
    /// Readable variant name, present only when it derives cleanly from
    /// lowercase letters, digits, hyphens, and `p` for decimal points, and
    /// fits the length limit. `None` sends the component to the digest name.
    pub(crate) readable_name: Option<String>,
    /// Compact feature text for built-in variant table lookup, such as
    /// `(prefers-color-scheme:dark)` or `print`. Absent for negations and
    /// non-atomic shapes.
    pub(crate) builtin_query: Option<String>,
    /// The width bound of a bare width range component, used to match
    /// existing project breakpoints.
    pub(crate) width_bound: Option<WidthBound>,
}

/// One provable width bound of a width-only component.
pub(crate) struct WidthBound {
    pub(crate) value: String,
    pub(crate) inclusive: bool,
    /// True for a lower bound (`width >= v`), false for an upper bound.
    pub(crate) lower: bool,
}

/// Parse, normalize, and decompose a media query condition (the text after
/// `@media`). Returns `None` when the condition cannot be represented
/// safely: unknown syntax, nested grouping, custom-media references, or
/// characters that a generated definition cannot carry.
pub(crate) fn parse_media_condition(query: &str) -> Option<ParsedMediaCondition> {
    // Only ASCII whitespace is insignificant in CSS; other whitespace code
    // points are identifier content and must survive untouched.
    let query = trim_css_whitespace(query);
    if query.is_empty()
        || query.contains(['{', '}', ';', '"', '\'', '\\', '[', ']'])
        // Comment placement can be significant inside function values such
        // as calc(), so a commented prelude stays on the retention path
        // instead of being rewritten.
        || query.contains("/*")
    {
        return None;
    }
    let branches = split_top_level(query, ',')?;
    let mut parsed = Vec::with_capacity(branches.len());
    for branch in &branches {
        parsed.push(parse_branch(branch)?);
    }

    if parsed.len() > 1 {
        return Some(ParsedMediaCondition::Whole(whole_component(&parsed)));
    }
    let [branch] = parsed.as_slice() else {
        return None;
    };
    if branch.connector == "or" {
        return Some(ParsedMediaCondition::Whole(whole_component(&parsed)));
    }
    if branch.modifier == Some("not") {
        let parts = usize::from(branch.media_type.is_some()) + branch.conditions.len();
        if parts > 1 {
            // The negation of a conjunction is a disjunction, which
            // stacking cannot express.
            return Some(ParsedMediaCondition::Whole(whole_component(&parsed)));
        }
        return Some(ParsedMediaCondition::Components(vec![negated_component(
            branch,
        )?]));
    }

    let mut components = Vec::new();
    if let Some(media_type) = &branch.media_type {
        components.push(type_component(branch.modifier, media_type));
    }
    for condition in &branch.conditions {
        match condition {
            Condition::DoubleRange {
                low,
                low_op,
                feature,
                high_op,
                high,
            } => {
                // A double range is the conjunction of its bounds, so each
                // bound becomes its own shareable component.
                components.push(feature_component(&Condition::Range {
                    feature: feature.clone(),
                    op: low_op.flipped(),
                    value: low.clone(),
                }));
                components.push(feature_component(&Condition::Range {
                    feature: feature.clone(),
                    op: *high_op,
                    value: high.clone(),
                }));
            }
            _ => components.push(feature_component(condition)),
        }
    }
    Some(ParsedMediaCondition::Components(components))
}

/// A short stable digest of a key, rendered as sixteen lowercase hex
/// digits, used for the `twm-media-<digest>` fallback name.
pub(crate) fn condition_digest(key: &str) -> String {
    // `DefaultHasher::new()` is keyless and therefore deterministic across
    // runs and platforms within one Rust release, which idempotent reruns
    // need. Its algorithm may change between Rust releases; that is
    // accepted for a one-shot migration CLI, where the worst outcome is a
    // harmless duplicate definition after a toolchain upgrade. Digest
    // collisions are contained by the resolver: a name is claimed by at
    // most one key and every later claimant falls back.
    let mut hasher = DefaultHasher::new();
    hasher.write(key.as_bytes());
    format!("{:016x}", hasher.finish())
}

fn type_component(modifier: Option<&'static str>, media_type: &str) -> MediaComponent {
    let key = match modifier {
        Some(modifier) => format!("{modifier} {media_type}"),
        None => media_type.to_string(),
    };
    let readable = match modifier {
        Some(modifier) => format!("{modifier}-{media_type}"),
        None => media_type.to_string(),
    };
    MediaComponent {
        key,
        readable_name: clean_name(readable),
        // A bare type such as `print` may match a built-in variant.
        builtin_query: modifier.is_none().then(|| media_type.to_string()),
        width_bound: None,
    }
}

fn feature_component(condition: &Condition) -> MediaComponent {
    MediaComponent {
        key: condition.render(),
        readable_name: condition.name_tokens().and_then(clean_name),
        builtin_query: Some(condition.builtin_query()),
        width_bound: condition.width_bound(),
    }
}

/// The single negated part of a `not` branch: `not screen` or `not (hover)`.
fn negated_component(branch: &Branch) -> Option<MediaComponent> {
    let (key, readable) = if let Some(media_type) = &branch.media_type {
        (
            format!("not {media_type}"),
            Some(format!("not-{media_type}")),
        )
    } else {
        let [condition] = branch.conditions.as_slice() else {
            return None;
        };
        (
            format!("not {}", condition.render()),
            condition.name_tokens().map(|name| format!("not-{name}")),
        )
    };
    Some(MediaComponent {
        key,
        readable_name: readable.and_then(clean_name),
        builtin_query: None,
        width_bound: None,
    })
}

/// One whole variant for a non-decomposable condition, joining the
/// component tokens for the readable name.
fn whole_component(branches: &[Branch]) -> MediaComponent {
    let key = branches
        .iter()
        .map(Branch::render)
        .collect::<Vec<_>>()
        .join(", ");
    let readable = branches
        .iter()
        .map(Branch::name_tokens)
        .collect::<Option<Vec<_>>>()
        .map(|names| names.join("-or-"));
    MediaComponent {
        key,
        readable_name: readable.and_then(clean_name),
        builtin_query: None,
        width_bound: None,
    }
}

/// Enforce name validity: a leading hyphen from a vendor-prefixed feature
/// is stripped, the name must start with a lowercase letter, and anything
/// over the length limit takes the digest name instead.
fn clean_name(name: String) -> Option<String> {
    let name = name.trim_start_matches('-').to_string();
    // Underscores are significant to Tailwind's candidate parsing, so a
    // name carrying one is not safely readable and takes the digest name.
    (name.len() <= MAX_NAME_LENGTH
        && !name.contains('_')
        && name.starts_with(|character: char| character.is_ascii_lowercase()))
    .then_some(name)
}

struct Branch {
    modifier: Option<&'static str>,
    media_type: Option<String>,
    conditions: Vec<Condition>,
    /// The uniform separator between conditions: `and`, or the MQ4 `or`
    /// form, which never mixes with `and` or follows a media type.
    connector: &'static str,
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
    /// `(low lowOp feature highOp high)`, kept in ascending form.
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
            if index > 0 {
                parts.push(self.connector.to_string());
            } else if self.media_type.is_some() {
                parts.push("and".to_string());
            }
            parts.push(condition.render());
        }
        parts.join(" ")
    }

    fn name_tokens(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(modifier) = self.modifier {
            parts.push(modifier.to_string());
        }
        if let Some(media_type) = &self.media_type {
            parts.push(media_type.clone());
        }
        for (index, condition) in self.conditions.iter().enumerate() {
            if index > 0 {
                parts.push(self.connector.to_string());
            } else if self.media_type.is_some() {
                parts.push("and".to_string());
            }
            parts.push(condition.name_tokens()?);
        }
        Some(parts.join("-"))
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

    /// The readable name fragment, or `None` when a value cannot contribute
    /// clean tokens; there is no character mangling.
    fn name_tokens(&self) -> Option<String> {
        Some(match self {
            Self::Plain { feature, value } => format!("{feature}-{}", value_name(value)?),
            Self::Boolean { feature } => feature.clone(),
            Self::Range { feature, op, value } => {
                format!("{feature}-{}-{}", op.name(), value_name(value)?)
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
                value_name(low)?,
                high_op.name(),
                value_name(high)?
            ),
        })
    }

    /// Compact feature text for built-in table lookup.
    fn builtin_query(&self) -> String {
        match self {
            Self::Plain { feature, value } => format!("({feature}:{value})"),
            Self::Boolean { feature } => format!("({feature})"),
            Self::Range { .. } | Self::DoubleRange { .. } => self.render(),
        }
    }

    fn width_bound(&self) -> Option<WidthBound> {
        let Self::Range { feature, op, value } = self else {
            return None;
        };
        // Equality is a single-width condition, not a bound; exposing it
        // here would let breakpoint matching broaden `(width = 48rem)` to
        // the exclusive upper bound `max-*` covers.
        if feature != "width" || matches!(op, Comparison::Eq) {
            return None;
        }
        Some(WidthBound {
            value: value.clone(),
            inclusive: matches!(op, Comparison::Lte | Comparison::Gte),
            lower: matches!(op, Comparison::Gt | Comparison::Gte),
        })
    }
}

/// Encode a value as a name fragment: lowercase letters and digits pass
/// through and a decimal point becomes `p`. Any other character means the
/// value cannot contribute a clean token.
fn value_name(value: &str) -> Option<String> {
    let mut name = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            'a'..='z' | '0'..='9' => name.push(character),
            '.' => name.push('p'),
            _ => return None,
        }
    }
    (!name.is_empty()).then_some(name)
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
    let mut connector: Option<&'static str> = None;
    loop {
        match tokens {
            [] => break,
            [Token::Group(content), rest @ ..] => {
                conditions.push(parse_condition(content)?);
                tokens = match rest {
                    [Token::Word(word), next @ ..] if !next.is_empty() => {
                        let separator = if word.eq_ignore_ascii_case("and") {
                            "and"
                        } else if word.eq_ignore_ascii_case("or") {
                            "or"
                        } else {
                            return None;
                        };
                        // MQ4 `or` joins bare condition groups only: it
                        // never mixes with `and` at one level and never
                        // follows a media type or modifier.
                        if separator == "or" && (media_type.is_some() || modifier.is_some()) {
                            return None;
                        }
                        match connector {
                            None => connector = Some(separator),
                            Some(existing) if existing == separator => {}
                            Some(_) => return None,
                        }
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
        connector: connector.unwrap_or("and"),
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
        let value = normalize_value(&feature, value);
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
                    op: *op,
                    value: normalize_value(&left_ident, right),
                    feature: left_ident,
                })
            } else if is_feature_ident(&right_ident) && !trim_css_whitespace(left).is_empty() {
                Some(Condition::Range {
                    op: op.flipped(),
                    value: normalize_value(&right_ident, left),
                    feature: right_ident,
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
            let low = normalize_value(&feature, low);
            let high = normalize_value(&feature, high);
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
/// as `env(MyInset)`, keep their authored case. Dimensions, unitless
/// numbers, and ratios canonicalize exact lexical equivalence: `+052rem`
/// and `5.2e1rem` both render as `52rem`, and `16 / 9` as `16/9`. A number
/// whose exact decimal form would be unreasonably long, such as `1e999px`
/// or `1e-324px`, keeps its authored spelling instead of rounding through
/// f64. No unit conversion is attempted.
fn normalize_value(feature: &str, value: &str) -> String {
    let collapsed = collapse_whitespace(value);
    if let Some((number, unit)) = canonical_dimension(&collapsed) {
        return format!("{number}{unit}");
    }
    if is_integer_feature(feature) {
        // `<integer>` features reject `<number>` spellings, so `1.0` for
        // `color` is an invalid, non-matching query; only the integer
        // grammar is provably equivalent, and other spellings keep their
        // authored form.
        if let Some(integer) = canonical_css_integer(&collapsed) {
            return integer;
        }
    } else {
        if let Some(number) = canonical_css_number(&collapsed) {
            return number;
        }
        if let Some(ratio) = canonical_ratio(&collapsed) {
            return ratio;
        }
    }
    if collapsed.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, ' ' | '.' | '%' | '+' | '-' | '/')
    }) {
        collapsed.to_ascii_lowercase()
    } else {
        collapsed
    }
}

/// Features whose values are `<integer>` tokens.
fn is_integer_feature(feature: &str) -> bool {
    let base = feature
        .strip_prefix("min-")
        .or_else(|| feature.strip_prefix("max-"))
        .unwrap_or(feature);
    matches!(
        base,
        "color"
            | "color-index"
            | "monochrome"
            | "grid"
            | "horizontal-viewport-segments"
            | "vertical-viewport-segments"
    )
}

/// The canonical form of a CSS `<integer>`: optional sign and digits only.
fn canonical_css_integer(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut index = 0;
    let negative = match bytes.first() {
        Some(b'-') => {
            index += 1;
            true
        }
        Some(b'+') => {
            index += 1;
            false
        }
        _ => false,
    };
    if index == bytes.len() || !bytes[index..].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let digits = text[index..].trim_start_matches('0');
    if digits.is_empty() {
        return Some("0".to_string());
    }
    Some(if negative {
        format!("-{digits}")
    } else {
        digits.to_string()
    })
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

/// Split `a <= b < c` into operand/operator pairs at depth zero, ending
/// with the trailing operand.
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

/// The exact plain-decimal canonical form of a complete CSS number, built
/// lexically so no precision is lost through f64: `+052` and `5.2e1` both
/// become `52`, while `1.0000000000000001` keeps every digit. Returns
/// `None` when the text is not exactly one number or when the exact form
/// would exceed a fixed length, in which case the caller preserves the
/// authored spelling.
fn canonical_css_number(text: &str) -> Option<String> {
    const MAX_RENDERED_DIGITS: usize = 64;
    let bytes = text.as_bytes();
    let mut index = 0;
    let negative = match bytes.first() {
        Some(b'-') => {
            index += 1;
            true
        }
        Some(b'+') => {
            index += 1;
            false
        }
        _ => false,
    };
    let integer_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    let integer = &text[integer_start..index];
    let mut fraction = "";
    if bytes.get(index) == Some(&b'.') {
        let fraction_start = index + 1;
        let mut cursor = fraction_start;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == fraction_start {
            return None;
        }
        fraction = &text[fraction_start..cursor];
        index = cursor;
    }
    if integer.is_empty() && fraction.is_empty() {
        return None;
    }
    let mut exponent: i64 = 0;
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        let mut cursor = index + 1;
        let negative_exponent = match bytes.get(cursor) {
            Some(b'-') => {
                cursor += 1;
                true
            }
            Some(b'+') => {
                cursor += 1;
                false
            }
            _ => false,
        };
        let exponent_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == exponent_start {
            return None;
        }
        // Leading zeros carry no magnitude: `1e00000` is exactly `1`.
        let exponent_digits = text[exponent_start..cursor].trim_start_matches('0');
        if exponent_digits.len() > 4 {
            return None;
        }
        exponent = if exponent_digits.is_empty() {
            0
        } else {
            exponent_digits.parse().ok()?
        };
        if negative_exponent {
            exponent = -exponent;
        }
        index = cursor;
    }
    if index != text.len() {
        return None;
    }

    let digits = format!("{integer}{fraction}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some("0".to_string());
    }
    let shift = exponent - fraction.len() as i64;
    let mut rendered = String::new();
    if shift >= 0 {
        if digits.len() as i64 + shift > MAX_RENDERED_DIGITS as i64 {
            return None;
        }
        rendered.push_str(digits);
        for _ in 0..shift {
            rendered.push('0');
        }
    } else {
        let places = usize::try_from(-shift).ok()?;
        if places > MAX_RENDERED_DIGITS {
            return None;
        }
        if digits.len() > places {
            let split = digits.len() - places;
            rendered.push_str(&digits[..split]);
            let fractional = digits[split..].trim_end_matches('0');
            if !fractional.is_empty() {
                rendered.push('.');
                rendered.push_str(fractional);
            }
        } else {
            let padded = format!("{}{}", "0".repeat(places - digits.len()), digits);
            let fractional = padded.trim_end_matches('0');
            rendered.push('0');
            if !fractional.is_empty() {
                rendered.push('.');
                rendered.push_str(fractional);
            }
        }
    }
    Some(if negative && rendered != "0" {
        format!("-{rendered}")
    } else {
        rendered
    })
}

/// The exact canonical form of a `<number><unit>` dimension, with the unit
/// folded to lowercase.
pub(crate) fn canonical_dimension(text: &str) -> Option<(String, String)> {
    let end = scan_css_number(text.as_bytes())?;
    let unit = &text[end..];
    if unit.is_empty()
        || !unit
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        return None;
    }
    Some((
        canonical_css_number(&text[..end])?,
        unit.to_ascii_lowercase(),
    ))
}

/// The canonical form of a `<number> / <number>` ratio value, or `None`
/// when the text is not a plain exact ratio.
fn canonical_ratio(text: &str) -> Option<String> {
    let (left, right) = split_top_level_once(text, '/')?;
    let left = canonical_css_number(trim_css_whitespace(left))?;
    let right = canonical_css_number(trim_css_whitespace(right))?;
    Some(format!("{left}/{right}"))
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

/// Collapse runs of ASCII whitespace only; other whitespace code points
/// are identifier content to the CSS tokenizer and must not be rewritten.
fn collapse_whitespace(text: &str) -> String {
    text.split(|character: char| character.is_ascii_whitespace())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_feature_ident(text: &str) -> bool {
    // A single leading hyphen admits vendor-prefixed feature names such as
    // `-webkit-min-device-pixel-ratio`; `--*` custom-media references are
    // rejected before this check ever runs. Underscores are valid CSS
    // identifier code points, so `foo_bar` stays representable; its
    // readable name is rejected separately and the digest name applies.
    let text = text.strip_prefix('-').unwrap_or(text);
    !text.is_empty()
        && text.starts_with(|character: char| character.is_ascii_lowercase() || character == '_')
        && text.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '_'
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn components(query: &str) -> Vec<MediaComponent> {
        match parse_media_condition(query).expect(query) {
            ParsedMediaCondition::Components(components) => components,
            ParsedMediaCondition::Whole(_) => panic!("expected components for {query}"),
        }
    }

    fn whole(query: &str) -> MediaComponent {
        match parse_media_condition(query).expect(query) {
            ParsedMediaCondition::Whole(component) => component,
            ParsedMediaCondition::Components(_) => panic!("expected whole for {query}"),
        }
    }

    fn keys(query: &str) -> Vec<String> {
        components(query)
            .into_iter()
            .map(|component| component.key)
            .collect()
    }

    fn names(query: &str) -> Vec<Option<String>> {
        components(query)
            .into_iter()
            .map(|component| component.readable_name)
            .collect()
    }

    #[test]
    fn decomposes_and_joined_conditions_in_authored_order() {
        assert_eq!(
            keys("screen and (width <= 768px)"),
            ["screen", "(width <= 768px)"]
        );
        assert_eq!(
            names("screen and (width <= 768px)"),
            [
                Some("screen".to_string()),
                Some("width-lte-768px".to_string())
            ]
        );
        assert_eq!(
            keys("screen and (prefers-color-scheme: dark) and (hover: hover)"),
            ["screen", "(prefers-color-scheme: dark)", "(hover: hover)"]
        );
    }

    #[test]
    fn double_ranges_decompose_into_shared_bounds() {
        assert_eq!(
            keys("(48rem <= width < 60rem)"),
            ["(width >= 48rem)", "(width < 60rem)"]
        );
        assert_eq!(
            names("(48rem <= width < 60rem)"),
            [
                Some("width-gte-48rem".to_string()),
                Some("width-lt-60rem".to_string())
            ]
        );
        assert_eq!(keys("(min-width: 52rem)"), ["(width >= 52rem)"]);
        assert_eq!(
            keys("(60rem > width >= 48rem)"),
            ["(width >= 48rem)", "(width < 60rem)"]
        );
    }

    #[test]
    fn width_bounds_expose_breakpoint_matching_shape() {
        let bounds: Vec<_> = components("(48rem <= width < 60rem)")
            .into_iter()
            .map(|component| component.width_bound.expect("bound"))
            .collect();
        assert!(bounds[0].lower && bounds[0].inclusive);
        assert_eq!(bounds[0].value, "48rem");
        assert!(!bounds[1].lower && !bounds[1].inclusive);
        assert_eq!(bounds[1].value, "60rem");
        assert!(components("(hover: hover)")[0].width_bound.is_none());
    }

    #[test]
    fn builtin_lookup_text_is_compact() {
        let compound = components("screen and (prefers-color-scheme: dark)");
        assert_eq!(compound[0].builtin_query.as_deref(), Some("screen"));
        assert_eq!(
            compound[1].builtin_query.as_deref(),
            Some("(prefers-color-scheme:dark)")
        );
        assert_eq!(
            components("print")[0].builtin_query.as_deref(),
            Some("print")
        );
    }

    #[test]
    fn modifiers_stay_attached_to_their_component() {
        let only = components("only screen and (color)");
        assert_eq!(only[0].key, "only screen");
        assert_eq!(only[0].readable_name.as_deref(), Some("only-screen"));
        assert!(only[0].builtin_query.is_none());

        let negated_type = components("not screen");
        assert_eq!(negated_type[0].key, "not screen");
        assert_eq!(negated_type[0].readable_name.as_deref(), Some("not-screen"));

        let negated_feature = components("not (hover)");
        assert_eq!(negated_feature[0].key, "not (hover)");
        assert_eq!(
            negated_feature[0].readable_name.as_deref(),
            Some("not-hover")
        );
        assert!(negated_feature[0].builtin_query.is_none());
    }

    #[test]
    fn non_decomposable_conditions_keep_one_whole_key() {
        let comma = whole("screen, print");
        assert_eq!(comma.key, "screen, print");
        assert_eq!(comma.readable_name.as_deref(), Some("screen-or-print"));

        let or_joined = whole("(color) or (hover)");
        assert_eq!(or_joined.key, "(color) or (hover)");
        assert_eq!(or_joined.readable_name.as_deref(), Some("color-or-hover"));

        let negated = whole("not screen and (color)");
        assert_eq!(negated.key, "not screen and (color)");
        assert_eq!(
            negated.readable_name.as_deref(),
            Some("not-screen-and-color")
        );
    }

    #[test]
    fn unclean_values_lose_only_their_readable_name() {
        let calc = components("screen and (min-width: calc(100vw - 2rem))");
        assert_eq!(calc[0].readable_name.as_deref(), Some("screen"));
        assert_eq!(calc[1].key, "(width >= calc(100vw - 2rem))");
        assert!(calc[1].readable_name.is_none());

        let env = components("(min-width: env(MyInset))");
        assert_eq!(env[0].key, "(width >= env(MyInset))");
        assert!(env[0].readable_name.is_none());

        let long_value = format!("(min-width: {}rem)", "1".repeat(60));
        assert!(components(&long_value)[0].readable_name.is_none());
    }

    #[test]
    fn keys_canonicalize_exact_equivalence_only() {
        for query in [
            "(min-width: +52rem)",
            "(min-width: 052rem)",
            "(min-width: 5.2e1rem)",
            "(min-width: 52REM)",
        ] {
            assert_eq!(keys(query), ["(width >= 52rem)"], "{query}");
        }
        assert_eq!(keys("(min-width: 1e3px)"), ["(width >= 1000px)"]);
        assert_eq!(
            keys("(min-width: 1.0000000000000001px)"),
            ["(width >= 1.0000000000000001px)"]
        );
        assert_eq!(keys("(min-width: 1e-324px)"), ["(width >= 1e-324px)"]);
        assert_eq!(keys("(aspect-ratio: 16 / 9)"), ["(aspect-ratio: 16/9)"]);
        assert_ne!(keys("(min-width: 48rem)"), keys("(min-width: 768px)"));
    }

    #[test]
    fn integer_feature_values_keep_number_spellings() {
        assert_eq!(keys("(color: 1.0)"), ["(color: 1.0)"]);
        assert_eq!(keys("(color: +01)"), ["(color: 1)"]);
        assert_eq!(keys("(grid: 1e0)"), ["(grid: 1e0)"]);
        assert_eq!(
            keys("(horizontal-viewport-segments: 1.0)"),
            ["(horizontal-viewport-segments: 1.0)"]
        );
    }

    #[test]
    fn case_and_whitespace_rules_are_css_exact() {
        assert_eq!(
            keys("SCREEN AND (MIN-WIDTH: 52rem)"),
            keys("screen and (min-width: 52rem)")
        );
        assert_eq!(
            keys("(orientation:\u{a0}landscape)"),
            ["(orientation: \u{a0}landscape)"]
        );
        assert!(parse_media_condition("(\u{a0}orientation: landscape)").is_none());
        assert_eq!(
            keys("(orientation:\u{b}landscape)"),
            ["(orientation: \u{b}landscape)"]
        );
        // Comment placement can be significant inside function values such
        // as calc(), so commented preludes are rejected wholesale rather
        // than rewritten.
        for query in [
            "screen/**/and (color)",
            "(min-width:/* tablet */52rem)",
            "(min-width: calc(1px/**/+/**/2px))",
            "(min-width: /* 52rem)",
        ] {
            assert!(parse_media_condition(query).is_none(), "{query}");
        }
    }

    #[test]
    fn unrepresentable_conditions_are_rejected() {
        for query in [
            "",
            "(--narrow)",
            "((min-width: 5em) and (max-width: 10em))",
            "layer and (min-width: 5em)",
            "or",
            "only (min-width: 5em)",
            "not(color)",
            "screen and(color)",
            "(color) or (hover) and (pointer: fine)",
            "screen or (color)",
            "not (color) or (hover)",
            "(width < 5em < 10em < 20em)",
            "{ }",
        ] {
            assert!(parse_media_condition(query).is_none(), "{query}");
        }
    }

    #[test]
    fn equality_bounds_never_match_breakpoints() {
        let component = &components("(width = 48rem)")[0];
        assert_eq!(component.key, "(width = 48rem)");
        assert!(component.width_bound.is_none());
        assert_eq!(component.readable_name.as_deref(), Some("width-eq-48rem"));
    }

    #[test]
    fn underscore_identifiers_are_representable_with_digest_names() {
        let media_type = &components("foo_bar")[0];
        assert_eq!(media_type.key, "foo_bar");
        assert!(media_type.readable_name.is_none());
        let feature = &components("(foo_bar: baz)")[0];
        assert_eq!(feature.key, "(foo_bar: baz)");
        assert!(feature.readable_name.is_none());
    }

    #[test]
    fn vendor_prefixed_features_are_representable() {
        let component = &components("(-webkit-min-device-pixel-ratio: 2)")[0];
        assert_eq!(component.key, "(-webkit-min-device-pixel-ratio: 2)");
        assert_eq!(
            component.readable_name.as_deref(),
            Some("webkit-min-device-pixel-ratio-2")
        );
    }

    #[test]
    fn digests_are_sixteen_hex_and_key_dependent() {
        let digest = condition_digest("(width >= 52rem)");
        assert_eq!(digest.len(), 16);
        assert!(
            digest
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        assert_eq!(digest, condition_digest("(width >= 52rem)"));
        assert_ne!(digest, condition_digest("(width >= 60rem)"));
    }
}

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
    /// Scan position of the first rule that uses this key, in request
    /// stylesheet order; cascade-position ordering builds on it.
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
pub fn collect_media_conditions_json(request: &str) -> Result<String, String> {
    let request: MediaCollectionRequest =
        serde_json::from_str(request).map_err(|error| format!("Invalid request: {error}"))?;

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
                            return Err(format!(
                                "Invalid Vue style block span in {}",
                                stylesheet.css_path
                            ));
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
            let parsed = parse_css(&allocator, source, syntax.parser_syntax())
                .map_err(|error| format!("Failed to parse {}: {error}", stylesheet.css_path))?;
            let mut at_rules = Vec::new();
            walk_media_at_rules(&parsed.statements, &mut |at_rule| at_rules.push(at_rule));
            for at_rule in at_rules {
                if at_rule.block.is_none() {
                    continue;
                }
                // Whole-condition matches the planner already converts stay
                // on their existing path and need no definitions.
                if media_feature_variant(at_rule, source).is_some()
                    || media_breakpoint_variant(at_rule, source, &request.theme_tokens).is_some()
                {
                    continue;
                }
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
    .map_err(|error| error.to_string())
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
    let canonical = canonical_dimension(&bound.value)?;
    let name = theme_tokens
        .iter()
        .filter_map(|(name, token_value)| {
            let name = name.strip_prefix("breakpoint-")?;
            (canonical_dimension(token_value.trim())? == canonical).then_some(name)
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
) -> Result<(), String> {
    let allocator = oxc_css_parser::Allocator::default();
    let parsed = parse_css(&allocator, source, oxc_css_parser::Syntax::Css)
        .map_err(|error| format!("Failed to parse {path}: {error}"))?;
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
    let query = at_rule_query(media, source, "media")?;
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

#[cfg(test)]
mod collection_tests {
    use serde_json::{Value, json};

    fn collect(request: Value) -> Value {
        let response = super::collect_media_conditions_json(&request.to_string()).unwrap();
        serde_json::from_str(&response).unwrap()
    }

    fn rem_tokens() -> Value {
        json!({ "breakpoint-md": "48rem", "breakpoint-lg": "64rem" })
    }

    #[test]
    fn skips_whole_conditions_the_planner_already_converts() {
        let response = collect(json!({
            "stylesheets": [{
                "cssPath": "card.css",
                "cssSource": "@media print { .card { margin: 0; } }\n\
                    @media (prefers-color-scheme: dark) { .card { color: white; } }\n\
                    @media (min-width: 48rem) { .card { padding: 1rem; } }",
            }],
            "themeTokens": rem_tokens(),
        }));
        assert_eq!(response["components"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn decomposes_compounds_and_reports_matches_per_component() {
        let response = collect(json!({
            "stylesheets": [{
                "cssPath": "card.css",
                "cssSource": "@media screen and (prefers-color-scheme: dark) { .card { color: white; } }\n\
                    @media (48rem <= width < 64rem) { .card { margin: 0; } }",
            }],
            "themeTokens": rem_tokens(),
        }));
        let components = response["components"].as_array().unwrap();
        let by_key: std::collections::HashMap<_, _> = components
            .iter()
            .map(|component| (component["key"].as_str().unwrap(), component))
            .collect();
        assert_eq!(components.len(), 4);
        assert_eq!(by_key["(prefers-color-scheme: dark)"]["builtin"], "dark");
        assert_eq!(by_key["screen"]["builtin"], Value::Null);
        assert_eq!(by_key["screen"]["readableName"], "screen");
        assert_eq!(by_key["(width >= 48rem)"]["breakpoint"], "md");
        assert_eq!(by_key["(width < 64rem)"]["breakpoint"], "max-lg");
    }

    #[test]
    fn components_deduplicate_across_conditions_and_stylesheets() {
        let response = collect(json!({
            "stylesheets": [
                {
                    "cssPath": "a.css",
                    "cssSource": ".card { @media screen and (width <= 768px) { margin: 0; } }",
                },
                {
                    "cssPath": "b.css",
                    "cssSource": "@media screen and (hover: hover) { .other { margin: 1px; } }\n\
                        @media (max-width: 768px) { .other { color: red; } }",
                },
            ],
            "themeTokens": rem_tokens(),
        }));
        let components = response["components"].as_array().unwrap();
        let keys: Vec<_> = components
            .iter()
            .map(|component| component["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, ["(hover: hover)", "(width <= 768px)", "screen"]);
        let screen = &components[2];
        assert_eq!(screen["cssPath"], "a.css");
        assert_eq!(screen["order"], 1);
    }

    #[test]
    fn whole_conditions_report_one_unit() {
        let response = collect(json!({
            "stylesheets": [{
                "cssPath": "card.css",
                "cssSource": "@media screen, print { .card { margin: 0; } }\n\
                    @media (min-width: calc(100vw - 2rem)) { .card { color: red; } }",
            }],
            "themeTokens": rem_tokens(),
        }));
        let components = response["components"].as_array().unwrap();
        assert_eq!(components.len(), 2);
        assert_eq!(components[0]["key"], "(width >= calc(100vw - 2rem))");
        assert_eq!(components[0]["whole"], false);
        assert_eq!(components[0]["readableName"], Value::Null);
        assert_eq!(components[1]["key"], "screen, print");
        assert_eq!(components[1]["whole"], true);
        assert_eq!(components[1]["readableName"], "screen-or-print");
    }

    #[test]
    fn collects_conditions_from_vue_style_blocks() {
        let sfc = "<template><div class=\"card\"></div></template>\n\
            <style scoped>@media (min-width: 52rem) { .card { margin: 0; } }</style>";
        let content_start = sfc.find("<style scoped>").unwrap() + "<style scoped>".len();
        let content_end = sfc.find("</style>").unwrap();
        let response = collect(json!({
            "stylesheets": [{
                "cssPath": "Card.vue",
                "cssSource": sfc,
                "vueBlocks": [{ "contentStart": content_start, "contentEnd": content_end }],
            }],
            "themeTokens": rem_tokens(),
        }));
        let components = response["components"].as_array().unwrap();
        assert_eq!(components.len(), 1);
        assert_eq!(components[0]["key"], "(width >= 52rem)");
        assert_eq!(components[0]["cssPath"], "Card.vue");
    }

    #[test]
    fn parses_authored_custom_variant_reservations() {
        let response = collect(json!({
            "stylesheets": [],
            "themeTokens": rem_tokens(),
            "tailwindSources": [{
                "path": "app.css",
                "source": "@custom-variant width-lte-768px {\n  @media (width <= 768px) {\n    @slot;\n  }\n}\n\
                    @custom-variant hocus (&:hover, &:focus);\n\
                    @custom-variant both {\n  @media screen and (width <= 768px) {\n    @slot;\n  }\n}",
            }],
        }));
        let variants = response["authoredVariants"].as_array().unwrap();
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0]["name"], "both");
        assert_eq!(variants[0]["mediaQueryKey"], Value::Null);
        assert_eq!(variants[1]["name"], "hocus");
        assert_eq!(variants[1]["mediaQueryKey"], Value::Null);
        assert_eq!(variants[2]["name"], "width-lte-768px");
        assert_eq!(variants[2]["mediaQueryKey"], "(width <= 768px)");
    }

    #[test]
    fn output_is_deterministic() {
        let request = json!({
            "stylesheets": [{
                "cssPath": "card.css",
                "cssSource": "@media (width <= 900px) { .a { margin: 0; } }\n\
                    @media (width <= 768px) { .b { margin: 0; } }",
            }],
            "themeTokens": rem_tokens(),
        });
        assert_eq!(collect(request.clone()), collect(request));
    }
}

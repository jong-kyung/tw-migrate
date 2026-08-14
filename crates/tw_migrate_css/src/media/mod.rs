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

use std::hash::{DefaultHasher, Hasher};

/// The longest readable generated name; anything longer takes the digest.
const MAX_NAME_LENGTH: usize = 48;

/// One media condition after normalization and decomposition.
pub enum ParsedMediaCondition {
    /// An `and`-joined single branch, split into components in authored
    /// order. Consumers stack the component variants.
    Components(Vec<MediaComponent>),
    /// A comma list, `or`-joined condition, or multi-part negation, kept as
    /// one whole variant.
    Whole(MediaComponent),
}

/// One generated-variant unit: a component of a decomposed condition, or a
/// complete non-decomposable condition.
pub struct MediaComponent {
    /// Canonical condition text; the generated definition wraps
    /// `@media <key>`.
    pub key: String,
    /// Readable variant name, present only when it derives cleanly from
    /// lowercase letters, digits, hyphens, and `p` for decimal points, and
    /// fits the length limit. `None` sends the component to the digest name.
    pub readable_name: Option<String>,
    /// Compact feature text for built-in variant table lookup, such as
    /// `(prefers-color-scheme:dark)` or `print`. Absent for negations and
    /// non-atomic shapes.
    pub builtin_query: Option<String>,
    /// The width bound of a bare width range component, used to match
    /// existing project breakpoints.
    pub width_bound: Option<WidthBound>,
}

/// One provable width bound of a width-only component.
pub struct WidthBound {
    pub value: String,
    pub inclusive: bool,
    /// True for a lower bound (`width >= v`), false for an upper bound.
    pub lower: bool,
}

/// Parse, normalize, and decompose a media query condition (the text after
/// `@media`). Returns `None` when the condition cannot be represented
/// safely: unknown syntax, nested grouping, custom-media references, or
/// characters that a generated definition cannot carry.
pub fn parse_media_condition(query: &str) -> Option<ParsedMediaCondition> {
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
pub fn condition_digest(key: &str) -> String {
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
                    op: *op,
                    value: normalize_value(right),
                    feature: left_ident,
                })
            } else if is_feature_ident(&right_ident) && !trim_css_whitespace(left).is_empty() {
                Some(Condition::Range {
                    op: op.flipped(),
                    value: normalize_value(left),
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
/// when every code point is provably case-insensitive: plain keywords and
/// dimensions. Values carrying functions or other tokens, such as
/// `env(MyInset)`, keep their authored case. No numeric canonicalization is
/// attempted: spellings the parser cannot prove identical, such as `+52rem`
/// against `52rem`, keep distinct keys, and the worst outcome is one
/// duplicate definition per authored spelling.
fn normalize_value(value: &str) -> String {
    let collapsed = collapse_whitespace(value);
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

mod collect;

pub use collect::{collect_media_conditions_json, media_probe_key_json};

#[cfg(test)]
mod tests;

//! Font-family stack parsing for canonicalization probes. The planner
//! hands the orchestration layer structured family data so TypeScript
//! never parses Tailwind candidate syntax or raw CSS value text.

/// One `font-family` declaration that produced an arbitrary candidate,
/// carrying the parsed stack so token registration can reuse and allocate
/// theme tokens without reparsing the candidate.
#[derive(Clone)]
pub struct FontFamilyProbe {
    /// The complete rule-level candidate, the same spelling
    /// `candidateProbes` emits.
    pub candidate: String,
    /// The normalized family stack: names quoted, generic keywords bare,
    /// one comma-space between families.
    pub value: String,
    /// The first family's decoded name.
    pub first_family_name: String,
    /// `name`, `generic`, or `css-wide`.
    pub first_family_kind: &'static str,
}

/// CSS generic family keywords per the CSS Fonts specification; unquoted
/// spellings of these never create font tokens.
const GENERIC_FAMILIES: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "ui-rounded",
    "math",
    "fangsong",
    "emoji",
];

const CSS_WIDE_KEYWORDS: &[&str] = &["initial", "inherit", "unset", "revert", "revert-layer"];

/// The parsed families of one raw `font-family` value: the normalized
/// stack, the first family's decoded name, and its kind. `None` when the
/// value is runtime-dependent or otherwise unreadable (functions,
/// escapes, unterminated quotes, empty families), in which case the rule
/// keeps its arbitrary candidate under the existing safety rules.
pub fn parse_font_stack(value: &str) -> Option<(String, String, &'static str)> {
    let mut families = Vec::new();
    for segment in value.split(',') {
        let segment = segment.trim();
        if segment.is_empty() {
            return None;
        }
        let quote = segment.chars().next().filter(|c| *c == '"' || *c == '\'');
        if let Some(quote) = quote {
            let inner = segment
                .strip_prefix(quote)
                .and_then(|rest| rest.strip_suffix(quote))?;
            if inner.is_empty()
                || inner.contains('\\')
                || inner.contains(quote)
                || inner.contains(['"', '\''])
            {
                return None;
            }
            // A quoted spelling is always a family name, even when it
            // matches a generic keyword.
            families.push((inner.to_string(), "name"));
            continue;
        }
        let tokens: Vec<&str> = segment.split_whitespace().collect();
        if tokens.is_empty()
            || tokens.iter().any(|token| {
                token.chars().any(|character| {
                    !(character.is_ascii_alphanumeric()
                        || character == '-'
                        || character == '_'
                        || !character.is_ascii())
                })
            })
        {
            return None;
        }
        // Generic and CSS-wide keywords match ASCII case-insensitively,
        // and their canonical output folds to lowercase so stack
        // comparison cannot split on authored casing.
        let folded = tokens[0].to_ascii_lowercase();
        let kind = if tokens.len() == 1 && GENERIC_FAMILIES.contains(&folded.as_str()) {
            "generic"
        } else if tokens.len() == 1 && CSS_WIDE_KEYWORDS.contains(&folded.as_str()) {
            "css-wide"
        } else {
            "name"
        };
        let joined = if kind == "name" { tokens.join(" ") } else { folded };
        families.push((joined, kind));
    }
    let (first_name, first_kind) = families.first().cloned()?;
    let normalized = families
        .iter()
        .map(|(name, kind)| {
            if *kind == "name" {
                format!("\"{name}\"")
            } else {
                name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some((normalized, first_name, first_kind))
}

/// JSON view of one parsed stack for the orchestration layer, so
/// existing theme-token values normalize through the same parser as the
/// planner's probes. `null` when the value is unreadable.
pub fn font_family_stack_json(value: &str) -> String {
    match parse_font_stack(value) {
        Some((normalized, name, kind)) => serde_json::json!({
            "value": normalized,
            "firstFamily": { "name": name, "kind": kind },
        })
        .to_string(),
        None => "null".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_font_stack;

    #[test]
    fn parses_and_normalizes_family_stacks() {
        assert_eq!(
            parse_font_stack("\"Open Sans\", sans-serif"),
            Some(("\"Open Sans\", sans-serif".to_string(), "Open Sans".to_string(), "name")),
        );
        assert_eq!(
            parse_font_stack("Acme  Display , serif"),
            Some(("\"Acme Display\", serif".to_string(), "Acme Display".to_string(), "name")),
        );
        // A quoted generic spelling is a family name, not the keyword.
        assert_eq!(
            parse_font_stack("'serif'"),
            Some(("\"serif\"".to_string(), "serif".to_string(), "name")),
        );
        assert_eq!(
            parse_font_stack("sans-serif"),
            Some(("sans-serif".to_string(), "sans-serif".to_string(), "generic")),
        );
        assert_eq!(
            parse_font_stack("inherit"),
            Some(("inherit".to_string(), "inherit".to_string(), "css-wide")),
        );
    }

    #[test]
    fn folds_keyword_casing_before_classifying() {
        assert_eq!(
            parse_font_stack("Serif"),
            Some(("serif".to_string(), "serif".to_string(), "generic")),
        );
        assert_eq!(
            parse_font_stack("INHERIT"),
            Some(("inherit".to_string(), "inherit".to_string(), "css-wide")),
        );
    }

    #[test]
    fn rejects_runtime_dependent_and_unreadable_values() {
        assert_eq!(parse_font_stack("var(--font-body)"), None);
        assert_eq!(parse_font_stack("env(brand)"), None);
        assert_eq!(parse_font_stack("\"Open"), None);
        assert_eq!(parse_font_stack("\"Open \\\"Sans\\\"\""), None);
        assert_eq!(parse_font_stack(""), None);
        assert_eq!(parse_font_stack("Brand,,serif"), None);
    }
}

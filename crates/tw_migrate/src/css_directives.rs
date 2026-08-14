//! Tailwind entry directives of a stylesheet: top-level `@import` targets
//! with their `source(...)` modifiers and `@source` scopes, read from
//! parser-proven prelude structure. Tailwind honors these directives only
//! at the top level, so directive-shaped text inside rule blocks or string
//! values never counts.

use serde::Serialize;
use tw_migrate_error::{MigrationError, MigrationResult};

use crate::at_rules::parse_css;
use crate::stylesheet_analysis::{import_href, literal_str};
use oxc_css_parser::ast::{
    AtRulePrelude, ComponentValue, Function, FunctionName, InterpolableIdent, MediaQuery, Statement,
};
use oxc_css_parser::token;

#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
enum CssDirective {
    /// A top-level `@import`, read from the parser's structured prelude.
    Import {
        /// The import target, or None when it cannot be read statically.
        specifier: Option<String>,
        /// True when the target is the tailwindcss package or a subpath.
        tailwind: bool,
        /// The `source(...)` modifier: "none", a literal path, or None
        /// when absent.
        source: Option<String>,
        /// True when a `source(...)` modifier exists but cannot be read.
        source_unreadable: bool,
    },
    /// A top-level `@source` directive, using Tailwind's own grammar over
    /// the parser-proven prelude text.
    Source {
        not: bool,
        inline: bool,
        scope: Option<String>,
        /// True when the prelude does not match the supported grammar.
        unreadable: bool,
    },
    Other {
        name: String,
    },
}

fn import_directive(source_text: &str, at_rule: &oxc_css_parser::ast::AtRule<'_>) -> CssDirective {
    let Some(AtRulePrelude::Import(prelude)) = &at_rule.prelude else {
        return CssDirective::Import {
            specifier: None,
            tailwind: false,
            source: None,
            source_unreadable: false,
        };
    };
    let specifier = import_href(&prelude.href);
    let tailwind = specifier
        .as_deref()
        .is_some_and(|spec| spec == "tailwindcss" || spec.starts_with("tailwindcss/"));
    let mut source = None;
    let mut source_unreadable = false;
    // A lone `source(...)` parses into the media-query list as a structured
    // function; mixed with other modifiers such as `prefix(...)` the whole
    // tail becomes a raw token sequence in `modifiers`. Both positions are
    // read.
    if let Some(media) = &prelude.media {
        for query in &media.queries {
            if let MediaQuery::Function(function) = query {
                read_source_function(function, &mut source, &mut source_unreadable);
            }
        }
    }
    if let Some(modifiers) = &prelude.modifiers {
        let mut structured = false;
        for value in &modifiers.values {
            if let ComponentValue::Function(function) = value {
                structured = true;
                read_source_function(function, &mut source, &mut source_unreadable);
            }
        }
        if !structured {
            read_source_tokens(
                source_text,
                &modifiers.values,
                &mut source,
                &mut source_unreadable,
            );
        }
    }
    CssDirective::Import {
        specifier,
        tailwind,
        source,
        source_unreadable,
    }
}

fn read_source_function(
    function: &Function<'_>,
    source: &mut Option<String>,
    source_unreadable: &mut bool,
) {
    let FunctionName::Ident(InterpolableIdent::Literal(name)) = &function.name else {
        return;
    };
    if name.name != "source" {
        return;
    }
    match function.args.as_slice() {
        [ComponentValue::InterpolableIdent(InterpolableIdent::Literal(ident))]
            if ident.name == "none" =>
        {
            *source = Some("none".to_string());
        }
        [ComponentValue::InterpolableStr(path)] => match literal_str(path) {
            Some(path) => *source = Some(path),
            None => *source_unreadable = true,
        },
        _ => *source_unreadable = true,
    }
}

/// Read `source(...)` from a raw modifier token sequence: an ident named
/// `source`, a parenthesis, then either the `none` ident or a quoted path.
fn read_source_tokens(
    source_text: &str,
    values: &[ComponentValue<'_>],
    source: &mut Option<String>,
    source_unreadable: &mut bool,
) {
    let tokens: Vec<(&token::TokenData, &str)> = values
        .iter()
        .filter_map(|value| match value {
            ComponentValue::TokenWithSpan(with_span) => Some((
                &with_span.token,
                &source_text[with_span.span.start..with_span.span.end],
            )),
            _ => None,
        })
        .collect();
    let mut index = 0;
    while index < tokens.len() {
        let (data, text) = tokens[index];
        let is_source = matches!(data, token::TokenData::Ident(_)) && text == "source";
        if !is_source {
            index += 1;
            continue;
        }
        let wrapped = matches!(
            tokens.get(index + 1),
            Some((token::TokenData::LParen(_), _))
        ) && matches!(
            tokens.get(index + 3),
            Some((token::TokenData::RParen(_), _))
        );
        match tokens.get(index + 2) {
            Some((token::TokenData::Ident(_), text)) if wrapped && *text == "none" => {
                *source = Some("none".to_string());
            }
            Some((token::TokenData::Str(_), text)) if wrapped => {
                *source = Some(text.trim_matches(['"', '\'']).to_string());
            }
            _ => *source_unreadable = true,
        }
        index += 1;
    }
}

/// Parse a `@source` prelude with Tailwind's grammar: an optional `not`,
/// then a quoted scope or an `inline(...)` list.
fn source_directive(prelude: &str) -> CssDirective {
    let mut rest = prelude.trim();
    let mut not = false;
    if let Some(stripped) = rest.strip_prefix("not") {
        if stripped.starts_with(char::is_whitespace) {
            not = true;
            rest = stripped.trim_start();
        }
    }
    if rest.starts_with("inline(") {
        return CssDirective::Source {
            not,
            inline: true,
            scope: None,
            unreadable: false,
        };
    }
    let scope = rest
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            rest.strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        });
    match scope {
        Some(scope) if !scope.is_empty() => CssDirective::Source {
            not,
            inline: false,
            scope: Some(scope.to_string()),
            unreadable: false,
        },
        _ => CssDirective::Source {
            not,
            inline: false,
            scope: None,
            unreadable: !rest.is_empty(),
        },
    }
}

/// Top-level at-rule directives of a stylesheet.
pub fn collect_css_directives_json(source: &str) -> MigrationResult<String> {
    let allocator = oxc_css_parser::Allocator::default();
    let parsed = parse_css(&allocator, source, oxc_css_parser::Syntax::Css).map_err(|error| {
        MigrationError::AuthoredStylesheetParse {
            message: format!("Failed to parse stylesheet: {error}"),
        }
    })?;
    let mut directives = Vec::new();
    for statement in &parsed.statements {
        let Statement::AtRule(at_rule) = statement else {
            continue;
        };
        match at_rule.name.name {
            "import" => directives.push(import_directive(source, at_rule)),
            "source" => {
                let prelude_end = at_rule
                    .block
                    .as_ref()
                    .map_or(at_rule.span.end, |block| block.span.start);
                let text = &source[at_rule.span.start..prelude_end];
                let prelude = text.trim_start().trim_start_matches("@source");
                directives.push(source_directive(prelude));
            }
            name => directives.push(CssDirective::Other {
                name: name.to_string(),
            }),
        }
    }
    serde_json::to_string(&directives).map_err(|error| MigrationError::Serialization {
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn collects_top_level_css_directives_only() {
        let parsed: Vec<serde_json::Value> = serde_json::from_str(
            &super::collect_css_directives_json(
                "@import \"tailwindcss\" source(none);\n\
                 @import \"tailwindcss/utilities\" prefix(tw) source(\"./apps\");\n\
                 @import url(./theme.css);\n\
                 @source not \"./packages/app\";\n\
                 @source inline(\"width-lte-700px:hidden\");\n\
                 .rule { content: '@source \"./inside-string\"'; }\n\
                 @media screen { .x { color: red; } }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed,
            serde_json::json!([
                {
                    "kind": "import",
                    "specifier": "tailwindcss",
                    "tailwind": true,
                    "source": "none",
                    "sourceUnreadable": false
                },
                {
                    "kind": "import",
                    "specifier": "tailwindcss/utilities",
                    "tailwind": true,
                    "source": "./apps",
                    "sourceUnreadable": false
                },
                {
                    "kind": "import",
                    "specifier": "./theme.css",
                    "tailwind": false,
                    "source": null,
                    "sourceUnreadable": false
                },
                { "kind": "source", "not": true, "inline": false, "scope": "./packages/app", "unreadable": false },
                { "kind": "source", "not": false, "inline": true, "scope": null, "unreadable": false },
                { "kind": "other", "name": "media" }
            ])
            .as_array()
            .unwrap()
            .clone()
        );
    }
}

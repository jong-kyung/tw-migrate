pub(crate) fn encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    let mut whitespace = false;

    for character in value.chars() {
        if character.is_whitespace() {
            whitespace = !encoded.is_empty();
            continue;
        }
        if whitespace {
            encoded.push('_');
            whitespace = false;
        }
        if character == '_' {
            encoded.push_str("\\_");
        } else {
            encoded.push(character);
        }
    }

    encoded
}

/// Encodes a CSS declaration value as a Tailwind arbitrary value, or returns
/// `None` when the value cannot round-trip through the Tailwind decoder.
///
/// Rules verified against the project Tailwind compiler (`candidatesToCss`):
/// - Outside `url()`, whitespace between tokens becomes `_` and literal `_`
///   becomes `\_`; this applies inside quoted strings too, where Tailwind
///   also decodes `_` to a space and `\_` to an underscore.
/// - Tailwind emits `url()` bodies verbatim (no underscore decoding), so the
///   body is copied through unchanged and whitespace there is
///   unrepresentable. Tailwind applies the same treatment to any function
///   whose name ends in `_url`.
/// - Unrepresentable values: `;` outside quotes, unterminated strings,
///   unbalanced parentheses or brackets, whitespace inside `url()`, and
///   non-space whitespace or escaped whitespace inside quoted strings (a
///   class attribute cannot carry a literal space).
pub(crate) fn encode_value(value: &str) -> Option<String> {
    let mut encoded = String::with_capacity(value.len());
    let mut chars = value.char_indices();
    let mut pending_space = false;
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut ident_start: Option<usize> = None;

    while let Some((index, character)) = chars.next() {
        if character.is_whitespace() {
            pending_space = !encoded.is_empty();
            ident_start = None;
            continue;
        }
        if pending_space {
            encoded.push('_');
            pending_space = false;
        }
        match character {
            ';' => return None,
            '"' | '\'' => {
                encoded.push(character);
                encode_quoted(&mut chars, character, &mut encoded)?;
                ident_start = None;
            }
            '(' => {
                let ident = ident_start.map_or("", |start| &value[start..index]);
                encoded.push('(');
                if ident == "url" || ident.ends_with("_url") {
                    encode_url_body(&mut chars, &mut encoded)?;
                } else {
                    parens += 1;
                }
                ident_start = None;
            }
            ')' => {
                parens = parens.checked_sub(1)?;
                encoded.push(')');
                ident_start = None;
            }
            '[' => {
                brackets += 1;
                encoded.push('[');
                ident_start = None;
            }
            ']' => {
                brackets = brackets.checked_sub(1)?;
                encoded.push(']');
                ident_start = None;
            }
            '\\' => {
                let (_, escaped) = chars.next()?;
                if escaped.is_whitespace() {
                    return None;
                }
                encoded.push('\\');
                encoded.push(escaped);
                ident_start = None;
            }
            '_' => {
                encoded.push_str("\\_");
                ident_start.get_or_insert(index);
            }
            character if character.is_alphanumeric() || character == '-' => {
                encoded.push(character);
                ident_start.get_or_insert(index);
            }
            character => {
                encoded.push(character);
                ident_start = None;
            }
        }
    }
    (parens == 0 && brackets == 0).then_some(encoded)
}

fn encode_quoted(
    chars: &mut impl Iterator<Item = (usize, char)>,
    quote: char,
    encoded: &mut String,
) -> Option<()> {
    loop {
        let (_, character) = chars.next()?;
        if character == quote {
            encoded.push(character);
            return Some(());
        }
        match character {
            '\\' => {
                let (_, escaped) = chars.next()?;
                if escaped.is_whitespace() {
                    return None;
                }
                encoded.push('\\');
                encoded.push(escaped);
            }
            '_' => encoded.push_str("\\_"),
            ' ' => encoded.push('_'),
            character if character.is_whitespace() => return None,
            character => encoded.push(character),
        }
    }
}

fn encode_url_body(
    chars: &mut impl Iterator<Item = (usize, char)>,
    encoded: &mut String,
) -> Option<()> {
    let mut quote: Option<char> = None;
    loop {
        let (_, character) = chars.next()?;
        if character.is_whitespace() {
            return None;
        }
        match quote {
            Some(closing) => {
                if character == '\\' {
                    let (_, escaped) = chars.next()?;
                    if escaped.is_whitespace() {
                        return None;
                    }
                    encoded.push('\\');
                    encoded.push(escaped);
                    continue;
                }
                encoded.push(character);
                if character == closing {
                    quote = None;
                }
            }
            None => match character {
                ')' => {
                    encoded.push(')');
                    return Some(());
                }
                '"' | '\'' => {
                    quote = Some(character);
                    encoded.push(character);
                }
                '(' | '[' | ']' | ';' => return None,
                character => encoded.push(character),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{encode, encode_value};

    #[test]
    fn distinguishes_spaces_from_literal_underscores() {
        assert_eq!(encode("Open Sans"), "Open_Sans");
        assert_eq!(encode("Open_Sans"), "Open\\_Sans");
        assert_eq!(
            encode(" var(--font_key, Open Sans) "),
            "var(--font\\_key,_Open_Sans)"
        );
    }

    #[test]
    fn encodes_quoted_strings() {
        assert_eq!(
            encode_value("\"My Font\", sans-serif").as_deref(),
            Some("\"My_Font\",_sans-serif")
        );
        assert_eq!(encode_value("\"a_b\"").as_deref(), Some("\"a\\_b\""));
        assert_eq!(encode_value("'a b'").as_deref(), Some("'a_b'"));
        assert_eq!(encode_value("\"a;b\"").as_deref(), Some("\"a;b\""));
        assert_eq!(
            encode_value("\"say \\\"hi\\\"\"").as_deref(),
            Some("\"say_\\\"hi\\\"\"")
        );
    }

    #[test]
    fn preserves_url_bodies_verbatim() {
        assert_eq!(
            encode_value("url(\"a_b.png\")").as_deref(),
            Some("url(\"a_b.png\")")
        );
        assert_eq!(
            encode_value("url(a_b.png) no-repeat").as_deref(),
            Some("url(a_b.png)_no-repeat")
        );
        assert_eq!(
            encode_value("image_url(a_b.png)").as_deref(),
            Some("image\\_url(a_b.png)")
        );
    }

    #[test]
    fn encodes_nested_functions_and_bracketed_line_names() {
        assert_eq!(
            encode_value("calc(min(100%, 50vw))").as_deref(),
            Some("calc(min(100%,_50vw))")
        );
        assert_eq!(
            encode_value("[full-start] 1fr").as_deref(),
            Some("[full-start]_1fr")
        );
        assert_eq!(
            encode_value(" var(--font_key, Open Sans) ").as_deref(),
            Some("var(--font\\_key,_Open_Sans)")
        );
    }

    #[test]
    fn rejects_unrepresentable_values() {
        // Spaces inside url() cannot be represented: Tailwind keeps url()
        // bodies verbatim and a class attribute cannot hold a literal space.
        assert_eq!(encode_value("url(\"a b.png\")"), None);
        assert_eq!(encode_value("url( a.png )"), None);
        assert_eq!(encode_value("\"unterminated"), None);
        assert_eq!(encode_value("100px; color: red"), None);
        assert_eq!(encode_value("calc(100% - (1rem)"), None);
        assert_eq!(encode_value("calc(100%))"), None);
        assert_eq!(encode_value("[full-start 1fr"), None);
        assert_eq!(encode_value("url(a(b.png)"), None);
    }
}

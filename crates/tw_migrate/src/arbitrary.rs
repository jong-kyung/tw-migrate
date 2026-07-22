/// CSS whitespace per CSS Syntax. Other Unicode whitespace (NBSP, em space,
/// ...) are ident code points and must be preserved verbatim: Tailwind passes
/// them through candidates unchanged.
fn is_css_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r' | '\x0C')
}

pub(crate) fn encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    let mut whitespace = false;

    for character in value.chars() {
        if is_css_whitespace(character) {
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
/// - Only CSS whitespace (space, tab, newline, carriage return, form feed)
///   counts as whitespace. Other Unicode whitespace such as NBSP is a CSS
///   ident code point: it is copied verbatim (Tailwind emits it unchanged)
///   and it keeps the surrounding ident running, so `A\u{a0}url(...)` is not
///   a `url()` function - neither here nor in Tailwind's decoder.
/// - The first CSS whitespace character after a hex escape (`\41 `) is the
///   escape terminator, consumed by the escape per CSS syntax. It is part of
///   the token and always becomes its own `_` (Tailwind decodes each `_` to
///   exactly one space), so `\41  B` ("A B") encodes as `\41__B` while
///   `\41 B` ("AB") encodes as `\41_B`.
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
    let mut hex_digits = 0u8;

    while let Some((index, character)) = chars.next() {
        if is_css_whitespace(character) {
            if hex_digits > 0 {
                // The escape terminator: consumed by the hex escape, so it is
                // part of the token and must round-trip as its own space.
                encoded.push('_');
            } else {
                pending_space = !encoded.is_empty();
            }
            hex_digits = 0;
            ident_start = None;
            continue;
        }
        if pending_space {
            encoded.push('_');
            pending_space = false;
        }
        let hex_run = hex_digits;
        hex_digits = 0;
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
                if push_escape(&mut chars, &mut encoded)? {
                    hex_digits = 1;
                }
                ident_start = None;
            }
            '_' => {
                encoded.push_str("\\_");
                ident_start.get_or_insert(index);
            }
            character if character.is_alphanumeric() || character == '-' => {
                encoded.push(character);
                ident_start.get_or_insert(index);
                if hex_run > 0 && hex_run < 6 && character.is_ascii_hexdigit() {
                    hex_digits = hex_run + 1;
                }
            }
            character => {
                encoded.push(character);
                if character.is_ascii() {
                    ident_start = None;
                } else {
                    // Non-ASCII code points (e.g. NBSP) are CSS ident code
                    // points and keep the current ident running.
                    ident_start.get_or_insert(index);
                }
            }
        }
    }
    (parens == 0 && brackets == 0).then_some(encoded)
}

/// Copies a backslash escape and reports whether it starts a hex escape,
/// whose trailing whitespace (if any) is the escape terminator.
fn push_escape(
    chars: &mut impl Iterator<Item = (usize, char)>,
    encoded: &mut String,
) -> Option<bool> {
    let (_, escaped) = chars.next()?;
    if is_css_whitespace(escaped) {
        return None;
    }
    encoded.push('\\');
    encoded.push(escaped);
    Some(escaped.is_ascii_hexdigit())
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
                push_escape(chars, encoded)?;
            }
            '_' => encoded.push_str("\\_"),
            ' ' => encoded.push('_'),
            character if is_css_whitespace(character) => return None,
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
        if is_css_whitespace(character) {
            return None;
        }
        match quote {
            Some(closing) => {
                if character == '\\' {
                    push_escape(chars, encoded)?;
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
    fn preserves_hex_escape_terminators() {
        // `\41  B` is the ident sequence "A B": the first space is consumed
        // by the escape as its terminator, so it must round-trip as its own
        // underscore (Tailwind decodes each `_` to one space).
        assert_eq!(encode_value("\\41  B").as_deref(), Some("\\41__B"));
        assert_eq!(encode_value("\\41 B").as_deref(), Some("\\41_B"));
        assert_eq!(encode_value("\\4B  C").as_deref(), Some("\\4B__C"));
        assert_eq!(encode_value("\\41 ").as_deref(), Some("\\41_"));
        // A hex escape consumes at most six digits: the seventh character is
        // an ordinary ident character and the space a plain separator.
        assert_eq!(encode_value("\\0000411 B").as_deref(), Some("\\0000411_B"));
    }

    #[test]
    fn treats_non_css_whitespace_as_ident_code_points() {
        // NBSP is a CSS ident code point, not whitespace; Tailwind passes it
        // through candidates verbatim.
        assert_eq!(encode_value("A\u{a0}B").as_deref(), Some("A\u{a0}B"));
        assert_eq!(encode_value("\"A\u{a0}B\"").as_deref(), Some("\"A\u{a0}B\""));
        // NBSP glues to a following ident, so this is not a `url()` function
        // and Tailwind decodes underscores inside its parentheses.
        assert_eq!(
            encode_value("A\u{a0}url(x y)").as_deref(),
            Some("A\u{a0}url(x_y)")
        );
        assert_eq!(encode("A\u{a0}B"), "A\u{a0}B");
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

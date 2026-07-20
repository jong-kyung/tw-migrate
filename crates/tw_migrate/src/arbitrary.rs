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

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn distinguishes_spaces_from_literal_underscores() {
        assert_eq!(encode("Open Sans"), "Open_Sans");
        assert_eq!(encode("Open_Sans"), "Open\\_Sans");
        assert_eq!(
            encode(" var(--font_key, Open Sans) "),
            "var(--font\\_key,_Open_Sans)"
        );
    }
}

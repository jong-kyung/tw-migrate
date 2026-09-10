use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DecodedSourceMapping {
    generated_line: u32,
    generated_column: u32,
    source: String,
    original_line: u32,
    original_column: u32,
}

pub fn decode_source_map_json(source_map: &str) -> MigrationResult<String> {
    let source_map = oxc_sourcemap::SourceMap::from_json_string(source_map).map_err(|error| {
        MigrationError::SourceMap {
            message: format!("Failed to decode source map: {error}"),
        }
    })?;
    let mappings = source_map
        .get_tokens()
        .filter_map(|token| {
            let source = source_map.get_source(token.get_source_id()?)?;
            Some(DecodedSourceMapping {
                generated_line: token.get_dst_line(),
                generated_column: token.get_dst_col(),
                source: source.to_owned(),
                original_line: token.get_src_line(),
                original_column: token.get_src_col(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&mappings).map_err(|error| MigrationError::Serialization {
        message: error.to_string(),
    })
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SourceMapping {
    pub(super) generated_line: usize,
    pub(super) generated_column: usize,
    pub(super) source_path: String,
    pub(super) original_line: usize,
    pub(super) original_column: usize,
}

pub(super) fn map_rule_spans(
    authored_source: &str,
    syntax: StylesheetSyntax,
    source_path: &str,
    source_mappings: &[SourceMapping],
    analysis_source: &str,
    rules: &mut [RulePlan],
    authored_base: usize,
) -> MigrationResult<()> {
    let allocator = oxc_css_parser::Allocator::default();
    let stylesheet =
        parse_css(&allocator, authored_source, syntax.parser_syntax()).map_err(|error| {
            MigrationError::AuthoredStylesheetParse {
                message: format!("Failed to parse {source_path}: {error}"),
            }
        })?;
    let mut authored_rules = Vec::new();
    collect_qualified_rule_spans(&stylesheet.statements, &mut authored_rules);
    let mappings = source_mappings
        .iter()
        .map(|mapping| ((mapping.generated_line, mapping.generated_column), mapping))
        .collect::<HashMap<_, _>>();

    let analysis_lines = line_starts(analysis_source);
    let authored_lines = line_starts(authored_source);
    for rule in rules.iter_mut() {
        let mut original_offsets = Vec::new();
        for generated_offset in &rule.provenance_offsets {
            let Some(position) =
                offset_to_line_column(analysis_source, &analysis_lines, *generated_offset)
            else {
                original_offsets.clear();
                break;
            };
            let Some(mapping) = mappings.get(&position) else {
                original_offsets.clear();
                break;
            };
            if mapping.source_path != source_path {
                original_offsets.clear();
                break;
            }
            let Some(offset) = line_column_to_offset(
                authored_source,
                &authored_lines,
                mapping.original_line,
                mapping.original_column,
            ) else {
                original_offsets.clear();
                break;
            };
            original_offsets.push(offset);
        }
        if original_offsets.is_empty() {
            continue;
        }
        rule.authored_span = authored_rules
            .iter()
            .filter(|(span, _)| {
                original_offsets
                    .iter()
                    .all(|offset| span.start <= *offset && *offset < span.end)
            })
            .min_by_key(|(span, _)| span.end - span.start)
            .map(|(span, _)| authored_base + span.start..authored_base + span.end);
    }

    let mut shared_spans: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (index, rule) in rules.iter().enumerate() {
        if let Some(span) = &rule.authored_span {
            shared_spans
                .entry((span.start, span.end))
                .or_default()
                .push(index);
        }
    }
    let mut ambiguous = BTreeSet::new();
    for indices in shared_spans.values().filter(|indices| indices.len() > 1) {
        ambiguous.extend(indices.iter().copied());
    }
    for left in 0..rules.len() {
        let Some(left_span) = &rules[left].authored_span else {
            continue;
        };
        for (right, right_rule) in rules.iter().enumerate().skip(left + 1) {
            let Some(right_span) = &right_rule.authored_span else {
                continue;
            };
            if left_span.start < right_span.end && right_span.start < left_span.end {
                ambiguous.extend([left, right]);
            }
        }
    }
    for index in ambiguous {
        rules[index].authored_span = None;
    }
    let interpolation = match syntax {
        StylesheetSyntax::Scss | StylesheetSyntax::Sass => Some("#{"),
        StylesheetSyntax::Less => Some("@{"),
        StylesheetSyntax::Css => None,
    };
    if let Some(interpolation) = interpolation {
        for rule in rules {
            let interpolated = rule.authored_span.as_ref().is_some_and(|authored_span| {
                authored_rules.iter().any(|(span, selector_span)| {
                    authored_span.start == authored_base + span.start
                        && authored_span.end == authored_base + span.end
                        && authored_source[selector_span.clone()].contains(interpolation)
                })
            });
            if interpolated {
                rule.authored_span = None;
            }
        }
    }
    Ok(())
}

fn collect_qualified_rule_spans(
    statements: &[Statement<'_>],
    spans: &mut Vec<(std::ops::Range<usize>, std::ops::Range<usize>)>,
) {
    for statement in statements {
        match statement {
            Statement::QualifiedRule(rule) => {
                spans.push((
                    rule.span.start..rule.span.end,
                    rule.selector.span.start..rule.selector.span.end,
                ));
                collect_qualified_rule_spans(&rule.block.statements, spans);
            }
            Statement::AtRule(at_rule) => {
                if let Some(block) = &at_rule.block {
                    collect_qualified_rule_spans(&block.statements, spans);
                }
            }
            _ => {}
        }
    }
}

// ponytail: columns still scan within one line; add per-line checkpoints only
// if very long single-line stylesheets become a measured bottleneck.
fn line_starts(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(offset, _)| offset + 1))
        .collect()
}

fn offset_to_line_column(source: &str, lines: &[usize], offset: usize) -> Option<(usize, usize)> {
    if offset > source.len() || !source.is_char_boundary(offset) {
        return None;
    }
    let line = lines.partition_point(|start| *start <= offset) - 1;
    let column = source[lines[line]..offset]
        .chars()
        .map(char::len_utf16)
        .sum();
    Some((line, column))
}

fn line_column_to_offset(
    source: &str,
    lines: &[usize],
    target_line: usize,
    target_column: usize,
) -> Option<usize> {
    let start = *lines.get(target_line)?;
    // Exclude LF, but keep CR as a UTF-16 column just like the source map.
    let end = lines
        .get(target_line + 1)
        .map_or(source.len(), |next| next - 1);
    let mut column = 0;
    for (offset, character) in source[start..end].char_indices() {
        if column == target_column {
            return Some(start + offset);
        }
        column += character.len_utf16();
        if column > target_column {
            return None;
        }
    }
    (column == target_column).then_some(end)
}

pub(super) fn mentions_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let bytes = text.as_bytes();
    text.match_indices(word).any(|(start, _)| {
        let end = start + word.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
        before_ok && after_ok
    })
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

#[cfg(test)]
mod tests {
    #[test]
    fn source_positions_preserve_character_and_line_boundaries() {
        use super::{line_column_to_offset, line_starts, offset_to_line_column};

        let empty = line_starts("");
        assert_eq!(offset_to_line_column("", &empty, 0), Some((0, 0)));
        assert_eq!(line_column_to_offset("", &empty, 0, 0), Some(0));

        let source = "A한😀\r\nB\n";
        let lines = line_starts(source);
        for (offset, line, column) in [
            (4, 0, 2),
            (8, 0, 4),
            (9, 0, 5),
            (10, 1, 0),
            (11, 1, 1),
            (12, 2, 0),
        ] {
            assert_eq!(
                offset_to_line_column(source, &lines, offset),
                Some((line, column))
            );
            assert_eq!(
                line_column_to_offset(source, &lines, line, column),
                Some(offset)
            );
        }
        for offset in [2, 5, usize::MAX] {
            assert_eq!(offset_to_line_column(source, &lines, offset), None);
        }
        for (line, column) in [(0, 3), (0, 6), (2, 1), (usize::MAX, 0)] {
            assert_eq!(line_column_to_offset(source, &lines, line, column), None);
        }
    }

    #[test]
    fn decodes_source_map_mappings() {
        let decoded = super::decode_source_map_json(
            r#"{"version":3,"sources":["input.scss"],"names":[],"mappings":"AAAA"}"#,
        )
        .unwrap();
        assert_eq!(
            decoded,
            r#"[{"generatedLine":0,"generatedColumn":0,"source":"input.scss","originalLine":0,"originalColumn":0}]"#
        );
    }
}

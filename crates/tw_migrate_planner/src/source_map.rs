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
    collect_qualified_rule_spans(&stylesheet.statements, &mut authored_rules, None);
    let mappings = source_mappings
        .iter()
        .map(|mapping| ((mapping.generated_line, mapping.generated_column), mapping))
        .collect::<HashMap<_, _>>();

    let analysis_lines = line_starts(analysis_source);
    let authored_lines = line_starts(authored_source);
    for rule in rules.iter_mut() {
        let mut original_bounds: Option<(usize, usize)> = None;
        for generated_offset in &rule.provenance_offsets {
            let Some(position) =
                offset_to_line_column(analysis_source, &analysis_lines, *generated_offset)
            else {
                original_bounds = None;
                break;
            };
            let Some(mapping) = mappings.get(&position) else {
                original_bounds = None;
                break;
            };
            if mapping.source_path != source_path {
                original_bounds = None;
                break;
            }
            let Some(offset) = line_column_to_offset(
                authored_source,
                &authored_lines,
                mapping.original_line,
                mapping.original_column,
            ) else {
                original_bounds = None;
                break;
            };
            original_bounds = Some(match original_bounds {
                Some((first, last)) => (first.min(offset), last.max(offset)),
                None => (offset, offset),
            });
        }
        let Some((first, last)) = original_bounds else {
            continue;
        };
        // AST spans are nested or disjoint and collected in source order.
        let mut candidate = authored_rules
            .partition_point(|rule| rule.span.start <= first)
            .checked_sub(1);
        rule.authored_span = None;
        while let Some(index) = candidate {
            let authored = &authored_rules[index];
            if last < authored.span.end {
                rule.authored_span =
                    Some(authored_base + authored.span.start..authored_base + authored.span.end);
                break;
            }
            candidate = authored.parent;
        }
    }

    let mut ordered = rules
        .iter()
        .enumerate()
        .filter_map(|(index, rule)| rule.authored_span.clone().map(|span| (index, span)))
        .collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|(_, span)| (span.start, span.end));
    let mut ordered = ordered.into_iter();
    if let Some((mut furthest_index, mut furthest_span)) = ordered.next() {
        for (index, span) in ordered {
            if span.start < furthest_span.end || span == furthest_span {
                rules[index].authored_span = None;
                rules[furthest_index].authored_span = None;
            }
            // Keep the original end even after marking its rule ambiguous.
            if span.end >= furthest_span.end {
                furthest_index = index;
                furthest_span = span;
            }
        }
    }
    let interpolation = match syntax {
        StylesheetSyntax::Scss | StylesheetSyntax::Sass => Some("#{"),
        StylesheetSyntax::Less => Some("@{"),
        StylesheetSyntax::Css => None,
    };
    if let Some(interpolation) = interpolation {
        let interpolated = authored_rules
            .iter()
            .filter(|rule| authored_source[rule.selector_span.clone()].contains(interpolation))
            .map(|rule| authored_base + rule.span.start..authored_base + rule.span.end)
            .collect::<HashSet<_>>();
        for rule in rules {
            if rule
                .authored_span
                .as_ref()
                .is_some_and(|span| interpolated.contains(span))
            {
                rule.authored_span = None;
            }
        }
    }
    Ok(())
}

struct AuthoredRuleSpan {
    span: std::ops::Range<usize>,
    selector_span: std::ops::Range<usize>,
    parent: Option<usize>,
}

fn collect_qualified_rule_spans(
    statements: &[Statement<'_>],
    spans: &mut Vec<AuthoredRuleSpan>,
    parent: Option<usize>,
) {
    for statement in statements {
        match statement {
            Statement::QualifiedRule(rule) => {
                let index = spans.len();
                spans.push(AuthoredRuleSpan {
                    span: rule.span.start..rule.span.end,
                    selector_span: rule.selector.span.start..rule.selector.span.end,
                    parent,
                });
                collect_qualified_rule_spans(&rule.block.statements, spans, Some(index));
            }
            Statement::AtRule(at_rule) => {
                if let Some(block) = &at_rule.block {
                    collect_qualified_rule_spans(&block.statements, spans, parent);
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
    use super::*;

    fn mapped_spans(
        source: &str,
        syntax: StylesheetSyntax,
        offsets: &[Vec<usize>],
        base: usize,
    ) -> Vec<Option<std::ops::Range<usize>>> {
        let analysis = (0..offsets.len())
            .map(|i| format!(".g{i} {{ padding: 13px; }}\n"))
            .collect::<String>();
        let mut rules = parse_css_rules(
            "input.css",
            "input.css",
            &analysis,
            &HashMap::new(),
            None,
            ParseOptions {
                syntax: Syntax::Css,
                is_module: true,
                can_move_at_rules: false,
                can_move_global_at_rules: false,
                relative_urls_stable: false,
            },
        )
        .unwrap()
        .rules;
        let authored_lines = line_starts(source);
        let analysis_lines = line_starts(&analysis);
        let mut mappings = Vec::new();
        for (line, originals) in offsets.iter().enumerate() {
            rules[line].provenance_offsets = (0..originals.len())
                .map(|column| analysis_lines[line] + column)
                .collect();
            for (column, &offset) in originals.iter().enumerate() {
                let (original_line, original_column) =
                    offset_to_line_column(source, &authored_lines, offset).unwrap();
                mappings.push(SourceMapping {
                    generated_line: line,
                    generated_column: column,
                    source_path: "input.scss".to_string(),
                    original_line,
                    original_column,
                });
            }
        }
        map_rule_spans(
            source,
            syntax,
            "input.scss",
            &mappings,
            &analysis,
            &mut rules,
            base,
        )
        .unwrap();
        rules.into_iter().map(|rule| rule.authored_span).collect()
    }

    #[test]
    fn mapped_spans_match_conservative_scan() {
        for (syntax, selector, outer_interpolated) in [
            (StylesheetSyntax::Css, ".outer", false),
            (StylesheetSyntax::Scss, ".outer", false),
            (StylesheetSyntax::Scss, ".outer-#{$name}", true),
            (StylesheetSyntax::Less, ".outer-@{name}", true),
        ] {
            let leaf = ".leaf { padding: 13px; }";
            let inner = format!(".inner {{ {leaf} color: red; }}");
            let sibling = ".sibling { display: grid; }";
            let outer =
                format!("{selector} {{ @media print {{ {inner} }} {sibling} margin: 7px; }}");
            let after = ".after { display: flex; }";
            let source = format!("/* 한😀 */\r\n{outer}{after}");
            let spans = [&outer, &inner, leaf, after, sibling].map(|text| {
                let start = source.find(text).unwrap();
                start..start + text.len()
            });
            let points = [
                vec![],
                vec![0],
                vec![spans[0].start],
                vec![spans[1].start],
                vec![spans[2].start],
                vec![spans[1].end],
                vec![spans[3].start],
                vec![spans[4].start],
                vec![source.len()],
                vec![spans[0].end - 1, spans[2].start],
                vec![spans[1].start, spans[3].start],
            ];
            for left in &points {
                for right in &points {
                    for offsets in [
                        vec![left.clone()],
                        vec![left.clone(), right.clone()],
                        vec![left.clone(), right.clone(), vec![spans[2].start]],
                    ] {
                        let matched = offsets
                            .iter()
                            .map(|offsets| {
                                spans
                                    .iter()
                                    .filter(|span| {
                                        !offsets.is_empty()
                                            && offsets.iter().all(|offset| span.contains(offset))
                                    })
                                    .min_by_key(|span| span.len())
                                    .cloned()
                            })
                            .collect::<Vec<_>>();
                        for base in [0, 37] {
                            let expected = matched
                                .iter()
                                .enumerate()
                                .map(|(i, span)| {
                                    let span = span.as_ref()?;
                                    let overlaps = matched.iter().enumerate().any(|(j, other)| {
                                        i != j
                                            && other.as_ref().is_some_and(|other| {
                                                span.start < other.end && other.start < span.end
                                            })
                                    });
                                    // Interpolation is rejected after overlap checks, not before them.
                                    let interpolated = outer_interpolated && span == &spans[0];
                                    (!overlaps && !interpolated)
                                        .then_some(base + span.start..base + span.end)
                                })
                                .collect::<Vec<_>>();
                            assert_eq!(
                                mapped_spans(&source, syntax, &offsets, base),
                                expected,
                                "{selector}: {offsets:?}, base {base}"
                            );
                        }
                    }
                }
            }
        }
    }

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

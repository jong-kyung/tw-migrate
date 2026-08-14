use super::*;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Edit {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) replacement: String,
}

/// Rebase an offset that lies outside every edited range onto the post-edit
/// string produced by [`apply_edits`] with `edits` (sorted, non-overlapping).
pub(super) fn shift_offset(edits: &[Edit], offset: usize) -> usize {
    let mut delta = 0isize;
    for edit in edits {
        if edit.end <= offset {
            delta += edit.replacement.len() as isize - (edit.end - edit.start) as isize;
        }
    }
    offset.checked_add_signed(delta).unwrap_or(offset)
}

pub(crate) fn original_offset(edit_batches: &[Vec<Edit>], mut offset: usize) -> usize {
    for edits in edit_batches.iter().rev() {
        let mut edits = edits.iter().collect::<Vec<_>>();
        edits.sort_by_key(|edit| (edit.start, edit.end));
        let mut delta = 0isize;
        for edit in edits {
            let Some(post_start) = edit.start.checked_add_signed(delta) else {
                continue;
            };
            let post_end = post_start + edit.replacement.len();
            if offset < post_start {
                break;
            }
            if offset < post_end {
                offset = edit.start;
                delta = 0;
                break;
            }
            delta += edit.replacement.len() as isize - (edit.end - edit.start) as isize;
        }
        offset = offset.checked_add_signed(-delta).unwrap_or(offset);
    }
    offset
}

pub(super) fn apply_edits(source: &str, mut edits: Vec<Edit>) -> Result<String, String> {
    edits.sort_by_key(|edit| (edit.start, edit.end));
    for pair in edits.windows(2) {
        if pair[0].end > pair[1].start {
            return Err("Overlapping source edits were produced".to_string());
        }
    }
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    for edit in edits {
        if edit.end > source.len() || edit.start > edit.end {
            return Err("Invalid source edit span".to_string());
        }
        output.push_str(&source[cursor..edit.start]);
        output.push_str(&edit.replacement);
        cursor = edit.end;
    }
    output.push_str(&source[cursor..]);
    Ok(output)
}

pub(super) fn remove_empty_conditionals(
    mut source: String,
    syntax: Syntax,
) -> Result<String, String> {
    loop {
        let allocator = oxc_css_parser::Allocator::default();
        let stylesheet = parse_css(&allocator, &source, syntax)
            .map_err(|error| format!("Failed to parse edited CSS: {error}"))?;
        let mut edits = Vec::new();
        collect_empty_conditionals(&stylesheet.statements, &mut edits);
        if edits.is_empty() {
            return Ok(source);
        }
        source = apply_edits(&source, edits)?;
    }
}

pub(super) fn collect_empty_conditionals(statements: &[Statement<'_>], edits: &mut Vec<Edit>) {
    for statement in statements {
        let Statement::AtRule(at_rule) = statement else {
            continue;
        };
        let Some(block) = &at_rule.block else {
            continue;
        };
        if is_conditional(at_rule.name.name) && block.statements.is_empty() {
            edits.push(Edit {
                start: at_rule.span.start,
                end: at_rule.span.end,
                replacement: String::new(),
            });
        } else {
            collect_empty_conditionals(&block.statements, edits);
        }
    }
}

pub(crate) fn validate_css(source: &str) -> Result<(), String> {
    validate_stylesheet(source, Syntax::Css)
}

pub(super) fn validate_stylesheet(source: &str, syntax: Syntax) -> Result<(), String> {
    let allocator = oxc_css_parser::Allocator::default();
    parse_css(&allocator, source, syntax)
        .map(|_| ())
        .map_err(|error| format!("Edited stylesheet no longer parses: {error}"))
}

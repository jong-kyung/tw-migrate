use super::*;

pub(super) fn apply_edits(source: &str, mut edits: Vec<Edit>) -> MigrationResult<String> {
    edits.sort_by_key(|edit| (edit.start, edit.end));
    for pair in edits.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(MigrationError::PlanCollision {
                message: "Overlapping source edits were produced".to_string(),
            });
        }
    }
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    for edit in edits {
        if edit.end > source.len() || edit.start > edit.end {
            return Err(MigrationError::InvalidEdit {
                message: "Invalid source edit span".to_string(),
            });
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
) -> MigrationResult<String> {
    loop {
        let allocator = oxc_css_parser::Allocator::default();
        let stylesheet = parse_css(&allocator, &source, syntax).map_err(|error| {
            MigrationError::EditedStylesheetParse {
                message: format!("Failed to parse edited CSS: {error}"),
            }
        })?;
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

pub(super) fn validate_stylesheet(source: &str, syntax: Syntax) -> MigrationResult<()> {
    let allocator = oxc_css_parser::Allocator::default();
    parse_css(&allocator, source, syntax)
        .map(|_| ())
        .map_err(|error| MigrationError::EditedStylesheetParse {
            message: format!("Edited stylesheet no longer parses: {error}"),
        })
}

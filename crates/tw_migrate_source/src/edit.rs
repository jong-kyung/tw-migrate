use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

/// Rebase an offset that lies outside every edited range onto the post-edit
/// string produced by applying `edits` in sorted, non-overlapping order.
pub fn shift_offset(edits: &[Edit], offset: usize) -> usize {
    let mut delta = 0isize;
    for edit in edits {
        if edit.end <= offset {
            delta += edit.replacement.len() as isize - (edit.end - edit.start) as isize;
        }
    }
    offset.checked_add_signed(delta).unwrap_or(offset)
}

pub fn original_offset(edit_batches: &[Vec<Edit>], mut offset: usize) -> usize {
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

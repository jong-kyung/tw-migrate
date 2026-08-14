use serde::Serialize;

#[derive(Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub file: String,
    /// Byte offsets into the authored file, or (0, 0) when a preprocessor
    /// rule has no unique authored mapping.
    pub start: usize,
    pub end: usize,
    pub message: String,
}

impl Warning {
    pub fn new(
        code: &'static str,
        file: String,
        (start, end): (usize, usize),
        message: String,
    ) -> Self {
        Self {
            code,
            file,
            start,
            end,
            message,
        }
    }

    pub fn existing_tailwind_conflict(
        file: &str,
        span: (usize, usize),
        generated: &str,
        existing: &str,
    ) -> Self {
        Self::new(
            "existing-tailwind-conflict",
            file.to_string(),
            span,
            format!("Generated utility `{generated}` may conflict with existing `{existing}`."),
        )
    }
}

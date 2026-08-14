use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlanResponse {
    pub(super) files: Vec<PlannedFile>,
    pub(super) deleted_files: Vec<String>,
    pub(super) unlinked_files: Vec<String>,
    pub(super) candidates: Vec<String>,
    pub(super) converted_rules: usize,
    pub(super) retained_rules: usize,
    pub(super) rules: Vec<RuleReport>,
    pub(super) warnings: Vec<Warning>,
    #[serde(skip)]
    pub(super) applied_edits: HashMap<String, Vec<Vec<Edit>>>,
}

#[derive(Serialize)]
pub(super) struct PlannedFile {
    pub(super) path: String,
    pub(super) source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RuleReport {
    pub(super) selector: String,
    pub(super) status: &'static str,
    pub(super) candidates: Vec<String>,
    pub(super) file: String,
    pub(super) rule_id: RuleId,
    /// Authored-domain rule span for anchoring caller-side warnings, or
    /// (0, 0) when the rule has no unique authored mapping.
    pub(super) authored_span: RuleId,
    /// Index of the owning batch stylesheet entry. Same-path entries (a Vue
    /// SFC's scoped and module blocks) reuse local rule spans, so compile
    /// failures must be attributed per entry, not per path; the JS caller
    /// strips this before the public report.
    pub(super) stylesheet: usize,
}

#[derive(Serialize)]
pub(crate) struct Warning {
    pub(crate) code: &'static str,
    pub(crate) file: String,
    /// Byte offsets into the authored file, or (0, 0) when a preprocessor
    /// rule has no unique authored mapping.
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) message: String,
}

impl Warning {
    pub(crate) fn new(
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

    /// The shared warning for a generated utility overlapping a Tailwind
    /// class already present at the rewrite site.
    pub(crate) fn existing_tailwind_conflict(
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

use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlanResponse {
    pub(super) files: Vec<PlannedFile>,
    pub(super) deleted_files: Vec<String>,
    pub(super) unlinked_files: Vec<String>,
    pub(super) candidates: Vec<String>,
    /// Every candidate a rule produced, collected before quote-fit and
    /// source-edit checks so the orchestration layer can canonicalize
    /// spellings whose alias may make a rejected rewrite fit on the next
    /// pass. Over-inclusive by design; internal orchestration data only.
    pub(super) candidate_probes: Vec<String>,
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

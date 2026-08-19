use super::*;

/// One stylesheet's slice of a batch pass: the shared request is borrowed,
/// while `sheet`, the evolving Tailwind entry source, and the per-pass file
/// snapshots are owned because batch planning rewrites them between passes.
pub(super) struct PlanRequest<'a> {
    pub(super) batch: &'a BatchPlanRequest,
    pub(super) sheet: BatchStylesheet,
    pub(super) tailwind_source: Option<String>,
    pub(super) files: Vec<SourceFile>,
}

fn default_entry_writable() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchPlanRequest {
    pub(super) stylesheets: Vec<BatchStylesheet>,
    #[serde(default)]
    pub(super) tailwind_path: Option<String>,
    #[serde(default)]
    pub(super) tailwind_source: Option<String>,
    #[serde(default)]
    pub(super) utility_prefix: Option<String>,
    #[serde(default)]
    pub(super) theme_tokens: HashMap<String, String>,
    /// The entry group's fixed key-to-name map: normalized media condition
    /// keys to resolved variant names. `None` when extraction is disabled,
    /// in which case media handling is unchanged. An explicitly supplied
    /// empty map stays authoritative: every condition then uses the
    /// arbitrary fallback, never the legacy conversions.
    #[serde(default)]
    pub(super) media_names: Option<HashMap<String, String>>,
    /// False when the resolved Tailwind entry must not be edited: keyframe
    /// and global at-rule movement is disabled so their rules retain with
    /// the existing warnings, and no entry file is planned.
    #[serde(default = "default_entry_writable")]
    pub(super) entry_writable: bool,
    /// False for members that reuse an ancestor-shared entry: actively
    /// applied global at-rules stay in their modules while renamed
    /// keyframes may still move.
    #[serde(default = "default_entry_writable")]
    pub(super) global_at_rule_moves: bool,
    /// Caller-accepted canonical spellings keyed by the planner's
    /// rule-level candidate, the same form candidateProbes emits. Applied
    /// after prefixing and before HTML context variants wrap the spelling,
    /// so conditional contexts render the canonical name inside their
    /// generated variants; empty on the first planning pass.
    #[serde(default)]
    pub(super) candidate_aliases: HashMap<String, String>,
    pub(super) files: Vec<SourceFile>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchStylesheet {
    /// Per-stylesheet override of the request-level flag: false for a
    /// shared-entry member's stylesheet inside a group batch, or for a
    /// stylesheet whose registrations collide with another member's.
    #[serde(default)]
    pub(super) global_at_rule_moves: Option<bool>,
    pub(super) css_path: String,
    pub(super) css_source: String,
    #[serde(default)]
    pub(super) analysis_source: Option<String>,
    #[serde(default)]
    pub(super) source_mappings: Vec<SourceMapping>,
    #[serde(default)]
    pub(super) syntax: StylesheetSyntax,
    #[serde(default)]
    pub(super) is_module: Option<bool>,
    #[serde(default)]
    pub(super) is_partial: bool,
    #[serde(default)]
    pub(super) css_module_id: Option<String>,
    #[serde(default)]
    pub(super) css_dependents: Vec<String>,
    /// Present only for Vue SFC stylesheets: the plain-CSS `<style scoped>`
    /// blocks of the `.vue` file named by `css_path`, whose `css_source` is
    /// the whole SFC source.
    #[serde(default)]
    pub(super) vue_blocks: Vec<VueBlock>,
    /// True for the `<style module>` entry of an SFC: consumers match proven
    /// `$style` binding sites instead of literal classes.
    #[serde(default)]
    pub(super) vue_module: bool,
    /// Present only for Vue SFC stylesheets with an open template surface:
    /// the retention warning code every otherwise-unwarned rule receives.
    #[serde(default)]
    pub(super) vue_retention: Option<String>,
    #[serde(default)]
    pub(super) vue_unscoped: bool,
    /// Present only for closed Vue SFC stylesheets: the package's non-scoped
    /// CSS corpus as parseable pieces (other stylesheets, retained SFC
    /// blocks, scope-escape selector fragments). Their parsed selector
    /// surface decides which scoped rules may be deleted without handing the
    /// cascade to an unlayered competitor.
    #[serde(default)]
    pub(super) vue_shadow_css: Vec<String>,
    /// CSS Module sources: their class and id names are localized at build
    /// time, so only their bare type and attribute selectors join the index.
    #[serde(default)]
    pub(super) vue_shadow_module_css: Vec<String>,
    /// True when the shadow corpus contains selectors that cannot be proven
    /// (preprocessor interpolation or concatenation, inline HTML styles,
    /// unextractable escapes); every closed rule is then retained.
    #[serde(default)]
    pub(super) vue_shadow_unverifiable: bool,
    /// Rules whose candidates failed Tailwind compilation in a previous
    /// planning pass; they are retained without converting anything.
    #[serde(default)]
    pub(super) blocked_rules: Vec<RuleId>,
}

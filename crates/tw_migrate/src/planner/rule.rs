use super::*;

/// Span of a rule in the analysis source (the compiled CSS for preprocessor
/// stylesheets), stable only across plans over identical `analysisSource`;
/// it round-trips through `blockedRules` in that domain.
#[derive(Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(super) struct RuleId {
    pub(super) start: usize,
    pub(super) end: usize,
}

pub(super) fn rule_id(rule: &RulePlan) -> RuleId {
    RuleId {
        start: rule.span.start,
        end: rule.span.end,
    }
}

pub(super) type RuleConflicts = HashMap<RuleId, BTreeSet<(String, String)>>;

pub(super) struct RuleOrigin {
    pub(super) rule: RuleId,
    pub(super) properties: BTreeSet<String>,
}

pub(super) struct CandidateMaps {
    pub(super) candidates: HashMap<SelectorKey, Vec<String>>,
    pub(super) origins: HashMap<(SelectorKey, String), Vec<RuleOrigin>>,
    pub(super) rule_selectors: HashMap<RuleId, String>,
    pub(super) retained_rules: HashSet<RuleId>,
    /// Multi-compound module rules whose relationship proof failed, with the
    /// retained-rule message.
    pub(super) unproven: HashMap<RuleId, String>,
}

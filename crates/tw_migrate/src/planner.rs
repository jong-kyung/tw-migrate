use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
};

use oxc_css_parser::{
    Parser as CssParser, Syntax,
    ast::{Statement, Stylesheet},
};
use serde::{Deserialize, Serialize};

use crate::{
    animations::append_keyframes,
    at_rules::{append_global_at_rules, is_conditional},
    css_plan::{ParseOptions, ParsedCss, RulePlan, SelectorKey, parse_css_rules},
    html_rewrite::plan_html_file,
    js_rewrite::{SourcePlan, plan_batch_source_file, plan_source_file, validate_js},
    jsx_graph,
    utilities::{css_properties_conflict, tailwind_utilities_conflict, tailwind_variants_match},
};

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum StylesheetSyntax {
    #[default]
    Css,
    Scss,
    Sass,
    Less,
}

impl StylesheetSyntax {
    fn parser_syntax(self) -> Syntax {
        match self {
            Self::Css => Syntax::Css,
            Self::Scss => Syntax::Scss,
            Self::Sass => Syntax::Sass,
            Self::Less => Syntax::Less,
        }
    }
}

fn is_stylesheet_module(path: &str) -> bool {
    ["css", "scss", "sass", "less"]
        .iter()
        .any(|extension| path.ends_with(&format!(".module.{extension}")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanRequest {
    css_path: String,
    css_source: String,
    #[serde(default)]
    analysis_source: Option<String>,
    #[serde(default)]
    source_mappings: Vec<SourceMapping>,
    #[serde(default)]
    syntax: StylesheetSyntax,
    #[serde(default)]
    is_module: Option<bool>,
    #[serde(default)]
    is_partial: bool,
    #[serde(default)]
    css_module_id: Option<String>,
    #[serde(default)]
    tailwind_path: Option<String>,
    #[serde(default)]
    tailwind_source: Option<String>,
    #[serde(default)]
    utility_prefix: Option<String>,
    #[serde(default)]
    theme_tokens: HashMap<String, String>,
    #[serde(default)]
    css_dependents: Vec<String>,
    files: Vec<SourceFile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchPlanRequest {
    stylesheets: Vec<BatchStylesheet>,
    #[serde(default)]
    tailwind_path: Option<String>,
    #[serde(default)]
    tailwind_source: Option<String>,
    #[serde(default)]
    utility_prefix: Option<String>,
    #[serde(default)]
    theme_tokens: HashMap<String, String>,
    files: Vec<SourceFile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchStylesheet {
    css_path: String,
    css_source: String,
    #[serde(default)]
    analysis_source: Option<String>,
    #[serde(default)]
    source_mappings: Vec<SourceMapping>,
    #[serde(default)]
    syntax: StylesheetSyntax,
    #[serde(default)]
    is_module: Option<bool>,
    #[serde(default)]
    is_partial: bool,
    #[serde(default)]
    css_module_id: Option<String>,
    #[serde(default)]
    css_dependents: Vec<String>,
    /// Rules whose candidates failed Tailwind compilation in a previous
    /// planning pass; they are retained without converting anything.
    #[serde(default)]
    blocked_rules: Vec<RuleId>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceMapping {
    generated_line: usize,
    generated_column: usize,
    source_path: String,
    original_line: usize,
    original_column: usize,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HtmlAttribute {
    pub(crate) value: String,
    pub(crate) start: usize,
    pub(crate) end: usize,
    #[serde(default)]
    pub(crate) synthetic: bool,
    #[serde(default = "default_writable")]
    pub(crate) writable: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HtmlElement {
    pub(crate) class_attribute: Option<HtmlAttribute>,
    pub(crate) id_attribute: Option<HtmlAttribute>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HtmlStylesheet {
    pub(crate) css_path: String,
    pub(crate) variants: Vec<String>,
    #[serde(default)]
    pub(crate) direct: bool,
    #[serde(default = "default_writable")]
    pub(crate) analyzable: bool,
}

fn default_writable() -> bool {
    true
}

#[derive(Clone, Deserialize)]
pub(crate) struct SourceFile {
    pub(crate) path: String,
    pub(crate) source: String,
    #[serde(default = "default_writable")]
    pub(crate) writable: bool,
    #[serde(default, rename = "htmlElements")]
    pub(crate) html_elements: Vec<HtmlElement>,
    #[serde(default, rename = "htmlStylesheets")]
    pub(crate) html_stylesheets: Vec<HtmlStylesheet>,
    #[serde(default = "default_writable", rename = "htmlReferencesSafe")]
    pub(crate) html_references_safe: bool,
    #[serde(default, rename = "htmlScriptText")]
    pub(crate) html_script_text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanResponse {
    files: Vec<PlannedFile>,
    deleted_files: Vec<String>,
    unlinked_files: Vec<String>,
    candidates: Vec<String>,
    converted_rules: usize,
    retained_rules: usize,
    rules: Vec<RuleReport>,
    warnings: Vec<Warning>,
}

#[derive(Serialize)]
struct PlannedFile {
    path: String,
    source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleReport {
    selector: String,
    status: &'static str,
    candidates: Vec<String>,
    file: String,
    rule_id: RuleId,
    /// Authored-domain rule span for anchoring caller-side warnings, or
    /// (0, 0) when the rule has no unique authored mapping.
    authored_span: RuleId,
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

/// Every warning code the migration can emit, including reason strings that
/// flow through `rule.warning` and the JS-side `candidate-compilation-failure`
/// stamped in index.js. Adding or removing a code requires updating this list
/// and the README warning table; `tests::warning_codes_are_pinned_to_the_readme`
/// enforces both.
#[cfg(test)]
const WARNING_CODES: &[&str] = &[
    "aliased-css-module-reference",
    "batch-stylesheet-conflict",
    "candidate-compilation-failure",
    "computed-css-module-reference",
    "cross-package-stylesheet-link",
    "css-module-composes",
    "dynamic-class-name",
    "dynamic-html-attribute",
    "existing-tailwind-conflict",
    "inferred-preprocessor-source",
    "module-utilities-conflict",
    "non-classname-css-module-reference",
    "rebuild-required",
    "reference-only-css-module-consumer",
    "retained-global-rule",
    "shared-preprocessor-source",
    "unproven-css-module-relationship",
    "unproven-script-reference",
    "unproven-source-map",
    "unresolved-selector-target",
    "unsupported-animation",
    "unsupported-at-rule",
    "unsupported-container-query",
    "unsupported-css-module-reference",
    "unsupported-declaration",
    "unsupported-html-base",
    "unsupported-html-stylesheet-link",
    "unsupported-important",
    "unsupported-link-media",
    "unsupported-media-query",
    "unsupported-nested-at-rule",
    "unsupported-overlap",
    "unsupported-rule-content",
    "unsupported-selector",
    "unsupported-starting-style",
    "unsupported-supports-query",
    "unsupported-value",
];

#[derive(Clone)]
pub(crate) struct Edit {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) replacement: String,
}

pub fn plan_json(request: &str) -> Result<String, String> {
    let request: PlanRequest = serde_json::from_str(request).map_err(|error| error.to_string())?;
    serde_json::to_string(&plan_request(
        request,
        &HashMap::new(),
        &HashSet::new(),
        &HashMap::new(),
        false,
    )?)
    .map_err(|error| error.to_string())
}

/// Span of a rule in the analysis source (the compiled CSS for preprocessor
/// stylesheets), stable only across plans over identical `analysisSource`;
/// it round-trips through `blockedRules` in that domain.
#[derive(Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
struct RuleId {
    start: usize,
    end: usize,
}

fn rule_id(rule: &RulePlan) -> RuleId {
    RuleId {
        start: rule.span.start,
        end: rule.span.end,
    }
}

type RuleConflicts = HashMap<RuleId, BTreeSet<(String, String)>>;

struct RuleOrigin {
    rule: RuleId,
    properties: BTreeSet<String>,
}

struct CandidateMaps {
    candidates: HashMap<SelectorKey, Vec<String>>,
    origins: HashMap<(SelectorKey, String), Vec<RuleOrigin>>,
    /// Multi-compound module rules whose relationship proof failed, with the
    /// retained-rule message.
    unproven: HashMap<RuleId, String>,
}

/// Run the JSX-graph proofs for every proof-needing rule against `files` (the
/// request's immutable snapshot) and return the rules that must be retained
/// with `unproven-css-module-relationship`, keyed by rule with their message.
// ponytail: the world is rebuilt once per stylesheet; share it across a
// batch's stylesheets if proof volume ever matters.
fn unproven_relationship_rules(
    rules: &[RulePlan],
    css_path: &str,
    files: &[SourceFile],
) -> HashMap<RuleId, String> {
    let proof_files = files
        .iter()
        .map(|file| (file.path.as_str(), file.source.as_str()))
        .collect::<Vec<_>>();
    let mut prepared = None;
    let mut unproven = HashMap::new();
    for rule in rules {
        let Some(relationship) = &rule.relationship else {
            continue;
        };
        if rule.warning.is_some() {
            continue;
        }
        let rule_id = rule_id(rule);
        if relationship.ancestor_state {
            unproven.insert(
                rule_id,
                format!(
                    "Ancestor-state selectors like `{}` are not convertible yet, so the rule is retained.",
                    rule.selector
                ),
            );
            continue;
        }
        for (index, step) in relationship.steps.iter().enumerate() {
            let prepared = prepared
                .get_or_insert_with(|| jsx_graph::prepare(&proof_files, css_path));
            let outcome = jsx_graph::prove_prepared(
                prepared,
                &step.ancestor,
                step.relation,
                &step.target,
                true,
            );
            if !outcome.aggregate_proven {
                let reason = outcome.reason.unwrap_or("unproven");
                let site = outcome
                    .usages
                    .iter()
                    .find(|usage| !usage.proven)
                    .map(|usage| format!(" at {}:{}", usage.file, usage.span.0))
                    .unwrap_or_default();
                unproven.insert(
                    rule_id,
                    format!(
                        "The selector `{}` requires a relationship that could not be proven for every usage ({reason}{site}), so the rule is retained.",
                        rule.selector
                    ),
                );
                break;
            }
            // The first step's target is the rule's own key: its usage sites
            // are the ones conversion would edit, so a non-writable site
            // makes the proven rule unconvertible.
            if index == 0
                && let Some(usage) = outcome.usages.iter().find(|usage| {
                    files
                        .iter()
                        .any(|file| !file.writable && file.path == usage.file)
                })
            {
                unproven.insert(
                    rule_id,
                    format!(
                        "The selector `{}` matches a usage in the reference-only file {}, so the rule is retained.",
                        rule.selector, usage.file
                    ),
                );
                break;
            }
        }
    }
    unproven
}

fn stamp_unproven_rules(rules: &mut [RulePlan], unproven: &HashMap<RuleId, String>) {
    for rule in rules {
        let rule_id = rule_id(rule);
        if rule.warning.is_none() && unproven.contains_key(&rule_id) {
            rule.warning = Some("unproven-css-module-relationship");
        }
    }
}

fn prefix_rule_candidates(rules: &mut [RulePlan], prefix: &str) {
    for rule in rules {
        rule.candidates = rule
            .candidates
            .drain(..)
            .map(|candidate| format!("{prefix}:{candidate}"))
            .collect();
        rule.candidate_properties = std::mem::take(&mut rule.candidate_properties)
            .into_iter()
            .map(|(candidate, properties)| (format!("{prefix}:{candidate}"), properties))
            .collect();
    }
}

struct BatchMatch {
    stylesheet: usize,
    candidate: String,
    rule: RuleId,
    properties: BTreeSet<String>,
}

fn plan_consumer_file(
    file: &SourceFile,
    css_path: &str,
    is_module: bool,
    candidates: &HashMap<SelectorKey, Vec<String>>,
    preserved_module_classes: &BTreeSet<String>,
    utility_prefix: Option<&str>,
    batch_mode: bool,
) -> Result<SourcePlan, String> {
    if Path::new(&file.path)
        .extension()
        .is_some_and(|extension| extension == "html")
    {
        return Ok(plan_html_file(file, css_path, candidates, utility_prefix));
    }
    if batch_mode {
        plan_batch_source_file(
            file,
            css_path,
            is_module,
            candidates,
            preserved_module_classes,
        )
    } else {
        plan_source_file(file, css_path, is_module, candidates)
    }
}

pub(crate) fn is_recoverable_input_error(error: &str) -> bool {
    (!error.starts_with("Failed to parse edited CSS") && error.starts_with("Failed to parse "))
        || error.starts_with("Failed to analyze ")
        || error.starts_with("Unsupported source file ")
}

pub fn plan_batch_json(request: &str) -> Result<String, String> {
    let request: BatchPlanRequest =
        serde_json::from_str(request).map_err(|error| error.to_string())?;
    if request.stylesheets.is_empty() {
        return Err("Batch migration requires at least one stylesheet".to_string());
    }

    let mut match_groups: HashMap<(String, usize, usize), Vec<BatchMatch>> = HashMap::new();
    // Relationship proofs run here against the request's immutable file set,
    // so every stylesheet is proven on the same snapshot regardless of the
    // edits earlier stylesheets make during the main pass.
    let mut unproven_maps = Vec::new();
    for (index, stylesheet) in request.stylesheets.iter().enumerate() {
        let plan_request = batch_stylesheet_request(&request, stylesheet, request.files.clone());
        let candidate_maps = candidate_map_for_request(&plan_request)?;
        unproven_maps.push(candidate_maps.unproven);
        for file in request.files.iter().filter(|file| file.writable) {
            let result = plan_consumer_file(
                file,
                &stylesheet.css_path,
                stylesheet
                    .is_module
                    .unwrap_or_else(|| is_stylesheet_module(&stylesheet.css_path)),
                &candidate_maps.candidates,
                &BTreeSet::new(),
                request.utility_prefix.as_deref(),
                true,
            )?;
            for matched in result.matches {
                if let Some(origins) = candidate_maps
                    .origins
                    .get(&(matched.key, matched.origin_candidate.clone()))
                {
                    match_groups
                        .entry((file.path.clone(), matched.start, matched.end))
                        .or_default()
                        .extend(origins.iter().map(|origin| BatchMatch {
                            stylesheet: index,
                            candidate: matched.candidate.clone(),
                            rule: origin.rule,
                            properties: origin.properties.clone(),
                        }));
                }
            }
        }
    }

    let mut blocked_rules: Vec<RuleConflicts> = vec![HashMap::new(); request.stylesheets.len()];
    for matches in match_groups.values() {
        for (left_index, left) in matches.iter().enumerate() {
            for right in &matches[left_index + 1..] {
                if left.stylesheet != right.stylesheet
                    && (tailwind_utilities_conflict(&left.candidate, &right.candidate)
                        || (tailwind_variants_match(&left.candidate, &right.candidate)
                            && left.properties.iter().any(|left_property| {
                                right.properties.iter().any(|right_property| {
                                    css_properties_conflict(left_property, right_property)
                                })
                            })))
                {
                    let pair = if left.candidate <= right.candidate {
                        (left.candidate.clone(), right.candidate.clone())
                    } else {
                        (right.candidate.clone(), left.candidate.clone())
                    };
                    blocked_rules[left.stylesheet]
                        .entry(left.rule)
                        .or_default()
                        .insert(pair.clone());
                    blocked_rules[right.stylesheet]
                        .entry(right.rule)
                        .or_default()
                        .insert(pair);
                }
            }
        }
    }

    let mut originals = HashMap::new();
    for file in &request.files {
        originals.insert(file.path.clone(), file.source.clone());
    }
    for stylesheet in &request.stylesheets {
        originals.insert(stylesheet.css_path.clone(), stylesheet.css_source.clone());
    }
    if let Some((path, source)) = request
        .tailwind_path
        .as_ref()
        .zip(request.tailwind_source.as_ref())
    {
        originals.insert(path.clone(), source.clone());
    }
    let mut current = originals.clone();
    let mut deleted = HashSet::new();
    let mut unlinked = HashSet::new();
    let mut candidates = BTreeSet::new();
    let mut converted_rules = 0;
    let mut retained_rules = 0;
    let mut rules = Vec::new();
    let mut warnings = Vec::new();
    let mut order = (0..request.stylesheets.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        request.stylesheets[*left]
            .css_path
            .cmp(&request.stylesheets[*right].css_path)
    });

    for index in order {
        let stylesheet = &request.stylesheets[index];
        let mut files = request.files.clone();
        for file in &mut files {
            if let Some(source) = current.get(&file.path) {
                file.source.clone_from(source);
            }
        }
        let mut stylesheet_request = batch_stylesheet_request(&request, stylesheet, files);
        stylesheet_request.css_source = current
            .get(&stylesheet.css_path)
            .cloned()
            .unwrap_or_else(|| stylesheet.css_source.clone());
        stylesheet_request.tailwind_source = request
            .tailwind_path
            .as_ref()
            .and_then(|path| current.get(path).cloned());
        let externally_blocked = stylesheet
            .blocked_rules
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let response = plan_request(
            stylesheet_request,
            &blocked_rules[index],
            &externally_blocked,
            &unproven_maps[index],
            true,
        )?;

        for file in response.files {
            deleted.remove(&file.path);
            current.insert(file.path, file.source);
        }
        for path in response.deleted_files {
            current.remove(&path);
            deleted.insert(path);
        }
        unlinked.extend(response.unlinked_files);
        candidates.extend(response.candidates);
        converted_rules += response.converted_rules;
        retained_rules += response.retained_rules;
        rules.extend(response.rules);
        warnings.extend(response.warnings);
    }

    let mut files = current
        .into_iter()
        .filter(|(path, source)| originals.get(path).is_some_and(|before| before != source))
        .map(|(path, source)| PlannedFile { path, source })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let mut deleted_files = deleted.into_iter().collect::<Vec<_>>();
    deleted_files.sort();
    let mut unlinked_files = unlinked.into_iter().collect::<Vec<_>>();
    unlinked_files.sort();
    warnings.sort_by(|left, right| {
        (&left.file, left.start, left.end, left.code).cmp(&(
            &right.file,
            right.start,
            right.end,
            right.code,
        ))
    });

    serde_json::to_string(&PlanResponse {
        files,
        deleted_files,
        unlinked_files,
        candidates: candidates.into_iter().collect(),
        converted_rules,
        retained_rules,
        rules,
        warnings,
    })
    .map_err(|error| error.to_string())
}

fn batch_stylesheet_request(
    batch: &BatchPlanRequest,
    stylesheet: &BatchStylesheet,
    files: Vec<SourceFile>,
) -> PlanRequest {
    PlanRequest {
        css_path: stylesheet.css_path.clone(),
        css_source: stylesheet.css_source.clone(),
        analysis_source: stylesheet.analysis_source.clone(),
        source_mappings: stylesheet.source_mappings.clone(),
        syntax: stylesheet.syntax,
        is_module: stylesheet.is_module,
        is_partial: stylesheet.is_partial,
        css_module_id: stylesheet.css_module_id.clone(),
        tailwind_path: batch.tailwind_path.clone(),
        tailwind_source: batch.tailwind_source.clone(),
        utility_prefix: batch.utility_prefix.clone(),
        theme_tokens: batch.theme_tokens.clone(),
        css_dependents: stylesheet.css_dependents.clone(),
        files,
    }
}

/// Shared head of the single-pass and batch-pass pipelines: derive the
/// request flags, parse the stylesheet, and apply the utility prefix, so
/// rule-selection behavior cannot silently diverge between the two paths.
fn parse_request_rules(request: &PlanRequest) -> Result<(bool, ParsedCss), String> {
    let is_module = request
        .is_module
        .unwrap_or_else(|| is_stylesheet_module(&request.css_path));
    let can_move_at_rules = request.syntax == StylesheetSyntax::Css
        && request
            .tailwind_path
            .as_ref()
            .zip(request.tailwind_source.as_ref())
            .is_some_and(|(path, _)| path != &request.css_path);
    let relative_urls_stable = request
        .tailwind_path
        .as_ref()
        .is_some_and(|path| Path::new(path).parent() == Path::new(&request.css_path).parent());
    let keyframe_scope = request
        .css_module_id
        .as_deref()
        .unwrap_or(&request.css_path);
    let analysis_source = request
        .analysis_source
        .as_deref()
        .unwrap_or(&request.css_source);
    let analysis_syntax = if request.analysis_source.is_some() {
        Syntax::Css
    } else {
        request.syntax.parser_syntax()
    };
    let mut parsed = parse_css_rules(
        &request.css_path,
        keyframe_scope,
        analysis_source,
        &request.theme_tokens,
        ParseOptions {
            syntax: analysis_syntax,
            is_module,
            can_move_at_rules,
            relative_urls_stable,
        },
    )?;
    if request.analysis_source.is_some() {
        map_authored_rule_spans(request, analysis_source, &mut parsed.rules)?;
        if is_module {
            for rule in &mut parsed.rules {
                if rule.warning.is_none() && rule.authored_span.is_none() {
                    rule.warning = Some("unproven-source-map");
                }
            }
        }
    } else {
        for rule in &mut parsed.rules {
            rule.authored_span = Some(rule.span.clone());
        }
    }
    if request.is_partial {
        for rule in &mut parsed.rules {
            rule.warning = Some("shared-preprocessor-source");
        }
    }
    if let Some(prefix) = request
        .utility_prefix
        .as_deref()
        .filter(|prefix| !prefix.is_empty())
    {
        prefix_rule_candidates(&mut parsed.rules, prefix);
    }
    Ok((is_module, parsed))
}

fn map_authored_rule_spans(
    request: &PlanRequest,
    analysis_source: &str,
    rules: &mut [RulePlan],
) -> Result<(), String> {
    let allocator = oxc_css_parser::Allocator::default();
    let mut parser = CssParser::new(
        &allocator,
        &request.css_source,
        request.syntax.parser_syntax(),
    );
    let stylesheet = parser
        .parse::<Stylesheet>()
        .map_err(|error| format!("Failed to parse {}: {error:?}", request.css_path))?;
    let mut authored_rules = Vec::new();
    collect_qualified_rule_spans(&stylesheet.statements, &mut authored_rules);
    let mappings = request
        .source_mappings
        .iter()
        .map(|mapping| ((mapping.generated_line, mapping.generated_column), mapping))
        .collect::<HashMap<_, _>>();

    for rule in rules.iter_mut() {
        let mut original_offsets = Vec::new();
        for generated_offset in &rule.provenance_offsets {
            let Some(position) = offset_to_line_column(analysis_source, *generated_offset) else {
                original_offsets.clear();
                break;
            };
            let Some(mapping) = mappings.get(&position) else {
                original_offsets.clear();
                break;
            };
            if mapping.source_path != request.css_path {
                original_offsets.clear();
                break;
            }
            let Some(offset) = line_column_to_offset(
                &request.css_source,
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
            .map(|(span, _)| span.clone());
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
    let interpolation = match request.syntax {
        StylesheetSyntax::Scss | StylesheetSyntax::Sass => Some("#{"),
        StylesheetSyntax::Less => Some("@{"),
        StylesheetSyntax::Css => None,
    };
    if let Some(interpolation) = interpolation {
        for rule in rules {
            let interpolated = rule.authored_span.as_ref().is_some_and(|authored_span| {
                authored_rules.iter().any(|(span, selector_span)| {
                    span == authored_span
                        && request.css_source[selector_span.clone()].contains(interpolation)
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

fn offset_to_line_column(source: &str, offset: usize) -> Option<(usize, usize)> {
    if offset > source.len() || !source.is_char_boundary(offset) {
        return None;
    }
    let mut line = 0;
    let mut column = 0;
    for character in source[..offset].chars() {
        if character == '\n' {
            line += 1;
            column = 0;
        } else {
            column += character.len_utf16();
        }
    }
    Some((line, column))
}

fn line_column_to_offset(source: &str, target_line: usize, target_column: usize) -> Option<usize> {
    let mut line = 0;
    let mut column = 0;
    for (offset, character) in source.char_indices() {
        if line == target_line && column == target_column {
            return Some(offset);
        }
        if character == '\n' {
            line += 1;
            column = 0;
        } else {
            column += character.len_utf16();
            if line == target_line && column > target_column {
                return None;
            }
        }
    }
    (line == target_line && column == target_column).then_some(source.len())
}

fn mentions_word(text: &str, word: &str) -> bool {
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

fn dedup_candidate_map(candidate_map: &mut HashMap<SelectorKey, Vec<String>>) {
    for candidates in candidate_map.values_mut() {
        candidates.sort();
        candidates.dedup();
    }
}

fn candidate_map_for_request(request: &PlanRequest) -> Result<CandidateMaps, String> {
    let (_, ParsedCss { mut rules, .. }) = parse_request_rules(request)?;
    let unproven = unproven_relationship_rules(&rules, &request.css_path, &request.files);
    stamp_unproven_rules(&mut rules, &unproven);
    let blocked_classes = rules
        .iter()
        .filter(|rule| rule.warning.is_some())
        .flat_map(|rule| rule.related_classes.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut candidate_map: HashMap<SelectorKey, Vec<String>> = HashMap::new();
    let mut origins: HashMap<(SelectorKey, String), Vec<RuleOrigin>> = HashMap::new();
    for rule in rules {
        let rule_id = rule_id(&rule);
        if let Some(key) = rule.key
            && rule.warning.is_none()
            && !matches!(&key, SelectorKey::Class(name) if blocked_classes.contains(name))
        {
            for candidate in &rule.candidates {
                origins
                    .entry((key.clone(), candidate.clone()))
                    .or_default()
                    .push(RuleOrigin {
                        rule: rule_id,
                        properties: rule
                            .candidate_properties
                            .get(candidate)
                            .cloned()
                            .unwrap_or_default(),
                    });
            }
            candidate_map
                .entry(key)
                .or_default()
                .extend(rule.candidates);
        }
    }
    dedup_candidate_map(&mut candidate_map);
    Ok(CandidateMaps {
        candidates: candidate_map,
        origins,
        unproven,
    })
}

/// Warnings that retain a single rule during batch planning without blocking
/// the rest of its class's rules from converting.
fn is_batch_retained(warning: Option<&str>) -> bool {
    matches!(
        warning,
        Some("batch-stylesheet-conflict" | "candidate-compilation-failure")
    )
}

fn plan_request(
    request: PlanRequest,
    blocked_rules: &RuleConflicts,
    externally_blocked: &HashSet<RuleId>,
    unproven_rules: &HashMap<RuleId, String>,
    batch_mode: bool,
) -> Result<PlanResponse, String> {
    let (
        is_module,
        ParsedCss {
            mut rules,
            keyframes,
            global_at_rules,
        },
    ) = parse_request_rules(&request)?;
    for rule in &mut rules {
        let rule_id = rule_id(rule);
        if blocked_rules.contains_key(&rule_id) {
            rule.warning = Some("batch-stylesheet-conflict");
        } else if rule.warning.is_none() && externally_blocked.contains(&rule_id) {
            rule.warning = Some("candidate-compilation-failure");
        }
    }
    // In batch mode the caller passes proof results computed against the
    // request snapshot; single-pass mode proves against its own files, which
    // are that snapshot.
    let computed_unproven;
    let unproven_rules = if batch_mode {
        unproven_rules
    } else {
        computed_unproven =
            unproven_relationship_rules(&rules, &request.css_path, &request.files);
        &computed_unproven
    };
    stamp_unproven_rules(&mut rules, unproven_rules);

    let preserved_module_classes = rules
        .iter()
        .filter(|rule| batch_mode && is_module && is_batch_retained(rule.warning))
        .flat_map(|rule| rule.related_classes.iter().cloned())
        .collect::<BTreeSet<_>>();
    let blocked_classes = rules
        .iter()
        .filter(|rule| rule.warning.is_some() && !(batch_mode && is_batch_retained(rule.warning)))
        .flat_map(|rule| rule.related_classes.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut candidate_map: HashMap<SelectorKey, Vec<String>> = HashMap::new();
    for rule in &rules {
        if let Some(key) = &rule.key
            && rule.warning.is_none()
            && !matches!(key, SelectorKey::Class(name) if blocked_classes.contains(name))
        {
            candidate_map
                .entry(key.clone())
                .or_default()
                .extend(rule.candidates.clone());
        }
    }
    dedup_candidate_map(&mut candidate_map);

    let mut planned_files = Vec::new();
    let mut candidates = BTreeSet::new();
    let mut module_refs: HashMap<String, usize> = HashMap::new();
    let mut matched_module_refs: HashMap<String, usize> = HashMap::new();
    let mut module_references_safe = true;
    let mut warnings = Vec::new();
    let mut source_plans = Vec::new();

    if is_module && !request.css_dependents.is_empty() {
        // Another stylesheet depends on this module (composes/@import), so
        // deleting it or removing imports would break that consumer.
        module_references_safe = false;
        for dependent in &request.css_dependents {
            warnings.push(Warning {
                code: "unsupported-css-module-reference",
                file: dependent.clone(),
                start: 0,
                end: 0,
                message: "Another stylesheet references the CSS Module, so it is retained."
                    .to_string(),
            });
        }
    }

    for file in &request.files {
        let mut result = plan_consumer_file(
            file,
            &request.css_path,
            is_module,
            &candidate_map,
            &preserved_module_classes,
            request.utility_prefix.as_deref(),
            batch_mode,
        )?;

        module_references_safe &= result.module_references_safe;
        let direct_html_link = file
            .html_stylesheets
            .iter()
            .any(|context| context.direct && context.css_path == request.css_path);
        let unsafe_html_link = file.html_stylesheets.iter().any(|context| {
            context.direct && !context.analyzable && context.css_path == request.css_path
        });
        if is_module && (unsafe_html_link || (direct_html_link && !file.html_references_safe)) {
            module_references_safe = false;
        }
        // Inline scripts are never analyzed, so a script that names one of the
        // module's classes may create consumers at runtime; retain the module.
        let any_html_context = file
            .html_stylesheets
            .iter()
            .any(|context| context.css_path == request.css_path);
        if is_module
            && any_html_context
            && !file.html_script_text.is_empty()
            && rules.iter().any(|rule| {
                rule.related_classes
                    .iter()
                    .any(|class| mentions_word(&file.html_script_text, class))
            })
        {
            module_references_safe = false;
            warnings.push(Warning {
                code: "unproven-script-reference",
                file: file.path.clone(),
                start: 0,
                end: 0,
                message: "An inline script names a CSS Module class, so the module is retained."
                    .to_string(),
            });
        }
        if !file.writable {
            if is_module
                && (direct_html_link
                    || !result.module_refs.is_empty()
                    || !result.removable_import_edits.is_empty())
            {
                module_references_safe = false;
                warnings.push(Warning {
                    code: "reference-only-css-module-consumer",
                    file: file.path.clone(),
                    start: 0,
                    end: 0,
                    message: "A reference-only source uses this CSS Module, so it is retained."
                        .to_string(),
                });
            }
            result.edits.clear();
            result.removable_import_edits.clear();
            result.candidates.clear();
            result.matched_module_refs.clear();
        }
        for candidate in &result.candidates {
            candidates.insert(candidate.clone());
        }
        merge_counts(&mut module_refs, &result.module_refs);
        merge_counts(&mut matched_module_refs, &result.matched_module_refs);
        warnings.append(&mut result.warnings);
        source_plans.push((file, result));
    }

    let all_module_refs_migrated =
        module_refs.values().sum::<usize>() == matched_module_refs.values().sum::<usize>();

    let mut css_edits = Vec::new();
    let mut converted_rules = 0;
    let mut retained_rules = 0;
    let mut rule_reports = Vec::new();

    for rule in rules {
        let can_remove = is_module
            && module_references_safe
            && (batch_mode || all_module_refs_migrated)
            && rule.warning.is_none()
            && match &rule.key {
                Some(SelectorKey::Class(name)) => {
                    let refs = module_refs.get(name).copied().unwrap_or(0);
                    refs > 0 && matched_module_refs.get(name).copied().unwrap_or(0) == refs
                }
                _ => false,
            };

        let rule_id = rule_id(&rule);
        let report_authored_span = rule.authored_span.as_ref().map_or(
            RuleId { start: 0, end: 0 },
            |span| RuleId {
                start: span.start,
                end: span.end,
            },
        );
        if can_remove {
            converted_rules += 1;
            let authored_span = rule
                .authored_span
                .clone()
                .expect("removable rules must have proven authored spans");
            css_edits.push(Edit {
                start: authored_span.start,
                end: authored_span.end,
                replacement: String::new(),
            });
            rule_reports.push(RuleReport {
                selector: rule.selector,
                status: "converted",
                candidates: rule.candidates,
                file: request.css_path.clone(),
                rule_id,
                authored_span: report_authored_span,
            });
        } else if rule.warning == Some("candidate-compilation-failure") {
            // The caller blocked this rule after a Tailwind compilation
            // failure and attributes the warning itself.
            retained_rules += 1;
            rule_reports.push(RuleReport {
                selector: rule.selector,
                status: "retained",
                candidates: rule.candidates,
                file: request.css_path.clone(),
                rule_id,
                authored_span: report_authored_span,
            });
        } else {
            retained_rules += 1;
            let (code, message) = if rule.warning == Some("batch-stylesheet-conflict") {
                let conflicts = blocked_rules
                    .get(&rule_id)
                    .expect("conflicting rule must retain its candidates")
                    .iter()
                    .map(|(left, right)| format!("`{left}` and `{right}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    "batch-stylesheet-conflict",
                    format!(
                        "Generated utilities {conflicts} conflict on the same source element, so the contributing rule is retained."
                    ),
                )
            } else if rule.warning == Some("unproven-css-module-relationship") {
                (
                    "unproven-css-module-relationship",
                    unproven_rules.get(&rule_id).cloned().unwrap_or_else(|| {
                        "The CSS Module selector relationship could not be proven for every usage."
                            .to_string()
                    }),
                )
            } else if rule.warning == Some("unproven-source-map") {
                (
                    "unproven-source-map",
                    "The generated rule does not map uniquely to one authored source rule, so it is retained."
                        .to_string(),
                )
            } else if rule.warning == Some("shared-preprocessor-source") {
                (
                    "shared-preprocessor-source",
                    "A Sass partial must be analyzed through every consuming entry, so it is retained."
                        .to_string(),
                )
            } else if let Some(code) = rule.warning {
                (
                    code,
                    "The rule is outside the supported declaration or selector subset.".to_string(),
                )
            } else if !is_module {
                (
                    "retained-global-rule",
                    "Global CSS is never deleted automatically.".to_string(),
                )
            } else {
                (
                    "unresolved-selector-target",
                    "No exclusively supported className references were found.".to_string(),
                )
            };
            warnings.push(Warning {
                code,
                file: request.css_path.clone(),
                start: report_authored_span.start,
                end: report_authored_span.end,
                message,
            });
            rule_reports.push(RuleReport {
                selector: rule.selector,
                status: "retained",
                candidates: rule.candidates,
                file: request.css_path.clone(),
                rule_id,
                authored_span: report_authored_span,
            });
        }
    }

    let remove_at_rules =
        is_module && module_references_safe && all_module_refs_migrated && retained_rules == 0;
    let moved_keyframes = keyframes
        .iter()
        .filter(|keyframe| {
            remove_at_rules
                || candidates
                    .iter()
                    .any(|candidate| candidate.contains(&keyframe.migrated_name))
        })
        .collect::<Vec<_>>();
    let moved_global_at_rules = if remove_at_rules {
        global_at_rules.iter().collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if remove_at_rules {
        css_edits.extend(keyframes.iter().map(|keyframe| Edit {
            start: keyframe.span.start,
            end: keyframe.span.end,
            replacement: String::new(),
        }));
        css_edits.extend(global_at_rules.iter().map(|at_rule| Edit {
            start: at_rule.span.start,
            end: at_rule.span.end,
            replacement: String::new(),
        }));
    }
    if (!moved_keyframes.is_empty() || !moved_global_at_rules.is_empty())
        && let Some((tailwind_path, tailwind_source)) = request
            .tailwind_path
            .as_ref()
            .zip(request.tailwind_source.as_ref())
    {
        let source = append_keyframes(tailwind_source, &moved_keyframes)?;
        let source = append_global_at_rules(&source, &moved_global_at_rules)?;
        validate_css(&source)?;
        if source != *tailwind_source {
            planned_files.push(PlannedFile {
                path: tailwind_path.clone(),
                source,
            });
        }
    }

    let stylesheet_changed = !css_edits.is_empty();
    let mut deleted_files = Vec::new();
    if stylesheet_changed {
        let source = apply_edits(&request.css_source, css_edits)?;
        let source = if is_module {
            remove_empty_conditionals(source, request.syntax.parser_syntax())?
        } else {
            source
        };
        validate_stylesheet(&source, request.syntax.parser_syntax())?;
        if is_module && source.trim().is_empty() {
            deleted_files.push(request.css_path.clone());
        } else {
            planned_files.push(PlannedFile {
                path: request.css_path.clone(),
                source,
            });
        }
    }

    let css_module_deleted = deleted_files.contains(&request.css_path);
    let module_import_is_unused =
        is_module && module_references_safe && all_module_refs_migrated && retained_rules == 0;
    for (file, mut result) in source_plans {
        if css_module_deleted || module_import_is_unused {
            result.edits.append(&mut result.removable_import_edits);
        }
        if !result.edits.is_empty() {
            let source = apply_edits(&file.source, result.edits)?;
            if Path::new(&file.path)
                .extension()
                .is_none_or(|extension| extension != "html")
            {
                validate_js(&file.path, &source)?;
            }
            planned_files.push(PlannedFile {
                path: file.path.clone(),
                source,
            });
        }
    }

    if stylesheet_changed
        && matches!(
            request.syntax,
            StylesheetSyntax::Scss | StylesheetSyntax::Sass | StylesheetSyntax::Less
        )
    {
        warnings.push(Warning {
            code: "rebuild-required",
            file: request.css_path.clone(),
            start: 0,
            end: 0,
            message: "Rebuild this preprocessor entry to refresh its generated CSS.".to_string(),
        });
    }

    Ok(PlanResponse {
        files: planned_files,
        deleted_files,
        unlinked_files: if module_import_is_unused {
            vec![request.css_path]
        } else {
            Vec::new()
        },
        candidates: candidates.into_iter().collect(),
        converted_rules,
        retained_rules,
        rules: rule_reports,
        warnings,
    })
}

fn merge_counts(target: &mut HashMap<String, usize>, source: &HashMap<String, usize>) {
    for (key, count) in source {
        *target.entry(key.clone()).or_default() += *count;
    }
}

fn apply_edits(source: &str, mut edits: Vec<Edit>) -> Result<String, String> {
    edits.sort_by_key(|edit| (edit.start, edit.end));
    for pair in edits.windows(2) {
        if pair[0].end > pair[1].start {
            return Err("Overlapping source edits were produced".to_string());
        }
    }
    let mut output = source.to_string();
    for edit in edits.into_iter().rev() {
        if edit.end > output.len() || edit.start > edit.end {
            return Err("Invalid source edit span".to_string());
        }
        output.replace_range(edit.start..edit.end, &edit.replacement);
    }
    Ok(output)
}

fn remove_empty_conditionals(mut source: String, syntax: Syntax) -> Result<String, String> {
    loop {
        let allocator = oxc_css_parser::Allocator::default();
        let mut parser = CssParser::new(&allocator, &source, syntax);
        let stylesheet = parser
            .parse::<Stylesheet>()
            .map_err(|error| format!("Failed to parse edited CSS: {error:?}"))?;
        let mut edits = Vec::new();
        collect_empty_conditionals(&stylesheet.statements, &mut edits);
        if edits.is_empty() {
            return Ok(source);
        }
        source = apply_edits(&source, edits)?;
    }
}

fn collect_empty_conditionals(statements: &[Statement<'_>], edits: &mut Vec<Edit>) {
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

fn validate_stylesheet(source: &str, syntax: Syntax) -> Result<(), String> {
    let allocator = oxc_css_parser::Allocator::default();
    CssParser::new(&allocator, source, syntax)
        .parse::<Stylesheet>()
        .map(|_| ())
        .map_err(|error| format!("Edited stylesheet no longer parses: {error:?}"))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use super::{SourceFile, apply_edits, plan_batch_json, plan_json};
    use crate::animations::{KeyframePlan, animation_candidate, append_keyframes};
    use crate::css_plan::SelectorKey;
    use crate::js_rewrite::plan_batch_source_file;
    use crate::utilities::{
        css_properties_conflict, declaration_to_candidate, tailwind_utilities_conflict,
    };

    #[test]
    fn parses_indented_sass_with_explicit_module_metadata() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.sass",
            "cssSource": ".button\n  padding: 13px\n",
            "syntax": "sass",
            "isModule": true,
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.sass';\nexport const Button = () => <button className={styles.button} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
        assert_eq!(response["convertedRules"], 1);
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Button.module.sass"])
        );
    }

    #[test]
    fn retains_scss_values_that_require_semantic_evaluation() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.scss",
            "cssSource": "$space: 13px;\n.button { padding: $space; }\n",
            "syntax": "scss",
            "isModule": true,
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.scss';\nexport const Button = () => <button className={styles.button} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["candidates"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 1);
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| { warning["code"] == "unsupported-declaration" })
        );
    }

    #[test]
    fn appends_a_global_class_and_retains_the_rule() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = () => <div className='card' />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 1);
        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <div className=\"card p-[13px]\" />;\n"
        );
        assert_eq!(response["warnings"][0]["code"], "retained-global-rule");
    }

    #[test]
    fn ignores_side_effect_imports_for_global_css() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import './global.css';\nexport const Card = () => <div className='card' />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|warning| { warning["code"] == "retained-global-rule" })
        );
    }

    #[test]
    fn does_not_duplicate_a_dynamic_global_class_name() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": "#hero { height: 100vh; }\n",
            "files": [{
                "path": "/project/Hero.tsx",
                "source": "export const Hero = () => <main id=\"hero\" className={getClass()} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        let codes = response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|warning| warning["code"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(codes, ["dynamic-class-name", "retained-global-rule"]);
    }

    #[test]
    fn does_not_flag_module_members_as_dynamic_for_global_css() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        let codes = response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|warning| warning["code"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(codes, ["retained-global-rule"]);
    }

    #[test]
    fn migrates_a_global_expression_string_literal_class_name() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = () => <div className={'card'} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <div className=\"card p-[13px]\" />;\n"
        );
        assert_eq!(response["warnings"][0]["code"], "retained-global-rule");
    }

    #[test]
    fn migrates_a_global_static_template_class_name() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".card { padding: 13px; }\n.featured { margin: 7px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = () => <div className={`card featured`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <div className=\"card featured p-[13px] m-[7px]\" />;\n"
        );
    }

    #[test]
    fn warns_on_an_unsupported_global_class_name_expression() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".a { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = ({ maybe }) => <div className={maybe ? 'a' : 'b'} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        let dynamic = response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|warning| warning["code"] == "dynamic-class-name")
            .unwrap();
        assert_eq!(dynamic["file"], "/project/Card.tsx");
    }

    #[test]
    fn quotes_a_global_candidate_containing_double_quotes() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": "#hero { content: \"\\\"\"; }\n",
            "files": [{
                "path": "/project/Hero.tsx",
                "source": "export const Hero = () => <main id=\"hero\" />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["files"][0]["source"],
            "export const Hero = () => <main id=\"hero\" className='[content:\"\\\"\"]' />;\n"
        );
    }

    #[test]
    fn a_second_run_over_a_migrated_global_expression_literal_is_a_no_op() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = () => <div className=\"card p-[13px]\" />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
    }

    #[test]
    fn keeps_a_module_reference_when_a_sibling_rule_is_unsupported() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n.card::before { content: 'x'; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
    }

    #[test]
    fn keeps_a_module_import_when_any_rule_is_retained() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n.other { display: grid; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();
        let source = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/Card.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();

        assert!(source.contains("import styles from './Card.module.css'"));
        assert!(source.contains("className=\"p-[13px]\""));
        assert_eq!(response["convertedRules"], 1);
        assert_eq!(response["retainedRules"], 1);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
    }

    #[test]
    fn rewrites_the_target_selector_even_when_its_name_recurs() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".a:not(.abc) { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = () => <div className='a' />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["[&:not(.abc)]:p-[13px]"])
        );
    }

    #[test]
    fn rewrites_the_last_compound_of_a_descendant_selector_by_span() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".foo .a:not(.abc) { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = () => <div className='a' />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["[.foo_&:not(.abc)]:p-[13px]"])
        );
    }

    #[test]
    fn keeps_only_the_last_duplicate_declaration() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { color: red; color: blue; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["candidates"], serde_json::json!(["text-[blue]"]));
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn arbitrary_properties_conflict_with_named_utilities() {
        assert!(tailwind_utilities_conflict("[display:block]", "hidden"));
        assert!(tailwind_utilities_conflict("[padding:8px]", "p-2"));
        assert!(tailwind_utilities_conflict("[margin-top:4px]", "mt-2"));
        assert!(tailwind_utilities_conflict("[width:10rem]", "w-4"));
        assert!(tailwind_utilities_conflict("md:[display:block]", "md:flex"));
        assert!(!tailwind_utilities_conflict("[display:block]", "p-2"));
        assert!(!tailwind_utilities_conflict("[opacity:1]", "hidden"));
        assert!(!tailwind_utilities_conflict("[display:block]", "sm:hidden"));
    }

    #[test]
    fn tier1_arbitrary_properties_conflict_with_named_utilities() {
        assert!(tailwind_utilities_conflict("[position:absolute]", "static"));
        assert!(tailwind_utilities_conflict("[top:0]", "top-[0]"));
        assert!(tailwind_utilities_conflict("[inset:0]", "left-2"));
        assert!(tailwind_utilities_conflict("[overflow:hidden]", "overflow-x-auto"));
        assert!(tailwind_utilities_conflict("[overflow-y:auto]", "overflow-hidden"));
        assert!(tailwind_utilities_conflict("[z-index:30]", "z-30"));
        assert!(tailwind_utilities_conflict("[opacity:0.5]", "opacity-50"));
        assert!(tailwind_utilities_conflict("[font-weight:700]", "font-bold"));
        assert!(tailwind_utilities_conflict("[line-height:1.15]", "leading-tight"));
        assert!(tailwind_utilities_conflict(
            "[letter-spacing:0.2em]",
            "tracking-wide"
        ));
        assert!(tailwind_utilities_conflict("[text-align:center]", "text-left"));
        assert!(tailwind_utilities_conflict("[flex-direction:row]", "flex-col"));
        assert!(tailwind_utilities_conflict("[align-items:center]", "items-start"));
        assert!(tailwind_utilities_conflict(
            "[justify-content:center]",
            "justify-between"
        ));
        assert!(tailwind_utilities_conflict("[flex-wrap:wrap]", "flex-nowrap"));
        assert!(tailwind_utilities_conflict("[border-width:2px]", "border-2"));
        assert!(tailwind_utilities_conflict("[border-style:solid]", "border-dashed"));
        assert!(tailwind_utilities_conflict("[border-color:red]", "border-red-500"));
        assert!(tailwind_utilities_conflict("[min-width:1rem]", "min-w-4"));
        assert!(tailwind_utilities_conflict("[max-height:1rem]", "max-h-4"));
        assert!(!tailwind_utilities_conflict("[overflow-x:auto]", "overflow-y-hidden"));
        assert!(!tailwind_utilities_conflict("[border-width:2px]", "border-dashed"));
        assert!(!tailwind_utilities_conflict("[top:0]", "left-2"));
        assert!(!tailwind_utilities_conflict("[z-index:30]", "opacity-50"));
    }

    #[test]
    fn maps_tier1_families_to_exact_or_arbitrary_candidates() {
        let tokens = HashMap::from(
            [
                ("spacing", "0.25rem"),
                ("leading-tight", "1.25"),
                ("tracking-wide", "0.025em"),
                ("font-weight-normal", "400"),
                ("font-weight-bold", "700"),
                ("container-sm", "24rem"),
                ("color-brand", "#123456"),
            ]
            .map(|(name, value)| (name.to_string(), value.to_string())),
        );
        let cases = [
            ("position", "absolute", "absolute"),
            ("position", "sticky", "sticky"),
            ("position", "inherit", "[position:inherit]"),
            ("z-index", "30", "z-30"),
            ("z-index", "auto", "z-auto"),
            ("z-index", "-1", "z-[-1]"),
            ("opacity", "50%", "opacity-50"),
            ("opacity", "0.5", "opacity-[0.5]"),
            ("font-weight", "700", "font-bold"),
            ("font-weight", "bold", "font-bold"),
            ("font-weight", "550", "font-[550]"),
            ("font-weight", "lighter", "[font-weight:lighter]"),
            ("line-height", "1.25", "leading-tight"),
            ("line-height", "1.15", "leading-[1.15]"),
            ("letter-spacing", "0.025em", "tracking-wide"),
            ("letter-spacing", "0.2em", "tracking-[0.2em]"),
            ("text-align", "center", "text-center"),
            ("text-align", "start", "[text-align:start]"),
            ("flex-direction", "column", "flex-col"),
            ("flex-direction", "inherit", "[flex-direction:inherit]"),
            ("align-items", "flex-start", "items-start"),
            ("align-items", "start", "[align-items:start]"),
            ("justify-content", "space-between", "justify-between"),
            ("justify-content", "left", "[justify-content:left]"),
            ("flex-wrap", "wrap", "flex-wrap"),
            ("flex-wrap", "inherit", "[flex-wrap:inherit]"),
            ("border-width", "1px", "border"),
            ("border-width", "2px", "border-2"),
            ("border-width", "thin", "[border-width:thin]"),
            ("border-color", "#123456", "border-brand"),
            ("border-color", "red", "border-[red]"),
            ("border-style", "dashed", "border-dashed"),
            ("border-style", "groove", "[border-style:groove]"),
            ("min-width", "24rem", "min-w-sm"),
            ("min-width", "0.5rem", "min-w-2"),
            ("min-width", "13px", "min-w-[13px]"),
            ("max-width", "24rem", "max-w-sm"),
            ("max-height", "0.5rem", "max-h-2"),
            ("max-height", "13px", "max-h-[13px]"),
            ("min-height", "13px", "min-h-[13px]"),
        ];
        for (property, value, expected) in cases {
            assert_eq!(
                declaration_to_candidate(property, value, &tokens).as_deref(),
                Ok(expected),
                "{property}: {value}"
            );
        }
    }

    #[test]
    fn normalizes_inset_shorthand_before_mapping() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { inset: 0; left: 2rem; }\n",
            "themeTokens": { "spacing": "0.25rem" },
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["bottom-[0]", "left-8", "right-[0]", "top-[0]"])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn normalizes_overflow_shorthand_before_mapping() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { overflow: hidden; overflow-x: auto; }\n.note { overflow: hidden auto; }\n",
            "files": [
                {
                    "path": "/project/Card.tsx",
                    "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.note} /></div>;\n"
                }
            ]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!([
                "overflow-x-auto",
                "overflow-x-hidden",
                "overflow-y-auto",
                "overflow-y-hidden"
            ])
        );
        assert_eq!(response["convertedRules"], 2);
    }

    #[test]
    fn collapses_an_equal_overflow_pair_into_the_shorthand_utility() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { overflow-x: hidden; overflow-y: hidden; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["overflow-hidden"])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn maps_tier1_static_keywords_in_a_rule() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { position: absolute; text-align: center; overflow: auto; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["absolute", "overflow-auto", "text-center"])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn retains_a_module_referenced_by_another_stylesheet() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "cssDependents": ["/project/Consumer.module.css"],
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(
                    |warning| warning["code"] == "unsupported-css-module-reference"
                        && warning["file"] == "/project/Consumer.module.css"
                )
        );
    }

    #[test]
    fn retains_rules_when_combined_template_members_conflict() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".a { padding: 8px; }\n.b { padding: 4px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`${styles.a} ${styles.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert_eq!(response["files"], serde_json::json!([]));
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "module-utilities-conflict")
        );
    }

    #[test]
    fn leaves_a_template_without_module_members_untouched() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`marketing-card`}><span className={styles.card} /></div>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();
        let source = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/Card.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();

        assert!(source.contains("className={`marketing-card`}"));
        assert!(source.contains("className=\"p-[13px]\""));
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn retains_a_module_required_from_commonjs() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }, {
                "path": "/project/legacy.cjs",
                "source": "const styles = require('./Card.module.css');\nmodule.exports = styles;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "unsupported-css-module-reference")
        );
    }

    #[test]
    fn retains_a_module_loaded_with_dynamic_import() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }, {
                "path": "/project/lazy.ts",
                "source": "export const load = () => import('./Card.module.css');\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
    }

    #[test]
    fn warns_and_retains_reexported_css_modules() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }, {
                "path": "/project/index.ts",
                "source": "export { default as cardStyles } from './Card.module.css';\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "unsupported-css-module-reference")
        );
    }

    #[test]
    fn retains_repeated_selector_rules_with_overlapping_utilities() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 8px; }\n.card { padding: 4px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
        assert_eq!(response["files"], serde_json::json!([]));
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|warning| warning["code"] == "unsupported-overlap")
        );
    }

    #[test]
    fn converts_an_exact_media_breakpoint() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": "@media (min-width: 48rem) { .card { padding: 13px; } }\n",
            "themeTokens": { "breakpoint-md": "48rem" },
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = () => <div className=\"card\" />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["candidates"], serde_json::json!(["md:p-[13px]"]));
        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <div className=\"card md:p-[13px]\" />;\n"
        );
    }

    #[test]
    fn converts_an_exact_media_breakpoint_range() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": "@media (min-width: 48rem) and (max-width: 63.999rem) { .card { padding: 13px; } }\n",
            "themeTokens": {
                "breakpoint-md": "48rem",
                "breakpoint-lg": "64rem"
            },
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["md:max-lg:p-[13px]"])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn converts_an_unmatched_media_range_to_an_arbitrary_variant() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": "@media (min-width: 48rem) and (max-width: 60rem) { .card { padding: 13px; } }\n",
            "themeTokens": {
                "breakpoint-md": "48rem",
                "breakpoint-lg": "64rem"
            },
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["[@media_(min-width:48rem)_and_(max-width:60rem)]:p-[13px]"])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn converts_nested_media_and_supports_rules() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": "@media (min-width: 48rem) { .button { padding: 1rem; } @supports (display: grid) { .button { display: grid; } } }\n",
            "themeTokens": { "breakpoint-md": "48rem" },
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["md:p-[1rem]", "md:supports-[display:grid]:grid"])
        );
        assert_eq!(response["convertedRules"], 2);
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Button.module.css"])
        );
    }

    #[test]
    fn converts_tailwind_conditional_variants() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": "@media (prefers-reduced-motion: reduce) { @starting-style { @container (min-width: 28rem) { .button { display: grid; } } } }\n@media (prefers-color-scheme: dark) { .button { color: white; } }\n",
            "themeTokens": { "container-md": "28rem" },
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["dark:text-[white]", "motion-reduce:starting:@md:grid"])
        );
        assert_eq!(response["convertedRules"], 2);
    }

    #[test]
    fn escapes_literal_underscores_in_arbitrary_candidates() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": "@supports (font-tech(color_colrv1)) { .button { --font-key: Open_Sans; } }\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["supports-[font-tech(color\\_colrv1)]:[--font-key:Open\\_Sans]"])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn encodes_quoted_values_and_urls_into_arbitrary_candidates() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": ".button { background-image: url(\"a_b.png\"); font-family: \"My Font\", sans-serif; content: \"a_b\"; width: calc(min(100%, 50vw)); }\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!([
                "[background-image:url(\"a_b.png\")]",
                "[content:\"a\\_b\"]",
                "[font-family:\"My_Font\",_sans-serif]",
                "w-[calc(min(100%,_50vw))]"
            ])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn encodes_grid_line_names_into_arbitrary_candidates() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": ".button { grid-template-columns: [full-start] 1fr; }\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["[grid-template-columns:[full-start]_1fr]"])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn retains_unrepresentable_values_with_an_unsupported_value_warning() {
        // Tailwind preserves url() bodies verbatim (underscores are not
        // decoded back to spaces there), so a space inside url() cannot be
        // represented in a class attribute.
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": ".button { background-image: url(\"a b.png\"); }\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        let warnings = response["warnings"].as_array().unwrap();
        assert!(!warnings.is_empty());
        assert!(
            warnings
                .iter()
                .any(|warning| warning["code"] == "unsupported-value")
        );
    }

    #[test]
    fn converts_conditions_nested_inside_style_rules() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": ".button { opacity: 1; @starting-style { opacity: 0; } @media (prefers-reduced-motion: reduce) { display: none; } }\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!([
                "motion-reduce:hidden",
                "opacity-[1]",
                "starting:opacity-[0]"
            ])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn moves_global_definition_at_rules_to_the_tailwind_entry() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": "@property --progress { syntax: \"<number>\"; inherits: false; initial-value: 0; }\n.button { display: grid; }\n",
            "tailwindPath": "/project/globals.css",
            "tailwindSource": "@import \"tailwindcss\";\n/* @property --progress { syntax: \"<number>\"; inherits: false; initial-value: 0; } */\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();
        let tailwind = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/globals.css")
            .unwrap();

        assert_eq!(
            tailwind["source"]
                .as_str()
                .unwrap()
                .matches("@property --progress")
                .count(),
            2
        );
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Button.module.css"])
        );
    }

    #[test]
    fn retains_global_definition_at_rules_with_urls() {
        let request = serde_json::json!({
            "cssPath": "/project/components/Button.module.css",
            "cssSource": "@font-face { font-family: Custom; src: url('./custom.woff2'); }\n.button { display: grid; }\n",
            "tailwindPath": "/project/globals.css",
            "tailwindSource": "@import \"tailwindcss\";\n",
            "files": [{
                "path": "/project/components/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 1);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| { warning["code"] == "unsupported-at-rule" })
        );
    }

    #[test]
    fn moves_global_at_rules_with_stable_urls() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": "@font-face { font-family: Custom; src: url('./fonts/custom.woff2'); }\n@page { margin: 2cm; }\n.button { display: grid; }\n",
            "tailwindPath": "/project/globals.css",
            "tailwindSource": "@import \"tailwindcss\";\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();
        let tailwind = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/globals.css")
            .unwrap()["source"]
            .as_str()
            .unwrap();

        assert!(tailwind.contains("url('./fonts/custom.woff2')"));
        assert!(tailwind.contains("@page"));
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Button.module.css"])
        );
    }

    #[test]
    fn converts_named_container_queries_to_arbitrary_variants() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": "@media (min-width: 48rem) { .button { padding: 1rem; } @container card_grid (min-width: 20rem) { .button { display: grid; } } }\n",
            "themeTokens": { "breakpoint-md": "48rem" },
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!([
                "md:[@container_card\\_grid_(min-width:20rem)]:grid",
                "md:p-[1rem]"
            ])
        );
        assert_eq!(response["convertedRules"], 2);
    }

    #[test]
    fn retains_unsupported_nested_at_rules() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": "@media (min-width: 48rem) { @layer components { .button { display: grid; } } }\n",
            "themeTokens": { "breakpoint-md": "48rem" },
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert!(response["files"].as_array().unwrap().is_empty());
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| { warning["code"] == "unsupported-nested-at-rule" })
        );
    }

    fn retained_at_rule_warning(css_source: &str) -> Vec<String> {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": css_source,
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });
        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|warning| warning["code"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn retains_an_unconvertible_media_query() {
        let codes = retained_at_rule_warning(
            "@media /* screens */ (min-width: 48rem) { .button { padding: 13px; } }\n",
        );
        assert!(codes.contains(&"unsupported-media-query".to_string()));
    }

    #[test]
    fn retains_an_unconvertible_supports_query() {
        let codes = retained_at_rule_warning(
            "@supports (content: \"x\") { .button { padding: 13px; } }\n",
        );
        assert!(codes.contains(&"unsupported-supports-query".to_string()));
    }

    #[test]
    fn retains_an_unconvertible_container_query() {
        let codes = retained_at_rule_warning(
            "@container /* card */ (min-width: 20rem) { .button { padding: 13px; } }\n",
        );
        assert!(codes.contains(&"unsupported-container-query".to_string()));
    }

    #[test]
    fn converts_a_global_descendant_selector_to_an_arbitrary_variant() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".menu_open .child { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = () => <span className=\"child\" />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["[.menu\\_open_&]:p-[13px]"])
        );
        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <span className=\"child [.menu\\_open_&]:p-[13px]\" />;\n"
        );
    }

    #[test]
    fn retains_a_css_module_class_referenced_by_composes() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n.featured { composes: card; color: red; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "css-module-composes")
        );
    }

    #[test]
    fn normalizes_spacing_shorthand_before_mapping() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { margin: 1rem; margin-left: 2rem; }\n",
            "themeTokens": { "spacing": "0.25rem" },
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["mb-4", "ml-8", "mr-4", "mt-4"])
        );
    }

    #[test]
    fn preserves_functional_spacing_values() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { margin: calc(100% - 1rem); padding: var(--space, 1rem); }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["m-[calc(100%_-_1rem)]", "p-[var(--space,_1rem)]"])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn prefers_an_exact_custom_theme_token() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "themeTokens": { "spacing-card": "13px" },
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["candidates"], serde_json::json!(["p-card"]));
    }

    #[test]
    fn converts_a_supported_pseudo_class_to_a_variant() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card:hover { color: red; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["hover:text-[red]"])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn adds_a_class_name_for_a_global_id() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": "#hero { height: 100vh; }\n",
            "files": [{
                "path": "/project/Hero.tsx",
                "source": "export const Hero = () => <main id=\"hero\" />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["files"][0]["source"],
            "export const Hero = () => <main id=\"hero\" className=\"h-[100vh]\" />;\n"
        );
        assert_eq!(response["warnings"][0]["code"], "retained-global-rule");
    }

    #[test]
    fn migrates_a_static_css_module_template() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`${styles.card} featured`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <div className=\"p-[13px] featured\" />;\n"
        );
        assert_eq!(response["convertedRules"], 1);
        assert_eq!(response["warnings"], serde_json::json!([]));
    }

    #[test]
    fn distinguishes_overlapping_tailwind_properties() {
        assert!(tailwind_utilities_conflict("p-[13px]", "pl-2"));
        assert!(!tailwind_utilities_conflict("ps-2", "pe-2"));
        assert!(!tailwind_utilities_conflict("rounded-t-lg", "rounded-b-lg"));
        assert!(!tailwind_utilities_conflict("text-sm", "text-red-500"));
    }

    #[test]
    fn warns_when_a_static_template_utility_conflicts() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`${styles.card} p-2`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 1);
        assert_eq!(
            response["warnings"][0]["code"],
            "existing-tailwind-conflict"
        );
        assert_eq!(response["warnings"][0]["file"], "/project/Card.tsx");
    }

    #[test]
    fn retains_a_rule_used_through_another_import_alias() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import first from './Card.module.css';\nimport second from './Card.module.css';\nconst card = first.card;\nexport const Card = () => <div className={second.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 1);
        assert!(
            response["files"][0]["source"]
                .as_str()
                .unwrap()
                .contains("first.card")
        );
    }

    #[test]
    fn converts_references_through_every_import_alias() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import first from './Card.module.css';\nimport second from './Card.module.css';\nexport const Card = () => <><div className={first.card} /><div className={second.card} /></>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();
        let source = response["files"][0]["source"].as_str().unwrap();

        assert_eq!(response["convertedRules"], 1);
        assert!(!source.contains("import "));
        assert!(!source.contains(".card"));
    }

    #[test]
    fn retains_a_module_with_an_unclassified_import_reference() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import first from './Card.module.css';\nimport second from './Card.module.css';\nconst card = first['card'];\nexport const Card = () => <div className={second.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| { warning["code"] == "computed-css-module-reference" })
        );
    }

    #[test]
    fn warns_at_the_computed_css_module_reference_site() {
        let source = "import styles from './Card.module.css';\nexport const name = styles['card'];\n";
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": source
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        let start = source.find("styles['card']").unwrap();
        let warning = response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|warning| warning["code"] == "computed-css-module-reference")
            .expect("computed reference warning");
        assert_eq!(warning["file"], "/project/Card.tsx");
        assert_eq!(warning["start"], start);
        assert_eq!(warning["end"], start + "styles['card']".len());
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|warning| warning["code"] != "unsupported-css-module-reference")
        );
    }

    #[test]
    fn warns_at_an_aliased_css_module_reference_site() {
        let source = "import styles from './Card.module.css';\nconst card = styles.card;\nexport const Card = () => <div className={styles.button} />;\n";
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n.button { color: red; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": source
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        let start = source.find("card = styles.card").unwrap();
        let aliased = response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "aliased-css-module-reference")
            .collect::<Vec<_>>();
        assert_eq!(aliased.len(), 1);
        assert_eq!(aliased[0]["file"], "/project/Card.tsx");
        assert_eq!(aliased[0]["start"], start);
        assert_eq!(aliased[0]["end"], start + "card = styles.card".len());
    }

    #[test]
    fn warns_at_a_non_classname_css_module_reference_site() {
        let source = "import styles from './Card.module.css';\nexport const find = () => document.querySelector(`.${styles.card}`);\n";
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": source
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        let start = source.find("styles.card").unwrap();
        let warning = response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|warning| warning["code"] == "non-classname-css-module-reference")
            .expect("non-className reference warning");
        assert_eq!(warning["file"], "/project/Card.tsx");
        assert_eq!(warning["start"], start);
        assert_eq!(warning["end"], start + "styles.card".len());
    }

    #[test]
    fn parses_jsx_in_javascript_files() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.js",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 1);
        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <div className=\"p-[13px]\" />;\n"
        );
    }

    #[test]
    fn moves_local_keyframes_to_the_tailwind_entry() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n.button { animation: fade 1s; }\n",
            "tailwindPath": "/project/globals.css",
            "tailwindSource": "@import \"tailwindcss\";\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();
        let candidate = response["candidates"][0].as_str().unwrap();
        let name = candidate
            .strip_prefix("[animation:")
            .and_then(|candidate| candidate.strip_suffix("_1s]"))
            .unwrap();
        let tailwind = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/globals.css")
            .unwrap();

        assert!(name.starts_with("tw-migrate-"));
        assert!(name.ends_with("-fade"));
        assert!(
            tailwind["source"]
                .as_str()
                .unwrap()
                .contains(&format!("@keyframes {name}"))
        );
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Button.module.css"])
        );
    }

    #[test]
    fn removes_an_import_after_moving_an_at_rule_only_module() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n",
            "tailwindPath": "/project/globals.css",
            "tailwindSource": "@import \"tailwindcss\";\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();
        let source = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/Button.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();

        assert_eq!(
            source,
            "export const Button = () => <button>Save</button>;\n"
        );
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Button.module.css"])
        );
    }

    #[test]
    fn rejects_conflicting_tailwind_keyframes() {
        let keyframe = KeyframePlan {
            span: 0..0,
            name: "fade".to_string(),
            migrated_name: "tw-migrate-fade".to_string(),
            source: "@keyframes tw-migrate-fade { from { opacity: 0; } }".to_string(),
        };

        assert!(
            append_keyframes(
                "@keyframes tw-migrate-fade { from { opacity: 1; } }",
                &[&keyframe]
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_ambiguous_animation_names() {
        let keyframes = HashMap::from([("linear", "tw-migrate-linear")]);

        assert_eq!(
            animation_candidate("animation", "linear 1s", &keyframes),
            None
        );
        assert_eq!(
            animation_candidate("animation-name", "linear", &keyframes),
            Some("[animation-name:tw-migrate-linear]".to_string())
        );

        let keyframes = HashMap::from([("fade_in", "tw-migrate-fade_in")]);
        assert_eq!(
            animation_candidate("animation", "fade_in 1s", &keyframes),
            Some("[animation:tw-migrate-fade\\_in_1s]".to_string())
        );
    }

    #[test]
    fn retains_unsupported_keyframe_dependencies() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n.button { animation: fade 1s, fade 2s; }\n",
            "tailwindPath": "/project/globals.css",
            "tailwindSource": "@import \"tailwindcss\";\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| { warning["code"] == "unsupported-animation" })
        );
    }

    #[test]
    fn plans_a_direct_css_module_padding_migration() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": ".button { padding: 13px; }\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
        assert_eq!(response["convertedRules"], 1);
        assert_eq!(response["retainedRules"], 0);
        assert_eq!(
            response["files"][0]["source"],
            "export const Button = () => <button className=\"p-[13px]\">Save</button>;\n"
        );
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Button.module.css"])
        );
    }

    #[test]
    fn batch_ignores_an_unparseable_unwritable_file_without_a_reference() {
        let request = serde_json::json!({
            "stylesheets": [{
                "cssPath": "/project/Card.module.css",
                "cssSource": ".card { padding: 13px; }\n"
            }],
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }, {
                "path": "/project/coverage.js",
                "source": "<% generated: mentions other.module.css but is not JavaScript %>\n",
                "writable": false
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 1);
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Card.module.css"])
        );
    }

    #[test]
    fn batch_retains_a_module_named_by_an_unparseable_unwritable_file() {
        let request = serde_json::json!({
            "stylesheets": [{
                "cssPath": "/project/Card.module.css",
                "cssSource": ".card { padding: 13px; }\n"
            }],
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }, {
                "path": "/project/generated.js",
                "source": "<% template referencing Card.module.css %>\n",
                "writable": false
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(
                    |warning| warning["code"] == "unsupported-css-module-reference"
                        && warning["file"] == "/project/generated.js"
                )
        );
    }

    #[test]
    fn batch_updates_distinct_module_references_without_losing_edits() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { padding: 13px; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { color: red; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <><div className={a.a} /><div className={b.b} /></>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();
        let source = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/App.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();

        assert_eq!(
            source,
            "export const App = () => <><div className=\"p-[13px]\" /><div className=\"text-[red]\" /></>;\n"
        );
        assert_eq!(response["convertedRules"], 2);
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/A.module.css", "/project/B.module.css"])
        );
    }

    #[test]
    fn batch_migrates_members_from_multiple_modules_in_one_template() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { padding: 13px; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { color: red; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();
        let source = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/App.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();

        assert_eq!(
            source,
            "export const App = () => <div className=\"p-[13px] text-[red]\" />;\n"
        );
        assert_eq!(response["convertedRules"], 2);
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/A.module.css", "/project/B.module.css"])
        );
    }

    #[test]
    fn batch_blocked_rules_are_retained_silently_and_reports_carry_rule_ids() {
        let request = serde_json::json!({
            "stylesheets": [{
                "cssPath": "/project/Button.module.css",
                "cssSource": ".bad { color: red; }\n.good { padding: 13px; }\n",
                "blockedRules": [{ "start": 0, "end": 20 }]
            }],
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.bad}><i className={styles.good} /></button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 1);
        assert_eq!(response["retainedRules"], 1);
        assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        // The blocked rule is retained without a Rust-side warning: the
        // caller attributes candidate-compilation-failure itself.
        assert_eq!(response["warnings"], serde_json::json!([]));
        let rules = response["rules"].as_array().unwrap();
        let blocked = rules.iter().find(|rule| rule["selector"] == ".bad").unwrap();
        assert_eq!(blocked["status"], "retained");
        assert_eq!(blocked["file"], "/project/Button.module.css");
        assert_eq!(blocked["ruleId"], serde_json::json!({ "start": 0, "end": 20 }));
        assert_eq!(blocked["authoredSpan"], serde_json::json!({ "start": 0, "end": 20 }));
        let converted = rules.iter().find(|rule| rule["selector"] == ".good").unwrap();
        assert_eq!(converted["status"], "converted");
        let source = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/Button.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();
        assert!(source.contains("styles.bad"));
        assert!(source.contains("\"p-[13px]\""));
    }

    #[test]
    fn batch_retains_conflicting_members_from_multiple_modules_in_one_template() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { padding: 8px; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { padding: 16px; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
        assert_eq!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
                .count(),
            2
        );
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|warning| warning["code"] != "dynamic-class-name")
        );
    }

    #[test]
    fn batch_retains_same_css_property_even_when_tailwind_prefix_is_ambiguous() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { color: red; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { color: blue; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
        assert_eq!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
                .count(),
            2
        );
    }

    #[test]
    fn batch_does_not_conflict_color_with_font_size() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { color: red; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { font-size: 13px; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();
        let source = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/App.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();

        assert_eq!(
            source,
            "export const App = () => <div className=\"text-[red] text-[13px]\" />;\n"
        );
        assert_eq!(response["convertedRules"], 2);
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|warning| warning["code"] != "batch-stylesheet-conflict")
        );
    }

    #[test]
    fn batch_keeps_dynamic_template_warnings() {
        let request = serde_json::json!({
            "stylesheets": [{
                "cssPath": "/project/A.module.css",
                "cssSource": ".a { padding: 8px; }\n"
            }],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nexport const App = ({ active }) => <div className={`${a.a} ${active ? 'on' : 'off'}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "dynamic-class-name")
        );
    }

    #[test]
    fn batch_blocks_only_the_conflicting_rule_for_a_shared_selector_key() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/a.css",
                    "cssSource": ".a { padding: 8px; }\n.a:hover { color: red; }\n"
                },
                {
                    "cssPath": "/project/b.css",
                    "cssSource": ".b { padding: 16px; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "export const App = () => <div className=\"a b\" />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();
        let source = response["files"][0]["source"].as_str().unwrap();

        assert_eq!(
            source,
            "export const App = () => <div className=\"a b hover:text-[red]\" />;\n"
        );
        assert_eq!(
            response["candidates"],
            serde_json::json!(["hover:text-[red]"])
        );
        assert_eq!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
                .count(),
            2
        );
    }

    #[test]
    fn batch_preserves_a_direct_module_member_when_appending_independent_candidates() {
        let file = SourceFile {
            path: "/project/App.tsx".to_string(),
            source: "import styles from './A.module.css';\nexport const App = () => <div className={styles.a} />;\n".to_string(),
            writable: true,
            html_elements: Vec::new(),
            html_stylesheets: Vec::new(),
            html_references_safe: true,
            html_script_text: String::new(),
        };
        let candidates = HashMap::from([(
            SelectorKey::Class("a".to_string()),
            vec!["hover:text-[red]".to_string()],
        )]);
        let preserved = BTreeSet::from(["a".to_string()]);

        let plan = plan_batch_source_file(
            &file,
            "/project/A.module.css",
            true,
            &candidates,
            &preserved,
        )
        .unwrap();
        let source = apply_edits(&file.source, plan.edits).unwrap();

        assert_eq!(
            source,
            "import styles from './A.module.css';\nexport const App = () => <div className={`${styles.a}${\" hover:text-[red]\"}`} />;\n"
        );
        assert_eq!(plan.matched_module_refs.get("a"), Some(&1));
    }

    #[test]
    fn batch_retains_arbitrary_border_shorthand_conflicts() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { border: 1px solid red; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { border-color: blue; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
        assert_eq!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
                .count(),
            2
        );
    }

    #[test]
    fn batch_retains_mask_shorthand_conflicts() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { mask: url(a.svg); }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { mask-image: url(b.svg); }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
    }

    #[test]
    fn batch_retains_all_reset_conflicts() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { all: unset; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { color: blue; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
        assert_eq!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
                .count(),
            2
        );
    }

    #[test]
    fn all_reset_excludes_css_wide_exceptions() {
        assert!(!css_properties_conflict("all", "--theme-color"));
        assert!(!css_properties_conflict("all", "direction"));
        assert!(!css_properties_conflict("all", "unicode-bidi"));
        assert!(css_properties_conflict("all", "color"));
    }

    #[test]
    fn batch_retains_grid_shorthand_conflicts() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { grid: auto / 1fr; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { grid-template-columns: 2fr; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
        assert_eq!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
                .count(),
            2
        );
    }

    #[test]
    fn batch_does_not_conflict_unrelated_border_radius_and_color() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { border-radius: 13px; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { border-color: blue; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();
        let app = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/App.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();

        assert_eq!(
            app,
            "export const App = () => <div className=\"rounded-[13px] border-[blue]\" />;\n"
        );
        assert_eq!(response["convertedRules"], 2);
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|warning| warning["code"] != "batch-stylesheet-conflict")
        );
    }

    #[test]
    fn batch_converts_independent_module_rules_while_preserving_conflicting_members() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { padding: 8px; }\n.a:hover { color: red; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { padding: 16px; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();
        let app = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/App.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();
        let css = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/A.module.css")
            .unwrap()["source"]
            .as_str()
            .unwrap();

        assert_eq!(
            app,
            "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}${\" hover:text-[red]\"}`} />;\n"
        );
        assert_eq!(css, ".a { padding: 8px; }\n\n");
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 1);
        assert_eq!(response["retainedRules"], 2);
        assert_eq!(
            response["candidates"],
            serde_json::json!(["hover:text-[red]"])
        );
    }

    #[test]
    fn batch_converts_a_different_module_class_when_one_class_conflicts() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { padding: 8px; }\n.c { color: red; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { padding: 16px; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <><div className={`${a.a} ${b.b}`} /><div className={a.c} /></>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();
        let app = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/App.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();
        let css = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/A.module.css")
            .unwrap()["source"]
            .as_str()
            .unwrap();

        assert_eq!(
            app,
            "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <><div className={`${a.a} ${b.b}`} /><div className=\"text-[red]\" /></>;\n"
        );
        assert_eq!(css, ".a { padding: 8px; }\n\n");
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 1);
        assert_eq!(response["retainedRules"], 2);
        assert_eq!(response["candidates"], serde_json::json!(["text-[red]"]));
    }

    #[test]
    fn batch_retains_cross_stylesheet_conflicts() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/a.css",
                    "cssSource": ".a { padding: 8px; }\n"
                },
                {
                    "cssPath": "/project/b.css",
                    "cssSource": ".b { padding: 16px; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "export const App = () => <div className=\"a b\" />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
        let conflict_files = response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
            .map(|warning| warning["file"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            conflict_files,
            BTreeSet::from(["/project/a.css", "/project/b.css"])
        );
        for warning in response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
        {
            let message = warning["message"].as_str().unwrap();
            assert!(message.contains("p-[8px]"));
            assert!(message.contains("p-[16px]"));
            assert!(message.contains("conflict"));
        }
    }

    #[test]
    fn batch_uses_candidate_specific_properties_for_font_size_conflicts() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": "@supports (display: grid) { .a { color: red; font-size: 12px; } }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": "@supports (display: grid) { .b { font-size: 13px; } }\n"
                }
            ],
            "utilityPrefix": "tw",
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();
        let messages = response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
            .map(|warning| warning["message"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(messages.len(), 2);
        assert!(
            messages
                .iter()
                .all(|message| message.contains("tw:supports-[display:grid]:text-[12px]"))
        );
        assert!(
            messages
                .iter()
                .all(|message| message.contains("tw:supports-[display:grid]:text-[13px]"))
        );
        assert!(
            messages
                .iter()
                .all(|message| !message.contains("text-[red]"))
        );
    }

    #[test]
    fn batch_uses_candidate_specific_properties_for_color_conflicts() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { color: red; font-size: 12px; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { color: blue; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={`${a.a} ${b.b}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();
        let messages = response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|warning| warning["code"] == "batch-stylesheet-conflict")
            .map(|warning| warning["message"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(messages.len(), 2);
        assert!(
            messages
                .iter()
                .all(|message| message.contains("text-[red]"))
        );
        assert!(
            messages
                .iter()
                .all(|message| message.contains("text-[blue]"))
        );
        assert!(
            messages
                .iter()
                .all(|message| !message.contains("text-[12px]"))
        );
    }

    #[test]
    fn batch_merges_properties_when_candidates_deduplicate() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { color: var(--value); font-size: var(--value); }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".b { color: blue; }\n"
                },
                {
                    "cssPath": "/project/C.module.css",
                    "cssSource": ".c { font-size: 13px; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nimport c from './C.module.css';\nexport const App = () => <div className={`${a.a} ${b.b} ${c.c}`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();
        let message = response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|warning| {
                warning["code"] == "batch-stylesheet-conflict"
                    && warning["file"] == "/project/A.module.css"
            })
            .unwrap()["message"]
            .as_str()
            .unwrap();

        assert!(message.contains("text-[var(--value)]"));
        assert!(message.contains("text-[blue]"));
        assert!(message.contains("text-[13px]"));
    }

    #[test]
    fn batch_combines_tailwind_entry_additions() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n.a { animation: fade 1s; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": "@keyframes spin { from { rotate: 0deg; } to { rotate: 360deg; } }\n.b { animation: spin 1s; }\n"
                }
            ],
            "tailwindPath": "/project/globals.css",
            "tailwindSource": "@import \"tailwindcss\";\n",
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <><div className={a.a} /><div className={b.b} /></>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();
        let tailwind = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/globals.css")
            .unwrap()["source"]
            .as_str()
            .unwrap();

        assert_eq!(tailwind.matches("@keyframes tw-migrate-").count(), 2);
    }

    #[test]
    fn batch_reference_only_consumer_prevents_module_deletion() {
        let request = serde_json::json!({
            "stylesheets": [{
                "cssPath": "/project/shared/Button.module.css",
                "cssSource": ".button { padding: 13px; }\n"
            }],
            "files": [{
                "path": "/project/app/Button.tsx",
                "source": "import styles from '../shared/Button.module.css';\nexport const Button = () => <button className={styles.button} />;\n",
                "writable": false
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 1);
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "reference-only-css-module-consumer")
        );
    }

    fn warning_message(response: &serde_json::Value, code: &str) -> String {
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|warning| warning["code"] == code)
            .unwrap_or_else(|| panic!("missing warning {code}: {:?}", response["warnings"]))
            ["message"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn retains_an_unproven_module_relationship_with_a_site_hint() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card > .title { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\nexport const Loose = () => <span className={styles.title} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 1);
        let message = warning_message(&response, "unproven-css-module-relationship");
        assert!(message.contains("/project/Card.tsx"), "{message}");
    }

    #[test]
    fn retains_a_module_relationship_behind_a_conditional_return() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card .title { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nfunction Title(props) {\n  if (props.compact) {\n    return <span className={styles.title} />;\n  }\n  return <span className={styles.title} />;\n}\nexport const Card = () => <div className={styles.card}><Title /></div>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["retainedRules"], 1);
        let message = warning_message(&response, "unproven-css-module-relationship");
        assert!(message.contains("conditional-return"), "{message}");
    }

    #[test]
    fn retains_a_module_relationship_used_inside_an_export_class() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card .title { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\nexport class Legacy {\n  render() {\n    return <span className={styles.title} />;\n  }\n}\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 1);
        let message = warning_message(&response, "unproven-css-module-relationship");
        assert!(message.contains("dynamic-content-boundary"), "{message}");
    }

    #[test]
    fn retains_a_module_relationship_behind_a_hoc() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card .title { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nimport { withTheme } from './theme';\nfunction Title() {\n  return <span className={styles.title} />;\n}\nconst Fancy = withTheme(Title);\nexport const Card = () => <div className={styles.card}><Fancy /></div>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["retainedRules"], 1);
        let message = warning_message(&response, "unproven-css-module-relationship");
        assert!(message.contains("hoc-or-dynamic-component"), "{message}");
    }

    #[test]
    fn batch_retains_a_proven_relationship_with_a_reference_only_target_usage() {
        let request = serde_json::json!({
            "stylesheets": [{
                "cssPath": "/project/Card.module.css",
                "cssSource": ".card > .title { padding: 13px; }\n"
            }],
            "files": [
                {
                    "path": "/project/Card.tsx",
                    "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\n"
                },
                {
                    "path": "/project/Extra.tsx",
                    "source": "import styles from './Card.module.css';\nexport const Extra = () => <div className={styles.card}><span className={styles.title} /></div>;\n",
                    "writable": false
                }
            ]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 1);
        let message = warning_message(&response, "unproven-css-module-relationship");
        assert!(message.contains("/project/Extra.tsx"), "{message}");
    }

    #[test]
    fn converts_a_proven_child_relationship_in_the_same_file() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { display: flex; }\n.card > .title { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title}>t</span></div>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 2);
        assert_eq!(response["retainedRules"], 0);
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Card.module.css"])
        );
        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <div className=\"flex\"><span className=\"p-[13px]\">t</span></div>;\n"
        );
    }

    #[test]
    fn converts_a_proven_relationship_through_an_imported_component() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { display: flex; }\n.card .title { padding: 13px; }\n",
            "files": [
                {
                    "path": "/project/Card.tsx",
                    "source": "import styles from './Card.module.css';\nimport Title from './Title';\nexport const Card = () => <div className={styles.card}><Title /></div>;\n"
                },
                {
                    "path": "/project/Title.tsx",
                    "source": "import styles from './Card.module.css';\nexport default function Title() {\n  return <h1 className={styles.title} />;\n}\n"
                }
            ]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 2);
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Card.module.css"])
        );
        let title = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/Title.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();
        assert!(title.contains("className=\"p-[13px]\""), "{title}");
        assert!(!title.contains("Card.module.css"), "{title}");
    }

    #[test]
    fn converts_a_target_pseudo_state_on_a_proven_relationship() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { display: flex; }\n.card .title:hover { color: red; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 2);
        assert_eq!(
            response["candidates"],
            serde_json::json!(["flex", "hover:text-[red]"])
        );
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Card.module.css"])
        );
    }

    #[test]
    fn converts_a_proven_three_compound_chain() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { display: flex; }\n.card > .list { margin: 1px; }\n.card .list > .item { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><ul className={styles.list}><li className={styles.item} /></ul></div>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 3);
        assert_eq!(response["retainedRules"], 0);
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/Card.module.css"])
        );
        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <div className=\"flex\"><ul className=\"m-[1px]\"><li className=\"p-[13px]\" /></ul></div>;\n"
        );
    }

    #[test]
    fn retains_an_ancestor_state_relationship() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card:hover .title { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 1);
        let message = warning_message(&response, "unproven-css-module-relationship");
        assert!(message.contains("Ancestor-state"), "{message}");
    }

    #[test]
    fn batch_proves_relationships_against_the_request_snapshot() {
        let request = serde_json::json!({
            "stylesheets": [
                {
                    "cssPath": "/project/A.module.css",
                    "cssSource": ".a { padding: 13px; }\n"
                },
                {
                    "cssPath": "/project/B.module.css",
                    "cssSource": ".card { display: flex; }\n.card > .title { color: red; }\n"
                }
            ],
            "files": [{
                "path": "/project/App.tsx",
                "source": "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <div className={a.a}><div className={b.card}><span className={b.title} /></div></div>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 3);
        assert_eq!(response["retainedRules"], 0);
        assert_eq!(
            response["deletedFiles"],
            serde_json::json!(["/project/A.module.css", "/project/B.module.css"])
        );
        assert_eq!(
            response["files"][0]["source"],
            "export const App = () => <div className=\"p-[13px]\"><div className=\"flex\"><span className=\"text-[red]\" /></div></div>;\n"
        );
    }

    #[test]
    fn batch_keeps_the_module_when_a_sibling_relationship_is_unproven() {
        let request = serde_json::json!({
            "stylesheets": [{
                "cssPath": "/project/Card.module.css",
                "cssSource": ".card { display: flex; }\n.card > .title { padding: 13px; }\n.card .loose { margin: 1px; }\n"
            }],
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card}><span className={styles.title} /></div>;\nexport const Loose = () => <i className={styles.loose} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 1);
        assert_eq!(response["retainedRules"], 2);
        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        let card = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/Card.tsx")
            .unwrap()["source"]
            .as_str()
            .unwrap();
        assert!(card.contains("import styles from './Card.module.css'"), "{card}");
        assert!(card.contains("className={styles.card}"), "{card}");
        assert!(card.contains("className={styles.loose}"), "{card}");
        assert!(card.contains("className=\"p-[13px]\""), "{card}");
        let css = response["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "/project/Card.module.css")
            .unwrap()["source"]
            .as_str()
            .unwrap();
        assert!(!css.contains(".title"), "{css}");
        assert!(css.contains(".card {"), "{css}");
        assert!(css.contains(".loose"), "{css}");
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "unproven-css-module-relationship")
        );
    }

    #[test]
    fn batch_unused_reference_only_import_prevents_module_deletion() {
        let request = serde_json::json!({
            "stylesheets": [{
                "cssPath": "/project/shared/Button.module.css",
                "cssSource": ".button { padding: 13px; }\n"
            }],
            "files": [
                {
                    "path": "/project/shared/Button.tsx",
                    "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button} />;\n"
                },
                {
                    "path": "/project/app/Unused.tsx",
                    "source": "import styles from '../shared/Button.module.css';\nexport const unused = true;\n",
                    "writable": false
                }
            ]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["deletedFiles"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 1);
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "reference-only-css-module-consumer")
        );
    }

    #[test]
    fn retains_an_unreferenced_module_rule_with_unresolved_selector_target() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".unused { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["retainedRules"], 1);
        assert_eq!(
            warning_message(&response, "unresolved-selector-target"),
            "No exclusively supported className references were found."
        );
    }

    #[test]
    fn warning_codes_are_pinned_to_the_readme() {
        let readme = include_str!("../../../README.md");
        let documented = readme
            .lines()
            .filter_map(|line| line.strip_prefix("| `")?.split('`').next())
            .collect::<Vec<_>>();
        assert_eq!(
            documented,
            super::WARNING_CODES,
            "the README warning table must list exactly the emitted codes, sorted"
        );

        // Strip the canonical list itself so it cannot satisfy its own check.
        let planner = include_str!("planner.rs");
        let const_start = planner.find("const WARNING_CODES").unwrap();
        let const_end = const_start + planner[const_start..].find("];").unwrap();
        // Scan every crate source and every repo-root JS file so a new module
        // cannot silently escape the pinning check.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sources = format!("{}\n{}", &planner[..const_start], &planner[const_end..]);
        for (dir, extension) in [(manifest.join("src"), "rs"), (manifest.join("../.."), "js")] {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|ext| ext == extension)
                    && path.file_name().is_some_and(|name| name != "planner.rs")
                {
                    sources.push('\n');
                    sources.push_str(&std::fs::read_to_string(path).unwrap());
                }
            }
        }
        for code in super::WARNING_CODES {
            assert!(
                sources.contains(&format!("\"{code}\"")) || sources.contains(&format!("'{code}'")),
                "documented warning code `{code}` no longer appears in the sources"
            );
        }

        // Every directly constructed warning code must be documented, whether
        // stamped as a `code:` field or passed positionally to `htmlWarning`.
        // Reason strings flowing through `rule.warning` are covered by the
        // check above plus the comment on WARNING_CODES. The patterns are
        // built at runtime so this test's own source cannot match them.
        let field_sites = format!("{}: ", "code");
        let helper_sites = format!("{}(", "htmlWarning");
        for pattern in [field_sites.as_str(), helper_sites.as_str()] {
            for site in sources.split(pattern).skip(1) {
                let site = site.trim_start();
                let Some(quote) = site.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
                    continue;
                };
                let code = site[1..].split(quote).next().unwrap();
                assert!(
                    super::WARNING_CODES.contains(&code),
                    "emitted warning code `{code}` is missing from WARNING_CODES and the README"
                );
            }
        }
    }
}

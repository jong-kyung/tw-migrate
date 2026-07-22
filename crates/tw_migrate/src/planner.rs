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
    css_plan::{ParsedCss, RulePlan, SelectorKey, parse_css_rules},
    js_rewrite::{plan_source_file, validate_js},
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
    syntax: StylesheetSyntax,
    #[serde(default)]
    is_module: Option<bool>,
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
    syntax: StylesheetSyntax,
    #[serde(default)]
    is_module: Option<bool>,
    #[serde(default)]
    css_module_id: Option<String>,
    #[serde(default)]
    css_dependents: Vec<String>,
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
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanResponse {
    files: Vec<PlannedFile>,
    deleted_files: Vec<String>,
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
struct RuleReport {
    selector: String,
    status: &'static str,
    candidates: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct Warning {
    pub(crate) code: &'static str,
    pub(crate) file: String,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) message: String,
}

#[derive(Clone)]
pub(crate) struct Edit {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) replacement: String,
}

pub fn plan_json(request: &str) -> Result<String, String> {
    let request: PlanRequest = serde_json::from_str(request).map_err(|error| error.to_string())?;
    serde_json::to_string(&plan_request(request, &HashMap::new(), false)?)
        .map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct RuleId {
    start: usize,
    end: usize,
}

type RuleConflicts = HashMap<RuleId, BTreeSet<(String, String)>>;

struct RuleOrigin {
    rule: RuleId,
    properties: BTreeSet<String>,
}

struct CandidateMaps {
    candidates: HashMap<SelectorKey, Vec<String>>,
    origins: HashMap<(SelectorKey, String), Vec<RuleOrigin>>,
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
    for (index, stylesheet) in request.stylesheets.iter().enumerate() {
        let plan_request = batch_stylesheet_request(&request, stylesheet, Vec::new());
        let candidate_maps = candidate_map_for_request(&plan_request)?;
        for file in request.files.iter().filter(|file| file.writable) {
            let result = crate::js_rewrite::plan_batch_source_file(
                file,
                &stylesheet.css_path,
                stylesheet
                    .is_module
                    .unwrap_or_else(|| is_stylesheet_module(&stylesheet.css_path)),
                &candidate_maps.candidates,
                &BTreeSet::new(),
            )?;
            for matched in result.matches {
                if let Some(origins) = candidate_maps
                    .origins
                    .get(&(matched.key, matched.candidate.clone()))
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
        let response = plan_request(stylesheet_request, &blocked_rules[index], true)?;

        for file in response.files {
            deleted.remove(&file.path);
            current.insert(file.path, file.source);
        }
        for path in response.deleted_files {
            current.remove(&path);
            deleted.insert(path);
        }
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
        syntax: stylesheet.syntax,
        is_module: stylesheet.is_module,
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
    let mut parsed = parse_css_rules(
        &request.css_path,
        keyframe_scope,
        &request.css_source,
        &request.theme_tokens,
        request.syntax.parser_syntax(),
        is_module,
        can_move_at_rules,
        relative_urls_stable,
    )?;
    if let Some(prefix) = request
        .utility_prefix
        .as_deref()
        .filter(|prefix| !prefix.is_empty())
    {
        prefix_rule_candidates(&mut parsed.rules, prefix);
    }
    Ok((is_module, parsed))
}

fn dedup_candidate_map(candidate_map: &mut HashMap<SelectorKey, Vec<String>>) {
    for candidates in candidate_map.values_mut() {
        candidates.sort();
        candidates.dedup();
    }
}

fn candidate_map_for_request(request: &PlanRequest) -> Result<CandidateMaps, String> {
    let (_, ParsedCss { rules, .. }) = parse_request_rules(request)?;
    let blocked_classes = rules
        .iter()
        .filter(|rule| rule.warning.is_some())
        .flat_map(|rule| rule.related_classes.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut candidate_map: HashMap<SelectorKey, Vec<String>> = HashMap::new();
    let mut origins: HashMap<(SelectorKey, String), Vec<RuleOrigin>> = HashMap::new();
    for rule in rules {
        if let Some(key) = rule.key
            && rule.warning.is_none()
            && !matches!(&key, SelectorKey::Class(name) if blocked_classes.contains(name))
        {
            let rule_id = RuleId {
                start: rule.span.start,
                end: rule.span.end,
            };
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
    })
}

fn plan_request(
    request: PlanRequest,
    blocked_rules: &RuleConflicts,
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
        let rule_id = RuleId {
            start: rule.span.start,
            end: rule.span.end,
        };
        if blocked_rules.contains_key(&rule_id) {
            rule.warning = Some("batch-stylesheet-conflict");
        }
    }

    let preserved_module_classes = rules
        .iter()
        .filter(|rule| batch_mode && is_module && rule.warning == Some("batch-stylesheet-conflict"))
        .flat_map(|rule| rule.related_classes.iter().cloned())
        .collect::<BTreeSet<_>>();
    let blocked_classes = rules
        .iter()
        .filter(|rule| {
            rule.warning.is_some()
                && !(batch_mode && rule.warning == Some("batch-stylesheet-conflict"))
        })
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
        let mut result = if batch_mode {
            crate::js_rewrite::plan_batch_source_file(
                file,
                &request.css_path,
                is_module,
                &candidate_map,
                &preserved_module_classes,
            )?
        } else {
            plan_source_file(file, &request.css_path, is_module, &candidate_map)?
        };

        module_references_safe &= result.module_references_safe;
        if !file.writable {
            if is_module
                && (!result.module_refs.is_empty() || !result.removable_import_edits.is_empty())
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

        if can_remove {
            converted_rules += 1;
            css_edits.push(Edit {
                start: rule.span.start,
                end: rule.span.end,
                replacement: String::new(),
            });
            rule_reports.push(RuleReport {
                selector: rule.selector,
                status: "converted",
                candidates: rule.candidates,
            });
        } else {
            retained_rules += 1;
            let rule_id = RuleId {
                start: rule.span.start,
                end: rule.span.end,
            };
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
                        "Generated utilities {conflicts} conflict on the same JSX element, so the contributing rule is retained."
                    ),
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
                start: rule.span.start,
                end: rule.span.end,
                message,
            });
            rule_reports.push(RuleReport {
                selector: rule.selector,
                status: "retained",
                candidates: rule.candidates,
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

    let mut deleted_files = Vec::new();
    if !css_edits.is_empty() {
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
    for (file, mut result) in source_plans {
        if css_module_deleted {
            result.edits.append(&mut result.removable_import_edits);
        }
        if !result.edits.is_empty() {
            let source = apply_edits(&file.source, result.edits)?;
            validate_js(&file.path, &source)?;
            planned_files.push(PlannedFile {
                path: file.path.clone(),
                source,
            });
        }
    }

    Ok(PlanResponse {
        files: planned_files,
        deleted_files,
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

fn validate_css(source: &str) -> Result<(), String> {
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
    use crate::utilities::{css_properties_conflict, tailwind_utilities_conflict};

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
            "export const Card = () => <div className='card p-[13px]' />;\n"
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
        assert_eq!(response["warnings"][0]["code"], "retained-global-rule");
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
                "[opacity:1]",
                "motion-reduce:hidden",
                "starting:[opacity:0]"
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
                .any(|warning| { warning["code"] == "unsupported-css-module-reference" })
        );
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
            "export const App = () => <div className=\"rounded-[13px] [border-color:blue]\" />;\n"
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
}

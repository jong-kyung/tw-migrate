use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Component, Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, CallExpression, ExportAllDeclaration, ExportNamedDeclaration, Expression,
    ImportDeclaration, ImportDeclarationSpecifier, ImportExpression, JSXAttributeItem,
    JSXAttributeName, JSXAttributeValue, JSXExpression, JSXOpeningElement,
    StaticMemberExpression, TemplateLiteral,
};
use oxc_ast_visit::{Visit, walk};
use oxc_css_parser::{
    Parser as CssParser, Syntax,
    ast::{ComplexSelectorChild, InterpolableIdent, SimpleSelector, Statement, Stylesheet},
};
use oxc_parser::Parser;
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_span::{SourceType, Span};
use oxc_syntax::symbol::SymbolId;
use serde::{Deserialize, Serialize};

use crate::{
    animations::{KeyframePlan, animation_candidate, append_keyframes, keyframe_plan},
    arbitrary::encode as encode_arbitrary,
    at_rules::{
        GlobalAtRulePlan, append_global_at_rules, conditional_variant, global_at_rule_plan,
        is_conditional, unsupported_warning,
    },
    utilities::{SpacingValues, declaration_to_candidate, tailwind_utilities_conflict},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanRequest {
    css_path: String,
    css_source: String,
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
    files: Vec<SourceFile>,
}

#[derive(Deserialize)]
struct SourceFile {
    path: String,
    source: String,
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
struct Warning {
    code: &'static str,
    file: String,
    start: usize,
    end: usize,
    message: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SelectorKey {
    Class(String),
    Id(String),
}

struct RulePlan {
    span: std::ops::Range<usize>,
    selector: String,
    related_classes: Vec<String>,
    key: Option<SelectorKey>,
    candidates: Vec<String>,
    warning: Option<&'static str>,
}

struct ParsedCss {
    rules: Vec<RulePlan>,
    keyframes: Vec<KeyframePlan>,
    global_at_rules: Vec<GlobalAtRulePlan>,
}

#[derive(Clone)]
struct Edit {
    start: usize,
    end: usize,
    replacement: String,
}

pub fn plan_json(request: &str) -> Result<String, String> {
    let request: PlanRequest = serde_json::from_str(request).map_err(|error| error.to_string())?;
    let is_module = request.css_path.ends_with(".module.css");
    let can_move_at_rules = request
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
    let ParsedCss {
        mut rules,
        keyframes,
        global_at_rules,
    } = parse_css_rules(
        &request.css_path,
        keyframe_scope,
        &request.css_source,
        &request.theme_tokens,
        is_module,
        can_move_at_rules,
        relative_urls_stable,
    )?;

    if let Some(prefix) = request
        .utility_prefix
        .as_deref()
        .filter(|prefix| !prefix.is_empty())
    {
        for candidate in rules.iter_mut().flat_map(|rule| &mut rule.candidates) {
            *candidate = format!("{prefix}:{candidate}");
        }
    }

    let blocked_classes = rules
        .iter()
        .filter(|rule| rule.warning.is_some())
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
    for candidates in candidate_map.values_mut() {
        candidates.sort();
        candidates.dedup();
    }

    let mut planned_files = Vec::new();
    let mut candidates = BTreeSet::new();
    let mut module_refs: HashMap<String, usize> = HashMap::new();
    let mut matched_module_refs: HashMap<String, usize> = HashMap::new();
    let mut module_references_safe = true;
    let mut warnings = Vec::new();
    let mut source_plans = Vec::new();

    for file in &request.files {
        let mut result = plan_source_file(file, &request.css_path, is_module, &candidate_map)?;

        module_references_safe &= result.module_references_safe;
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
            && all_module_refs_migrated
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
            let (code, message) = if let Some(code) = rule.warning {
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
            remove_empty_conditionals(source)?
        } else {
            source
        };
        validate_css(&source)?;
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

    serde_json::to_string(&PlanResponse {
        files: planned_files,
        deleted_files,
        candidates: candidates.into_iter().collect(),
        converted_rules,
        retained_rules,
        rules: rule_reports,
        warnings,
    })
    .map_err(|error| error.to_string())
}

fn merge_counts(target: &mut HashMap<String, usize>, source: &HashMap<String, usize>) {
    for (key, count) in source {
        *target.entry(key.clone()).or_default() += *count;
    }
}

fn parse_css_rules(
    path: &str,
    keyframe_scope: &str,
    source: &str,
    theme_tokens: &HashMap<String, String>,
    is_module: bool,
    can_move_at_rules: bool,
    relative_urls_stable: bool,
) -> Result<ParsedCss, String> {
    let allocator = oxc_css_parser::Allocator::default();
    let mut parser = CssParser::new(&allocator, source, Syntax::Css);
    let stylesheet = parser
        .parse::<Stylesheet>()
        .map_err(|error| format!("Failed to parse {path}: {error:?}"))?;

    let keyframes = if is_module && can_move_at_rules {
        stylesheet
            .statements
            .iter()
            .filter_map(|statement| {
                let Statement::AtRule(at_rule) = statement else {
                    return None;
                };
                keyframe_plan(at_rule, keyframe_scope, source)
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let global_at_rules = if is_module && can_move_at_rules {
        stylesheet
            .statements
            .iter()
            .filter_map(|statement| {
                let Statement::AtRule(at_rule) = statement else {
                    return None;
                };
                global_at_rule_plan(at_rule, source, relative_urls_stable)
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let keyframe_names = keyframes
        .iter()
        .map(|keyframe| (keyframe.name.as_str(), keyframe.migrated_name.as_str()))
        .collect::<HashMap<_, _>>();

    let mut composed_classes = BTreeSet::new();
    if is_module {
        collect_composed_classes(&stylesheet.statements, source, &mut composed_classes);
    }

    let movable_at_rule_starts = keyframes
        .iter()
        .map(|keyframe| keyframe.span.start)
        .chain(global_at_rules.iter().map(|at_rule| at_rule.span.start))
        .collect::<HashSet<_>>();
    let mut qualified_rules = Vec::new();
    let mut rules = Vec::new();
    collect_conditional_rules(
        &stylesheet.statements,
        &[],
        source,
        theme_tokens,
        &movable_at_rule_starts,
        &mut qualified_rules,
        &mut rules,
    );

    for (rule, outer_variants) in qualified_rules {
        let selector = source[rule.selector.span.start..rule.selector.span.end].to_string();
        let selector_match = selector_match(rule, source, is_module);
        let key = selector_match.as_ref().map(|(key, _)| key.clone());
        let mut variants = outer_variants;
        if let Some(variant) = selector_match.and_then(|(_, variant)| variant) {
            variants.push(variant);
        }
        let (mut candidates, declaration_warning) = collect_declaration_candidates(
            &rule.block.statements,
            &variants,
            source,
            theme_tokens,
            &keyframe_names,
            is_module,
        );
        let mut warning = key.is_none().then_some("unsupported-selector");
        if matches!(&key, Some(SelectorKey::Class(name)) if composed_classes.contains(name)) {
            warning = Some("css-module-composes");
        }
        if declaration_warning.is_some() {
            warning = declaration_warning;
        }
        candidates.sort();
        candidates.dedup();
        rules.push(RulePlan {
            span: rule.span.start..rule.span.end,
            selector,
            related_classes: selector_classes(rule),
            key,
            candidates,
            warning,
        });
    }
    let mut overlapping_rules = BTreeSet::new();
    for left in 0..rules.len() {
        for right in left + 1..rules.len() {
            if rules[left].key == rules[right].key
                && rules[left].key.is_some()
                && rules[left].candidates.iter().any(|left_candidate| {
                    rules[right].candidates.iter().any(|right_candidate| {
                        tailwind_utilities_conflict(left_candidate, right_candidate)
                    })
                })
            {
                overlapping_rules.extend([left, right]);
            }
        }
    }
    for index in overlapping_rules {
        rules[index].warning = Some("unsupported-overlap");
    }

    Ok(ParsedCss {
        rules,
        keyframes,
        global_at_rules,
    })
}

fn collect_declaration_candidates(
    statements: &[Statement<'_>],
    variants: &[String],
    source: &str,
    theme_tokens: &HashMap<String, String>,
    keyframes: &HashMap<&str, &str>,
    is_module: bool,
) -> (Vec<String>, Option<&'static str>) {
    // CSS keeps the last of duplicate same-property declarations, so a later
    // declaration replaces the candidate emitted by an earlier one.
    fn push_last_wins<'p>(
        slots: &mut HashMap<&'p str, usize>,
        candidates: &mut Vec<String>,
        property: &'p str,
        candidate: String,
    ) {
        if let Some(&slot) = slots.get(property) {
            candidates[slot] = candidate;
        } else {
            slots.insert(property, candidates.len());
            candidates.push(candidate);
        }
    }

    let mut candidates = Vec::new();
    let mut local_candidates = Vec::new();
    let mut property_slots = HashMap::new();
    let mut margin = SpacingValues::default();
    let mut padding = SpacingValues::default();
    let mut warning = None;

    for statement in statements {
        if let Statement::AtRule(at_rule) = statement {
            let Some((variant, block)) = conditional_variant(at_rule, source, theme_tokens)
                .zip(at_rule.block.as_ref())
                .filter(|_| is_conditional(at_rule.name.name))
            else {
                warning = Some("unsupported-rule-content");
                continue;
            };
            let mut nested_variants = variants.to_vec();
            nested_variants.push(variant);
            let (nested_candidates, nested_warning) = collect_declaration_candidates(
                &block.statements,
                &nested_variants,
                source,
                theme_tokens,
                keyframes,
                is_module,
            );
            candidates.extend(nested_candidates);
            if nested_warning.is_some() {
                warning = nested_warning;
            }
            continue;
        }

        let Statement::Declaration(declaration) = statement else {
            warning = Some("unsupported-rule-content");
            continue;
        };
        if declaration.important.is_some() {
            warning = Some("unsupported-important");
            continue;
        }
        let Some(property) = literal_ident(&declaration.name) else {
            warning = Some("unsupported-declaration");
            continue;
        };
        let value = declaration_value(source, declaration);
        if property == "composes" {
            warning = Some("css-module-composes");
            continue;
        }
        if is_module && matches!(property, "animation" | "animation-name") {
            match animation_candidate(property, value, keyframes) {
                Some(candidate) => {
                    push_last_wins(&mut property_slots, &mut local_candidates, property, candidate);
                }
                None => warning = Some("unsupported-animation"),
            }
            continue;
        }
        let components = declaration
            .value
            .iter()
            .map(|component| {
                let span = component.span();
                source[span.start..span.end].trim()
            })
            .collect::<Vec<_>>();
        let spacing_result = margin
            .apply(property, "margin", value, &components)
            .and_then(|handled| {
                if handled {
                    Ok(true)
                } else {
                    padding.apply(property, "padding", value, &components)
                }
            });
        match spacing_result {
            Ok(true) => continue,
            Err(()) => {
                warning = Some("unsupported-overlap");
                continue;
            }
            Ok(false) => {}
        }
        match declaration_to_candidate(property, value, theme_tokens) {
            Some(candidate) => {
                push_last_wins(&mut property_slots, &mut local_candidates, property, candidate);
            }
            None => warning = Some("unsupported-declaration"),
        }
    }

    local_candidates.extend(margin.candidates("m", theme_tokens));
    local_candidates.extend(padding.candidates("p", theme_tokens));
    if !variants.is_empty() {
        let variants = variants.join(":");
        for candidate in &mut local_candidates {
            *candidate = format!("{variants}:{candidate}");
        }
    }
    candidates.extend(local_candidates);
    if candidates.is_empty() && warning.is_none() {
        warning = Some("unsupported-declaration");
    }
    (candidates, warning)
}

fn collect_composed_classes(
    statements: &[Statement<'_>],
    source: &str,
    classes: &mut BTreeSet<String>,
) {
    for statement in statements {
        match statement {
            Statement::QualifiedRule(rule) => {
                for statement in &rule.block.statements {
                    let Statement::Declaration(declaration) = statement else {
                        continue;
                    };
                    if literal_ident(&declaration.name) == Some("composes") {
                        let value = declaration_value(source, declaration);
                        classes.extend(
                            value
                                .split_whitespace()
                                .take_while(|part| *part != "from")
                                .map(str::to_string),
                        );
                    }
                }
            }
            Statement::AtRule(at_rule) => {
                if let Some(block) = &at_rule.block {
                    collect_composed_classes(&block.statements, source, classes);
                }
            }
            _ => {}
        }
    }
}

fn collect_conditional_rules<'a, 's>(
    statements: &'s [Statement<'a>],
    variants: &[String],
    source: &str,
    theme_tokens: &HashMap<String, String>,
    movable_at_rule_starts: &HashSet<usize>,
    qualified_rules: &mut Vec<(&'s oxc_css_parser::ast::QualifiedRule<'a>, Vec<String>)>,
    retained_rules: &mut Vec<RulePlan>,
) -> bool {
    let mut all_supported = true;
    for statement in statements {
        match statement {
            Statement::QualifiedRule(rule) => {
                qualified_rules.push((rule, variants.to_vec()));
            }
            Statement::AtRule(at_rule)
                if variants.is_empty() && movable_at_rule_starts.contains(&at_rule.span.start) => {}
            Statement::AtRule(at_rule) if is_conditional(at_rule.name.name) => {
                let Some((variant, block)) =
                    conditional_variant(at_rule, source, theme_tokens).zip(at_rule.block.as_ref())
                else {
                    retained_rules.push(retained_at_rule(
                        at_rule,
                        source,
                        unsupported_warning(at_rule.name.name),
                    ));
                    all_supported = false;
                    continue;
                };

                let mut nested_variants = variants.to_vec();
                nested_variants.push(variant);
                let mut nested_rules = Vec::new();
                let mut nested_retained = Vec::new();
                if collect_conditional_rules(
                    &block.statements,
                    &nested_variants,
                    source,
                    theme_tokens,
                    movable_at_rule_starts,
                    &mut nested_rules,
                    &mut nested_retained,
                ) {
                    qualified_rules.extend(nested_rules);
                } else {
                    retained_rules.push(retained_at_rule(
                        at_rule,
                        source,
                        "unsupported-nested-at-rule",
                    ));
                    all_supported = false;
                }
            }
            Statement::AtRule(at_rule) => {
                retained_rules.push(retained_at_rule(at_rule, source, "unsupported-at-rule"));
                all_supported = false;
            }
            _ => all_supported = false,
        }
    }
    all_supported
}

fn retained_at_rule(
    at_rule: &oxc_css_parser::ast::AtRule<'_>,
    source: &str,
    warning: &'static str,
) -> RulePlan {
    let end = at_rule
        .block
        .as_ref()
        .map_or(at_rule.span.end, |block| block.span.start);
    let mut related_classes = BTreeSet::new();
    if let Some(block) = &at_rule.block {
        collect_statement_classes(&block.statements, &mut related_classes);
    }
    RulePlan {
        span: at_rule.span.start..at_rule.span.end,
        selector: source[at_rule.span.start..end].trim().to_string(),
        related_classes: related_classes.into_iter().collect(),
        key: None,
        candidates: Vec::new(),
        warning: Some(warning),
    }
}

fn collect_statement_classes(statements: &[Statement<'_>], classes: &mut BTreeSet<String>) {
    for statement in statements {
        match statement {
            Statement::QualifiedRule(rule) => classes.extend(selector_classes(rule)),
            Statement::AtRule(at_rule) => {
                if let Some(block) = &at_rule.block {
                    collect_statement_classes(&block.statements, classes);
                }
            }
            _ => {}
        }
    }
}

fn selector_classes(rule: &oxc_css_parser::ast::QualifiedRule<'_>) -> Vec<String> {
    rule.selector
        .selectors
        .iter()
        .flat_map(|selector| &selector.children)
        .filter_map(|child| match child {
            ComplexSelectorChild::CompoundSelector(compound) => Some(compound),
            ComplexSelectorChild::Combinator(_) => None,
        })
        .flat_map(|compound| &compound.children)
        .filter_map(|selector| match selector {
            SimpleSelector::Class(class) => literal_ident(&class.name).map(str::to_string),
            _ => None,
        })
        .collect()
}

fn declaration_value<'a>(
    source: &'a str,
    declaration: &oxc_css_parser::ast::Declaration<'_>,
) -> &'a str {
    source[declaration.colon_span.end..declaration.span.end]
        .trim()
        .trim_end_matches(';')
        .trim()
}

fn selector_match(
    rule: &oxc_css_parser::ast::QualifiedRule<'_>,
    source: &str,
    is_module: bool,
) -> Option<(SelectorKey, Option<String>)> {
    let selector = rule.selector.selectors.first()?;
    if rule.selector.selectors.len() != 1 {
        return None;
    }

    if selector.children.len() == 1 {
        let ComplexSelectorChild::CompoundSelector(compound) = &selector.children[0] else {
            return None;
        };
        let key = selector_key(compound.children.first()?)?;
        let variant = match compound.children.as_slice() {
            [_] => None,
            [_, SimpleSelector::PseudoClass(pseudo)] if pseudo.arg.is_none() => {
                let name = literal_ident(&pseudo.name)?;
                if !matches!(
                    name,
                    "active"
                        | "disabled"
                        | "focus"
                        | "focus-visible"
                        | "focus-within"
                        | "hover"
                        | "visited"
                ) {
                    return None;
                }
                Some(name.to_string())
            }
            _ if !is_module => arbitrary_selector_variant(rule, source, compound)?,
            _ => return None,
        };
        return Some((key, variant));
    }

    if is_module {
        return None;
    }
    let ComplexSelectorChild::CompoundSelector(target) = selector.children.last()? else {
        return None;
    };
    let key = selector_key(target.children.first()?)?;
    let variant = arbitrary_selector_variant(rule, source, target)?;
    Some((key, variant))
}

fn selector_key(selector: &SimpleSelector<'_>) -> Option<SelectorKey> {
    match selector {
        SimpleSelector::Class(class) => {
            literal_ident(&class.name).map(|name| SelectorKey::Class(name.to_string()))
        }
        SimpleSelector::Id(id) => {
            literal_ident(&id.name).map(|name| SelectorKey::Id(name.to_string()))
        }
        _ => None,
    }
}

fn arbitrary_selector_variant(
    rule: &oxc_css_parser::ast::QualifiedRule<'_>,
    source: &str,
    target: &oxc_css_parser::ast::CompoundSelector<'_>,
) -> Option<Option<String>> {
    // Replace the target simple selector by its parsed span. Searching the
    // selector text for ".name" matched the wrong occurrence when the name
    // recurred later (e.g. inside `:not(.abc)` for `.a:not(.abc)`).
    let target_span = match target.children.first()? {
        SimpleSelector::Class(class) if literal_ident(&class.name).is_some() => class.span,
        SimpleSelector::Id(id) if literal_ident(&id.name).is_some() => id.span,
        _ => return None,
    };
    let selector_span = rule.selector.span;
    let mut condition = source[selector_span.start..selector_span.end].to_string();
    condition.replace_range(
        target_span.start - selector_span.start..target_span.end - selector_span.start,
        "&",
    );
    Some(Some(format!("[{}]", encode_arbitrary(&condition))))
}

fn literal_ident<'a>(ident: &'a InterpolableIdent<'a>) -> Option<&'a str> {
    match ident {
        InterpolableIdent::Literal(ident) => Some(ident.name),
        _ => None,
    }
}

struct SourcePlan {
    edits: Vec<Edit>,
    removable_import_edits: Vec<Edit>,
    candidates: Vec<String>,
    module_refs: HashMap<String, usize>,
    matched_module_refs: HashMap<String, usize>,
    module_references_safe: bool,
    warnings: Vec<Warning>,
}

fn source_type_for_path(path: &str) -> Result<SourceType, String> {
    let source_type = SourceType::from_path(Path::new(path)).map_err(|error| error.to_string())?;
    Ok(
        if Path::new(path)
            .extension()
            .is_some_and(|extension| extension == "js")
        {
            source_type.with_jsx(true)
        } else {
            source_type
        },
    )
}

fn plan_source_file(
    file: &SourceFile,
    css_path: &str,
    is_module: bool,
    candidates: &HashMap<SelectorKey, Vec<String>>,
) -> Result<SourcePlan, String> {
    let allocator = Allocator::default();
    let source_type = source_type_for_path(&file.path)
        .map_err(|error| format!("Unsupported source file {}: {error}", file.path))?;
    let parsed = Parser::new(&allocator, &file.source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "Failed to parse {}: {:?}",
            file.path, parsed.diagnostics
        ));
    }
    let semantic = SemanticBuilder::new_compiler().build(&parsed.program);
    if !semantic.diagnostics.is_empty() {
        return Err(format!(
            "Failed to analyze {}: {:?}",
            file.path, semantic.diagnostics
        ));
    }

    let mut imports = ImportCollector {
        file_path: &file.path,
        css_path,
        bindings: Vec::new(),
        unsupported_shape: false,
        warning_span: None,
    };
    if is_module {
        imports.visit_program(&parsed.program);
    }

    let scoping = semantic.semantic.scoping();
    let total_import_refs = imports
        .bindings
        .iter()
        .map(|binding| scoping.get_resolved_reference_ids(binding.symbol).len())
        .sum::<usize>();
    let mut collector = UsageCollector {
        source: &file.source,
        file_path: &file.path,
        is_module,
        scoping,
        import_bindings: &imports.bindings,
        candidates,
        edits: Vec::new(),
        emitted_candidates: BTreeSet::new(),
        module_refs: HashMap::new(),
        matched_module_refs: HashMap::new(),
        warnings: Vec::new(),
    };
    collector.visit_program(&parsed.program);

    let classified_import_refs = collector.module_refs.values().sum::<usize>();
    let module_references_safe =
        !imports.unsupported_shape && total_import_refs == classified_import_refs;
    if !module_references_safe && let Some(span) = imports.warning_span {
        collector.warnings.push(Warning {
            code: "unsupported-css-module-reference",
            file: file.path.clone(),
            start: span.start as usize,
            end: span.end as usize,
            message: "The CSS Module has an import or reference that cannot be migrated safely."
                .to_string(),
        });
    }

    let removable_import_edits = if is_module
        && module_references_safe
        && !imports.bindings.is_empty()
        && classified_import_refs == collector.matched_module_refs.values().sum::<usize>()
    {
        imports
            .bindings
            .iter()
            .map(|binding| Edit {
                start: binding.span.start as usize,
                end: consume_following_newline(&file.source, binding.span.end as usize),
                replacement: String::new(),
            })
            .collect()
    } else {
        Vec::new()
    };

    Ok(SourcePlan {
        edits: collector.edits,
        removable_import_edits,
        candidates: collector.emitted_candidates.into_iter().collect(),
        module_refs: collector.module_refs,
        matched_module_refs: collector.matched_module_refs,
        module_references_safe,
        warnings: collector.warnings,
    })
}

struct ImportBinding {
    symbol: SymbolId,
    span: Span,
}

struct ImportCollector<'s> {
    file_path: &'s str,
    css_path: &'s str,
    bindings: Vec<ImportBinding>,
    unsupported_shape: bool,
    warning_span: Option<Span>,
}

impl<'a> Visit<'a> for ImportCollector<'_> {
    fn visit_import_declaration(&mut self, declaration: &ImportDeclaration<'a>) {
        let resolved = resolve_import(self.file_path, declaration.source.value.as_str());
        if resolved == normalize_path(Path::new(self.css_path)) {
            self.warning_span.get_or_insert(declaration.span);
            let Some(specifiers) = &declaration.specifiers else {
                self.unsupported_shape = true;
                walk::walk_import_declaration(self, declaration);
                return;
            };
            if specifiers.len() != 1 {
                self.unsupported_shape = true;
            }
            for specifier in specifiers {
                if let ImportDeclarationSpecifier::ImportDefaultSpecifier(specifier) = specifier
                    && let Some(symbol) = specifier.local.symbol_id.get()
                {
                    self.bindings.push(ImportBinding {
                        symbol,
                        span: declaration.span,
                    });
                } else {
                    self.unsupported_shape = true;
                }
            }
        }
        walk::walk_import_declaration(self, declaration);
    }

    fn visit_export_named_declaration(&mut self, declaration: &ExportNamedDeclaration<'a>) {
        if declaration.source.as_ref().is_some_and(|source| {
            resolve_import(self.file_path, source.value.as_str())
                == normalize_path(Path::new(self.css_path))
        }) {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(declaration.span);
        }
        walk::walk_export_named_declaration(self, declaration);
    }

    fn visit_export_all_declaration(&mut self, declaration: &ExportAllDeclaration<'a>) {
        if resolve_import(self.file_path, declaration.source.value.as_str())
            == normalize_path(Path::new(self.css_path))
        {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(declaration.span);
        }
        walk::walk_export_all_declaration(self, declaration);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Expression::Identifier(callee) = &call.callee
            && callee.name == "require"
            && let Some(Argument::StringLiteral(source)) = call.arguments.first()
            && resolve_import(self.file_path, source.value.as_str())
                == normalize_path(Path::new(self.css_path))
        {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(call.span);
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_import_expression(&mut self, import: &ImportExpression<'a>) {
        if let Expression::StringLiteral(source) = &import.source
            && resolve_import(self.file_path, source.value.as_str())
                == normalize_path(Path::new(self.css_path))
        {
            self.unsupported_shape = true;
            self.warning_span.get_or_insert(import.span);
        }
        walk::walk_import_expression(self, import);
    }
}

struct UsageCollector<'s> {
    source: &'s str,
    file_path: &'s str,
    is_module: bool,
    scoping: &'s Scoping,
    import_bindings: &'s [ImportBinding],
    candidates: &'s HashMap<SelectorKey, Vec<String>>,
    edits: Vec<Edit>,
    emitted_candidates: BTreeSet<String>,
    module_refs: HashMap<String, usize>,
    matched_module_refs: HashMap<String, usize>,
    warnings: Vec<Warning>,
}

impl UsageCollector<'_> {
    fn module_member_name<'a>(&self, member: &'a StaticMemberExpression<'a>) -> Option<&'a str> {
        let Expression::Identifier(object) = &member.object else {
            return None;
        };
        let reference = object.reference_id.get()?;
        let symbol = self.scoping.get_reference(reference).symbol_id()?;
        self.import_bindings
            .iter()
            .any(|binding| binding.symbol == symbol)
            .then(|| member.property.name.as_str())
    }

    fn static_template(
        &self,
        template: &TemplateLiteral<'_>,
    ) -> Option<(String, Vec<String>, Vec<String>)> {
        let mut value = String::new();
        let mut original = String::new();
        let mut members = Vec::new();
        for (index, quasi) in template.quasis.iter().enumerate() {
            let cooked = quasi.value.cooked.as_ref()?.as_str();
            value.push_str(cooked);
            original.push_str(cooked);
            let Some(expression) = template.expressions.get(index) else {
                continue;
            };
            let Expression::StaticMemberExpression(member) = expression else {
                return None;
            };
            let name = self.module_member_name(member)?.to_string();
            let candidates = self.candidates.get(&SelectorKey::Class(name.clone()))?;
            value.push_str(&candidates.join(" "));
            original.push('\0');
            members.push(name);
        }
        let static_classes = original
            .split_whitespace()
            .filter(|class| !class.contains('\0'))
            .map(str::to_string)
            .collect();
        Some((
            value.split_whitespace().collect::<Vec<_>>().join(" "),
            members,
            static_classes,
        ))
    }

    fn conflicting_utilities(
        &self,
        members: &[String],
        static_classes: &[String],
    ) -> Option<(String, String)> {
        for member in members {
            let candidates = self.candidates.get(&SelectorKey::Class(member.clone()))?;
            for candidate in candidates {
                if let Some(existing) = static_classes
                    .iter()
                    .find(|existing| tailwind_utilities_conflict(candidate, existing))
                {
                    return Some((candidate.clone(), existing.clone()));
                }
            }
        }
        None
    }

    fn global_element(&mut self, element: &JSXOpeningElement<'_>) {
        let mut id_candidates = Vec::new();
        let mut class_literal = None;
        let mut has_class_name = false;

        for item in &element.attributes {
            let JSXAttributeItem::Attribute(attribute) = item else {
                continue;
            };
            let JSXAttributeName::Identifier(name) = &attribute.name else {
                continue;
            };
            if name.name == "className" {
                has_class_name = true;
                if let Some(JSXAttributeValue::StringLiteral(literal)) = &attribute.value {
                    class_literal = Some((literal.span, literal.value.to_string()));
                }
            } else if name.name == "id"
                && let Some(JSXAttributeValue::StringLiteral(literal)) = &attribute.value
                && let Some(candidates) = self
                    .candidates
                    .get(&SelectorKey::Id(literal.value.to_string()))
            {
                id_candidates.extend(candidates.clone());
            }
        }

        if let Some((span, value)) = class_literal {
            self.global_literal_edit(span, &value, &id_candidates);
        } else if !has_class_name && !id_candidates.is_empty() {
            let end = element.span.end as usize;
            let mut insertion = if self.source[..end].ends_with("/>") {
                end - 2
            } else {
                end - 1
            };
            while insertion > element.span.start as usize
                && self.source.as_bytes()[insertion - 1].is_ascii_whitespace()
            {
                insertion -= 1;
            }
            for candidate in &id_candidates {
                self.emitted_candidates.insert(candidate.clone());
            }
            self.edits.push(Edit {
                start: insertion,
                end: insertion,
                replacement: format!(" className=\"{}\"", id_candidates.join(" ")),
            });
        }
    }

    fn global_literal_edit(&mut self, span: Span, value: &str, extra_candidates: &[String]) {
        let mut classes = value
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let original_classes = classes.clone();
        for class in original_classes {
            if let Some(candidates) = self.candidates.get(&SelectorKey::Class(class)) {
                for candidate in candidates {
                    self.emitted_candidates.insert(candidate.clone());
                    if !classes.contains(candidate) {
                        classes.push(candidate.clone());
                    }
                }
            }
        }
        for candidate in extra_candidates {
            self.emitted_candidates.insert(candidate.clone());
            if !classes.contains(candidate) {
                classes.push(candidate.clone());
            }
        }
        let replacement_value = classes.join(" ");
        if replacement_value == value {
            return;
        }
        let quote = self.source.as_bytes()[span.start as usize] as char;
        self.edits.push(Edit {
            start: span.start as usize,
            end: span.end as usize,
            replacement: format!("{quote}{replacement_value}{quote}"),
        });
    }
}

impl<'a> Visit<'a> for UsageCollector<'_> {
    fn visit_static_member_expression(&mut self, member: &StaticMemberExpression<'a>) {
        if let Some(name) = self.module_member_name(member).map(str::to_string) {
            *self.module_refs.entry(name).or_default() += 1;
        }
        walk::walk_static_member_expression(self, member);
    }

    fn visit_jsx_opening_element(&mut self, element: &JSXOpeningElement<'a>) {
        if !self.is_module {
            self.global_element(element);
            walk::walk_jsx_opening_element(self, element);
            return;
        }

        for item in &element.attributes {
            let JSXAttributeItem::Attribute(attribute) = item else {
                continue;
            };
            let JSXAttributeName::Identifier(name) = &attribute.name else {
                continue;
            };
            if name.name != "className" {
                continue;
            }
            let Some(JSXAttributeValue::ExpressionContainer(container)) = &attribute.value else {
                continue;
            };
            let (replacement_value, members, static_classes) = match &container.expression {
                JSXExpression::StaticMemberExpression(member) => {
                    let Some(member_name) = self.module_member_name(member).map(str::to_string)
                    else {
                        continue;
                    };
                    let key = SelectorKey::Class(member_name.clone());
                    let Some(candidates) = self.candidates.get(&key) else {
                        continue;
                    };
                    (candidates.join(" "), vec![member_name], Vec::new())
                }
                JSXExpression::TemplateLiteral(template) => {
                    let Some(result) = self.static_template(template) else {
                        self.warnings.push(Warning {
                            code: "dynamic-class-name",
                            file: self.file_path.to_string(),
                            start: container.span.start as usize,
                            end: container.span.end as usize,
                            message: "The template contains a dynamic or unsupported class."
                                .to_string(),
                        });
                        continue;
                    };
                    result
                }
                _ => {
                    self.warnings.push(Warning {
                        code: "dynamic-class-name",
                        file: self.file_path.to_string(),
                        start: container.span.start as usize,
                        end: container.span.end as usize,
                        message: "Only static className values are supported.".to_string(),
                    });
                    continue;
                }
            };
            if let Some((generated, existing)) =
                self.conflicting_utilities(&members, &static_classes)
            {
                self.warnings.push(Warning {
                    code: "existing-tailwind-conflict",
                    file: self.file_path.to_string(),
                    start: container.span.start as usize,
                    end: container.span.end as usize,
                    message: format!(
                        "Generated utility `{generated}` may conflict with existing `{existing}`."
                    ),
                });
            }
            self.edits.push(Edit {
                start: container.span.start as usize,
                end: container.span.end as usize,
                replacement: serde_json::to_string(&replacement_value)
                    .expect("string serialization"),
            });
            for member in members {
                let key = SelectorKey::Class(member.clone());
                for candidate in &self.candidates[&key] {
                    self.emitted_candidates.insert(candidate.clone());
                }
                *self.matched_module_refs.entry(member).or_default() += 1;
            }
        }
        walk::walk_jsx_opening_element(self, element);
    }
}

fn resolve_import(file_path: &str, import: &str) -> PathBuf {
    let parent = Path::new(file_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    normalize_path(&parent.join(import))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn consume_following_newline(source: &str, end: usize) -> usize {
    if source[end..].starts_with("\r\n") {
        end + 2
    } else if source[end..].starts_with('\n') {
        end + 1
    } else {
        end
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

fn validate_js(path: &str, source: &str) -> Result<(), String> {
    let allocator = Allocator::default();
    let source_type = source_type_for_path(path)
        .map_err(|error| format!("Unsupported source file {path}: {error}"))?;
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.diagnostics.is_empty() {
        Ok(())
    } else {
        Err(format!("Edited source no longer parses: {path}"))
    }
}

fn remove_empty_conditionals(mut source: String) -> Result<String, String> {
    loop {
        let allocator = oxc_css_parser::Allocator::default();
        let mut parser = CssParser::new(&allocator, &source, Syntax::Css);
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
    let allocator = oxc_css_parser::Allocator::default();
    CssParser::new(&allocator, source, Syntax::Css)
        .parse::<Stylesheet>()
        .map(|_| ())
        .map_err(|error| format!("Edited CSS no longer parses: {error:?}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        KeyframePlan, animation_candidate, append_keyframes, plan_json, tailwind_utilities_conflict,
    };

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
}

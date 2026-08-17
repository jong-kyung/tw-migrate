//! CSS-side planning: parse rules, build candidates, and classify selectors.

use std::collections::{BTreeSet, HashMap, HashSet};

use oxc_css_parser::{
    Syntax,
    ast::{ComplexSelectorChild, SimpleSelector, Statement},
};

use crate::{
    animations::{KeyframePlan, animation_candidate, keyframe_plan},
    at_rules::{
        GlobalAtRulePlan, conditional_variant, global_at_rule_plan, is_conditional, parse_css,
        unsupported_warning,
    },
    utilities::{
        OverflowValues, SpacingValues, declaration_to_candidate, tailwind_utilities_conflict,
    },
};

mod selectors;

pub use selectors::{ShadowIndex, index_shadow_selectors};
use selectors::{declaration_value, literal_ident, module_relationship_match, selector_match};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Relation {
    Child,
    Descendant,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SelectorKey {
    Class(String),
    Id(String),
}

/// One pairwise proof obligation of a multi-compound CSS Module selector:
/// every usage of `target` must be proven `relation` under `ancestor`.
pub struct RelationshipStep {
    pub ancestor: SelectorKey,
    pub relation: Relation,
    pub target: SelectorKey,
}

/// Decomposition of a supported multi-compound CSS Module selector whose
/// conversion requires a JSX-graph proof.
pub struct ModuleRelationship {
    /// Obligations ordered from the selector target upward; the first step's
    /// target is the rule's own key.
    pub steps: Vec<RelationshipStep>,
    /// An ancestor compound carries a pseudo-class (e.g. `.card:hover .title`);
    /// converting it would require Tailwind's group pattern, so it is retained.
    pub ancestor_state: bool,
}

pub struct RulePlan {
    /// Rule span in the analysis source (compiled CSS for preprocessor
    /// stylesheets, the authored file otherwise).
    pub span: std::ops::Range<usize>,
    /// Rule span in the authored file; `None` until source-map resolution
    /// proves a unique mapping (always `Some` for plain CSS).
    pub authored_span: Option<std::ops::Range<usize>>,
    /// Declaration offsets in the analysis source used to source-map the
    /// rule back to its authored span.
    pub provenance_offsets: Vec<usize>,
    pub selector: String,
    pub related_classes: Vec<String>,
    pub contains_selectors: bool,
    pub key: Option<SelectorKey>,
    pub relationship: Option<ModuleRelationship>,
    pub candidates: Vec<String>,
    pub candidate_properties: HashMap<String, BTreeSet<String>>,
    /// Arbitrary font-family candidates with their parsed stacks, for
    /// theme-token registration; empty unless the rule still rewrites.
    pub font_family_probes: Vec<crate::fonts::FontFamilyProbe>,
    pub warning: Option<&'static str>,
}

pub struct ParsedCss {
    pub rules: Vec<RulePlan>,
    pub keyframes: Vec<KeyframePlan>,
    pub global_at_rules: Vec<GlobalAtRulePlan>,
}

pub struct ParseOptions {
    pub syntax: Syntax,
    pub is_module: bool,
    pub can_move_at_rules: bool,
    /// False when actively applied global at-rules such as `@font-face`
    /// must stay in place even though renamed keyframes may still move:
    /// appending them to an ancestor-shared entry would activate them in
    /// every other flow loading that entry.
    pub can_move_global_at_rules: bool,
    pub relative_urls_stable: bool,
}

pub fn parse_css_rules(
    path: &str,
    keyframe_scope: &str,
    source: &str,
    theme_tokens: &HashMap<String, String>,
    media_names: Option<&HashMap<String, String>>,
    options: ParseOptions,
) -> Result<ParsedCss, String> {
    let ParseOptions {
        syntax,
        is_module,
        can_move_at_rules,
        can_move_global_at_rules,
        relative_urls_stable,
    } = options;
    let allocator = oxc_css_parser::Allocator::default();
    let stylesheet = parse_css(&allocator, source, syntax)
        .map_err(|error| format!("Failed to parse {path}: {error}"))?;

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
    let global_at_rules = if is_module && can_move_at_rules && can_move_global_at_rules {
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
        media_names,
        &movable_at_rule_starts,
        &mut qualified_rules,
        &mut rules,
    );

    for (rule, outer_variants) in qualified_rules {
        let selector = source[rule.selector.span.start..rule.selector.span.end].to_string();
        let mut relationship = None;
        let selector_match = selector_match(rule, source, is_module).or_else(|| {
            if !is_module {
                return None;
            }
            let (key, variant, decomposed) = module_relationship_match(rule)?;
            relationship = Some(decomposed);
            Some((key, variant))
        });
        let key = selector_match.as_ref().map(|(key, _)| key.clone());
        let mut variants = outer_variants;
        if let Some(variant) = selector_match.and_then(|(_, variant)| variant) {
            variants.push(variant);
        }
        let (candidate_properties, font_family_probes, declaration_warning) =
            collect_declaration_candidates(
            &rule.block.statements,
            &variants,
            source,
            theme_tokens,
            media_names,
            &keyframe_names,
            syntax,
            is_module,
        );
        let mut candidates = candidate_properties.keys().cloned().collect::<Vec<_>>();
        let mut warning = key.is_none().then_some("unsupported-selector");
        if matches!(&key, Some(SelectorKey::Class(name)) if composed_classes.contains(name)) {
            warning = Some("css-module-composes");
        }
        if declaration_warning.is_some() {
            warning = declaration_warning;
        }
        candidates.sort();
        candidates.dedup();
        let mut provenance_offsets = vec![rule.selector.span.start];
        collect_declaration_offsets(&rule.block.statements, &mut provenance_offsets);
        rules.push(RulePlan {
            span: rule.span.start..rule.span.end,
            authored_span: None,
            provenance_offsets,
            selector,
            related_classes: selector_classes(rule),
            contains_selectors: true,
            key,
            relationship,
            candidates,
            candidate_properties,
            font_family_probes,
            warning,
        });
    }
    let mut overlapping_rules = BTreeSet::new();
    let mut rules_by_key: HashMap<&SelectorKey, Vec<usize>> = HashMap::new();
    for (index, rule) in rules.iter().enumerate() {
        if let Some(key) = &rule.key {
            rules_by_key.entry(key).or_default().push(index);
        }
    }
    for indices in rules_by_key.values() {
        for (position, &left) in indices.iter().enumerate() {
            for &right in &indices[position + 1..] {
                if rules[left].candidates.iter().any(|left_candidate| {
                    rules[right].candidates.iter().any(|right_candidate| {
                        tailwind_utilities_conflict(left_candidate, right_candidate)
                    })
                }) {
                    overlapping_rules.extend([left, right]);
                }
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

fn collect_declaration_offsets(statements: &[Statement<'_>], offsets: &mut Vec<usize>) {
    for statement in statements {
        match statement {
            Statement::Declaration(declaration) => offsets.push(declaration.span.start),
            Statement::AtRule(at_rule) => {
                if let Some(block) = &at_rule.block {
                    collect_declaration_offsets(&block.statements, offsets);
                }
            }
            _ => {}
        }
    }
}

fn declaration_is_plain_css(
    declaration: &oxc_css_parser::ast::Declaration<'_>,
    source: &str,
) -> bool {
    let raw = &source[declaration.span.start..declaration.span.end];
    if raw.contains(['$', '@', '`', '(']) || raw.contains("#{") {
        return false;
    }
    let wrapped = format!("a{{{raw}}}");
    let allocator = oxc_css_parser::Allocator::default();
    parse_css(&allocator, &wrapped, Syntax::Css).is_ok()
}

fn collect_declaration_candidates(
    statements: &[Statement<'_>],
    variants: &[String],
    source: &str,
    theme_tokens: &HashMap<String, String>,
    media_names: Option<&HashMap<String, String>>,
    keyframes: &HashMap<&str, &str>,
    syntax: Syntax,
    is_module: bool,
) -> (
    HashMap<String, BTreeSet<String>>,
    Vec<crate::fonts::FontFamilyProbe>,
    Option<&'static str>,
) {
    // CSS keeps the last of duplicate same-property declarations, so a later
    // declaration replaces the candidate emitted by an earlier one.
    fn push_last_wins<'p>(
        slots: &mut HashMap<&'p str, usize>,
        candidates: &mut Vec<(String, String)>,
        property: &'p str,
        candidate: String,
    ) {
        let entry = (candidate, property.to_string());
        if let Some(&slot) = slots.get(property) {
            candidates[slot] = entry;
        } else {
            slots.insert(property, candidates.len());
            candidates.push(entry);
        }
    }

    fn merge_candidate(
        candidates: &mut HashMap<String, BTreeSet<String>>,
        candidate: String,
        properties: impl IntoIterator<Item = String>,
    ) {
        candidates.entry(candidate).or_default().extend(properties);
    }

    let mut candidates = HashMap::new();
    let mut local_candidates = Vec::new();
    let mut property_slots = HashMap::new();
    let mut font_probes: Vec<crate::fonts::FontFamilyProbe> = Vec::new();
    // Last-wins like the candidate slots: a later font-family declaration
    // replaces the earlier probe.
    let mut local_font_probe: Option<crate::fonts::FontFamilyProbe> = None;
    let mut margin = SpacingValues::default();
    let mut padding = SpacingValues::default();
    let mut inset = SpacingValues::default();
    let mut overflow = OverflowValues::default();
    let mut margin_properties = BTreeSet::new();
    let mut padding_properties = BTreeSet::new();
    let mut inset_properties = BTreeSet::new();
    let mut overflow_properties = BTreeSet::new();
    let mut warning = None;

    for statement in statements {
        if let Statement::AtRule(at_rule) = statement {
            let Some((variant, block)) =
                conditional_variant(at_rule, source, theme_tokens, media_names)
                    .zip(at_rule.block.as_ref())
                    .filter(|_| is_conditional(at_rule.name.name))
            else {
                warning = Some("unsupported-rule-content");
                continue;
            };
            let mut nested_variants = variants.to_vec();
            nested_variants.push(variant);
            let (nested_candidates, nested_probes, nested_warning) =
                collect_declaration_candidates(
                    &block.statements,
                    &nested_variants,
                    source,
                    theme_tokens,
                    media_names,
                    keyframes,
                    syntax,
                    is_module,
                );
            for (candidate, properties) in nested_candidates {
                merge_candidate(&mut candidates, candidate, properties);
            }
            font_probes.extend(nested_probes);
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
        if syntax != Syntax::Css && !declaration_is_plain_css(declaration, source) {
            warning = Some("unsupported-declaration");
            continue;
        }
        let Some(property) = literal_ident(&declaration.name) else {
            warning = Some("unsupported-declaration");
            continue;
        };
        let value = declaration_value(source, declaration);
        // Vue rewrites `v-bind()` only while compiling SFC styles; a value
        // moved into global Tailwind output would lose its reactive custom
        // property, so such declarations are never converted.
        if value.contains("v-bind(") {
            warning = Some("unsupported-value");
            continue;
        }
        if property == "composes" {
            warning = Some("css-module-composes");
            continue;
        }
        if is_module && matches!(property, "animation" | "animation-name") {
            match animation_candidate(property, value, keyframes) {
                Some(candidate) => push_last_wins(
                    &mut property_slots,
                    &mut local_candidates,
                    property,
                    candidate,
                ),
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
        let mut family_result = Ok(false);
        for (values, family, longhands, properties) in [
            (
                &mut margin,
                "margin",
                ["margin-top", "margin-right", "margin-bottom", "margin-left"],
                &mut margin_properties,
            ),
            (
                &mut padding,
                "padding",
                [
                    "padding-top",
                    "padding-right",
                    "padding-bottom",
                    "padding-left",
                ],
                &mut padding_properties,
            ),
            (
                &mut inset,
                "inset",
                ["top", "right", "bottom", "left"],
                &mut inset_properties,
            ),
        ] {
            family_result = values.apply(property, family, longhands, value, &components);
            if family_result == Ok(true) {
                properties.insert(property.to_string());
            }
            if family_result != Ok(false) {
                break;
            }
        }
        if family_result == Ok(false) {
            family_result = overflow.apply(property, value, &components);
            if family_result == Ok(true) {
                overflow_properties.insert(property.to_string());
            }
        }
        match family_result {
            Ok(true) => continue,
            Err(()) => {
                warning = Some("unsupported-overlap");
                continue;
            }
            Ok(false) => {}
        }
        match declaration_to_candidate(property, value, theme_tokens) {
            Ok(candidate) => {
                // A font-family that stayed arbitrary is a registration
                // probe; a runtime-dependent or unreadable stack keeps its
                // candidate under the existing safety rules without one.
                if property == "font-family" && candidate.starts_with("[font-family:") {
                    local_font_probe = crate::fonts::parse_font_stack(value).map(
                        |(normalized, first_family_name, first_family_kind)| {
                            crate::fonts::FontFamilyProbe {
                                candidate: candidate.clone(),
                                value: normalized,
                                first_family_name,
                                first_family_kind,
                            }
                        },
                    );
                } else if property == "font-family" {
                    local_font_probe = None;
                }
                push_last_wins(
                    &mut property_slots,
                    &mut local_candidates,
                    property,
                    candidate,
                );
            }
            Err(code) => warning = Some(code),
        }
    }

    let normalized_families = [
        (
            margin.candidates("m", ["mt", "mr", "mb", "ml"], theme_tokens),
            margin_properties,
        ),
        (
            padding.candidates("p", ["pt", "pr", "pb", "pl"], theme_tokens),
            padding_properties,
        ),
        (
            inset.candidates("inset", ["top", "right", "bottom", "left"], theme_tokens),
            inset_properties,
        ),
        (overflow.candidates(), overflow_properties),
    ];
    for (family_candidates, family_properties) in normalized_families {
        match family_candidates {
            Some(family_candidates) => {
                for candidate in family_candidates {
                    merge_candidate(
                        &mut candidates,
                        candidate,
                        family_properties.iter().cloned(),
                    );
                }
            }
            None => warning = Some("unsupported-value"),
        }
    }
    for (candidate, property) in local_candidates {
        merge_candidate(&mut candidates, candidate, [property]);
    }
    font_probes.extend(local_font_probe);
    if !variants.is_empty() {
        let variants = variants.join(":");
        candidates = candidates
            .into_iter()
            .map(|(candidate, properties)| (format!("{variants}:{candidate}"), properties))
            .collect();
        // Probe candidates mirror the candidate map exactly, including
        // this prefix step, so they stay valid alias keys.
        for probe in &mut font_probes {
            probe.candidate = format!("{variants}:{}", probe.candidate);
        }
    }
    if candidates.is_empty() && warning.is_none() {
        warning = Some("unsupported-declaration");
    }
    (candidates, font_probes, warning)
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
    media_names: Option<&HashMap<String, String>>,
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
                    conditional_variant(at_rule, source, theme_tokens, media_names)
                        .zip(at_rule.block.as_ref())
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
                    media_names,
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
    let contains_selectors = at_rule.block.as_ref().is_some_and(|block| {
        collect_statement_classes(&block.statements, &mut related_classes);
        statements_contain_selectors(&block.statements)
    });
    RulePlan {
        span: at_rule.span.start..at_rule.span.end,
        authored_span: None,
        provenance_offsets: Vec::new(),
        selector: source[at_rule.span.start..end].trim().to_string(),
        related_classes: related_classes.into_iter().collect(),
        contains_selectors,
        key: None,
        relationship: None,
        candidates: Vec::new(),
        candidate_properties: HashMap::new(),
        font_family_probes: Vec::new(),
        warning: Some(warning),
    }
}

fn statements_contain_selectors(statements: &[Statement<'_>]) -> bool {
    statements.iter().any(|statement| match statement {
        Statement::QualifiedRule(_) => true,
        Statement::AtRule(at_rule) => at_rule
            .block
            .as_ref()
            .is_some_and(|block| statements_contain_selectors(&block.statements)),
        _ => false,
    })
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

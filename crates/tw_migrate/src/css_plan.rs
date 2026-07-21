//! CSS-side planning: parse rules, build candidates, and classify selectors.

use std::collections::{BTreeSet, HashMap, HashSet};

use oxc_css_parser::{
    Parser as CssParser, Syntax,
    ast::{ComplexSelectorChild, InterpolableIdent, SimpleSelector, Statement, Stylesheet},
};

use crate::{
    animations::{KeyframePlan, animation_candidate, keyframe_plan},
    arbitrary::encode as encode_arbitrary,
    at_rules::{
        GlobalAtRulePlan, conditional_variant, global_at_rule_plan, is_conditional,
        unsupported_warning,
    },
    utilities::{SpacingValues, declaration_to_candidate, tailwind_utilities_conflict},
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SelectorKey {
    Class(String),
    Id(String),
}

pub(crate) struct RulePlan {
    pub(crate) span: std::ops::Range<usize>,
    pub(crate) selector: String,
    pub(crate) related_classes: Vec<String>,
    pub(crate) key: Option<SelectorKey>,
    pub(crate) candidates: Vec<String>,
    pub(crate) candidate_properties: HashMap<String, BTreeSet<String>>,
    pub(crate) warning: Option<&'static str>,
}

pub(crate) struct ParsedCss {
    pub(crate) rules: Vec<RulePlan>,
    pub(crate) keyframes: Vec<KeyframePlan>,
    pub(crate) global_at_rules: Vec<GlobalAtRulePlan>,
}

pub(crate) fn parse_css_rules(
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
        let (candidate_properties, declaration_warning) = collect_declaration_candidates(
            &rule.block.statements,
            &variants,
            source,
            theme_tokens,
            &keyframe_names,
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
        rules.push(RulePlan {
            span: rule.span.start..rule.span.end,
            selector,
            related_classes: selector_classes(rule),
            key,
            candidates,
            candidate_properties,
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
) -> (HashMap<String, BTreeSet<String>>, Option<&'static str>) {
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
    let mut margin = SpacingValues::default();
    let mut padding = SpacingValues::default();
    let mut margin_properties = BTreeSet::new();
    let mut padding_properties = BTreeSet::new();
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
            for (candidate, properties) in nested_candidates {
                merge_candidate(&mut candidates, candidate, properties);
            }
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
            Ok(true) => {
                if property == "margin" || property.starts_with("margin-") {
                    margin_properties.insert(property.to_string());
                } else {
                    padding_properties.insert(property.to_string());
                }
                continue;
            }
            Err(()) => {
                warning = Some("unsupported-overlap");
                continue;
            }
            Ok(false) => {}
        }
        match declaration_to_candidate(property, value, theme_tokens) {
            Ok(candidate) => push_last_wins(
                &mut property_slots,
                &mut local_candidates,
                property,
                candidate,
            ),
            Err(code) => warning = Some(code),
        }
    }

    match margin.candidates("m", theme_tokens) {
        Some(margin_candidates) => {
            for candidate in margin_candidates {
                merge_candidate(
                    &mut candidates,
                    candidate,
                    margin_properties.iter().cloned(),
                );
            }
        }
        None => warning = Some("unsupported-value"),
    }
    match padding.candidates("p", theme_tokens) {
        Some(padding_candidates) => {
            for candidate in padding_candidates {
                merge_candidate(
                    &mut candidates,
                    candidate,
                    padding_properties.iter().cloned(),
                );
            }
        }
        None => warning = Some("unsupported-value"),
    }
    for (candidate, property) in local_candidates {
        merge_candidate(&mut candidates, candidate, [property]);
    }
    if !variants.is_empty() {
        let variants = variants.join(":");
        candidates = candidates
            .into_iter()
            .map(|(candidate, properties)| (format!("{variants}:{candidate}"), properties))
            .collect();
    }
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
        candidate_properties: HashMap::new(),
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

use std::collections::HashMap;

use super::{RulePlan, SelectorKey};
use crate::utilities::{UtilityConflictSignature, tailwind_utility_parts};

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum ConflictProperty<'a> {
    MaskBit(u32),
    Arbitrary(&'a str),
}

struct Candidate<'a> {
    variants: &'a str,
    properties: Vec<ConflictProperty<'a>>,
}

impl<'a> Candidate<'a> {
    fn new(candidate: &'a str) -> Self {
        let (variants, utility) = tailwind_utility_parts(candidate);
        let signature = UtilityConflictSignature::new(utility);
        let mut mask = signature.property_mask;
        let mut properties = Vec::new();
        while mask != 0 {
            properties.push(ConflictProperty::MaskBit(mask.trailing_zeros()));
            mask &= mask - 1;
        }
        if let Some(property) = signature.arbitrary_property {
            properties.push(ConflictProperty::Arbitrary(property));
        }
        Self {
            variants,
            properties,
        }
    }
}

#[derive(Default)]
struct PropertyCounts {
    total: usize,
    by_rule: HashMap<usize, usize>,
    by_candidate: HashMap<usize, usize>,
}

/// Find every rule with a conflicting candidate in another rule, without
/// enumerating pairs. Candidates must be deduplicated within each rule, as in
/// parse_css_rules. Beyond parsing and hashing strings, expected time and space
/// are linear in rules and property occurrences (at most 64 mask bits and one
/// arbitrary property per candidate).
pub(super) fn overlapping_rules(rules: &[RulePlan]) -> Vec<bool> {
    let mut candidates = Vec::new();
    let mut candidate_ids = HashMap::new();
    let mut buckets: HashMap<(&SelectorKey, &str), Vec<(usize, usize)>> = HashMap::new();
    for (rule_id, rule) in rules.iter().enumerate() {
        let Some(key) = &rule.key else { continue };
        for candidate in &rule.candidates {
            let next_id = candidates.len();
            let candidate_id = *candidate_ids.entry(candidate.as_str()).or_insert_with(|| {
                candidates.push(Candidate::new(candidate));
                next_id
            });
            buckets
                .entry((key, candidates[candidate_id].variants))
                .or_default()
                .push((rule_id, candidate_id));
        }
    }

    let mut overlapping = vec![false; rules.len()];
    for occurrences in buckets.values() {
        // A bucket confined to one rule cannot contribute a cross-rule overlap.
        if occurrences
            .iter()
            .all(|(rule, _)| *rule == occurrences[0].0)
        {
            continue;
        }
        let mut counts: HashMap<ConflictProperty<'_>, PropertyCounts> = HashMap::new();
        for &(rule, candidate) in occurrences {
            for &property in &candidates[candidate].properties {
                let count = counts.entry(property).or_default();
                count.total += 1;
                *count.by_rule.entry(rule).or_default() += 1;
                *count.by_candidate.entry(candidate).or_default() += 1;
            }
        }
        for &(rule, candidate) in occurrences {
            if candidates[candidate].properties.iter().any(|property| {
                let count = &counts[property];
                // Exclude this rule and identical spellings. Their intersection
                // is exactly this occurrence because rule candidates are unique.
                count.total + 1 > count.by_rule[&rule] + count.by_candidate[&candidate]
            }) {
                overlapping[rule] = true;
            }
        }
    }
    overlapping
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Instant};

    use super::super::{ParseOptions, RulePlan, parse_css_rules};
    use super::overlapping_rules;
    use crate::utilities::tailwind_utilities_conflict;
    use oxc_css_parser::Syntax;

    fn parse(source: &str) -> Vec<RulePlan> {
        parse_css_rules(
            "overlap.css",
            "overlap",
            source,
            &HashMap::new(),
            None,
            ParseOptions {
                syntax: Syntax::Css,
                is_module: false,
                can_move_at_rules: false,
                can_move_global_at_rules: false,
                relative_urls_stable: false,
            },
        )
        .unwrap()
        .rules
    }

    // The former pairwise pass is the differential oracle. Keep it independent
    // of the indexed implementation, including its rule and equality gates.
    fn pairwise(rules: &[RulePlan]) -> Vec<bool> {
        let mut overlapping = vec![false; rules.len()];
        for (left, a) in rules.iter().enumerate() {
            for (right, b) in rules.iter().enumerate().skip(left + 1) {
                if a.key.is_some()
                    && a.key == b.key
                    && a.candidates.iter().any(|a| {
                        b.candidates
                            .iter()
                            .any(|b| tailwind_utilities_conflict(a, b))
                    })
                {
                    overlapping[left] = true;
                    overlapping[right] = true;
                }
            }
        }
        overlapping
    }

    #[test]
    fn preserves_overlap_warning_targets() {
        let overlap = Some("unsupported-overlap");
        let cases: &[(&str, &[Option<&str>])] = &[
            (".a { padding: 8px; } .a { padding: 8px; }", &[None, None]),
            (
                ".a { padding: 8px; } .a { padding-left: 4px; } .a { padding-inline-start: 12px; }",
                &[overlap, overlap, overlap],
            ),
            (".a { padding: 8px; padding-inline-start: 12px; }", &[None]),
            (
                ".a { border-left-width: 1px; } .a { border-inline-start-width: 2px; }",
                &[overlap, overlap],
            ),
            (
                ".a { --custom: 1; } .a { --custom: 2; } .a { --other: 2; }",
                &[overlap, overlap, None],
            ),
            (
                ".a { display: grid; } #a { display: flex; } .b { display: block; }",
                &[None, None, None],
            ),
            (
                ".a { display: grid; } .a { display: flex; padding: 1px !important; } .a { padding: 2px !important; }",
                &[overlap, overlap, Some("unsupported-important")],
            ),
            (
                ".a, .b { display: grid; } .a, .b { display: flex; }",
                &[Some("unsupported-selector"), Some("unsupported-selector")],
            ),
            (
                "@supports (width: 1px) { .a { display: grid; } } @supports (width: 2px) { .a { display: flex; } }",
                &[None, None],
            ),
            (
                ".a { display: grid; @supports (display: grid) { display: flex; } } .a { @supports (display: grid) { display: block; } }",
                &[overlap, overlap],
            ),
            (
                ".a:not(:first-child) { padding: 8px; } .a:not(:first-child) { padding: 4px; } .a:not(:last-child) { padding: 2px; }",
                &[overlap, overlap, None],
            ),
        ];
        for &(source, expected) in cases {
            let rules = parse(source);
            let warnings: Vec<_> = rules.iter().map(|rule| rule.warning).collect();
            assert_eq!(warnings, expected, "{source}");
            let actual: Vec<_> = warnings.iter().map(|warning| *warning == overlap).collect();
            assert_eq!(actual, pairwise(&rules), "{source}");
        }
    }

    #[test]
    fn indexed_overlap_matches_pairwise_candidates() {
        let variants = [
            "",
            "hover:",
            "focus:",
            "supports-[display:grid]:",
            "[&:not(:first-child)]:",
            "hover:focus:",
            "focus:hover:",
        ];
        let utilities = [
            "p-2",
            "p-4",
            "pl-2",
            "ps-4",
            "pe-4",
            "-ml-2",
            "ml-2!",
            "[padding-left:3px]",
            "[padding-inline-start:4px]",
            "border-2",
            "border-solid",
            "border-red-500",
            "[border-inline-start-width:3px]",
            "[border-left-width:4px]",
            "[border-left-color:red]",
            "[unknown:1]",
            "[unknown:2]",
            "[other:1]",
            "[display:block]",
            "grid",
            "flex",
            "text-sm",
        ];
        let candidates: Vec<_> = variants
            .iter()
            .flat_map(|variant| {
                utilities
                    .iter()
                    .map(move |utility| format!("{variant}{utility}"))
            })
            .collect();
        let mut rules = parse(
            ".a { display: grid; } .a { display: flex; } .a { display: block; } .b { display: grid; } #a { display: grid; } .a, .b { display: grid; } .a {}",
        );
        for a in &candidates {
            for b in &candidates {
                for (rule, values) in rules.iter_mut().zip([
                    vec![a.clone(), "hover:p-2".into()],
                    vec![b.clone()],
                    vec![a.clone(), b.clone()],
                    vec![a.clone()],
                    vec![b.clone()],
                    vec![a.clone(), b.clone()],
                    vec![],
                ]) {
                    rule.candidates = values;
                    rule.candidates.sort();
                    rule.candidates.dedup();
                }
                assert_eq!(overlapping_rules(&rules), pairwise(&rules), "{a}, {b}");
            }
        }
        assert!(overlapping_rules(&[]).is_empty());
    }

    #[test]
    fn excludes_same_rule_and_identical_spellings_without_losing_other_conflicts() {
        let cases: &[(&[&[&str]], &[bool])] = &[
            (&[&["p-2", "pl-4"]], &[false]),
            (&[&["p-2"], &["p-2"], &["p-2"]], &[false, false, false]),
            (&[&["p-2", "pl-4"], &["p-2"]], &[true, true]),
            (&[&["p-2"], &["p-2"], &["pl-4"]], &[true, true, true]),
            (&[&["ps-2"], &["pe-4"]], &[false, false]),
            (
                &[&["hover:focus:p-2"], &["focus:hover:p-4"]],
                &[false, false],
            ),
            (&[&["[unknown:1]"], &["[unknown:2]"]], &[true, true]),
        ];
        for &(candidates, expected) in cases {
            let mut rules = parse(&".a { display: grid; }".repeat(candidates.len()));
            for (rule, candidates) in rules.iter_mut().zip(candidates) {
                rule.candidates = candidates
                    .iter()
                    .map(|candidate| candidate.to_string())
                    .collect();
            }
            assert_eq!(overlapping_rules(&rules), expected, "{candidates:?}");
            assert_eq!(pairwise(&rules), expected, "{candidates:?}");
        }
    }

    #[test]
    #[ignore = "manual scaling benchmark; run with --ignored --nocapture"]
    fn overlap_scaling() {
        for mode in [
            "distinct-variants",
            "identical",
            "conflicting",
            "distinct-selectors",
        ] {
            for count in [500, 1_000, 2_000] {
                let source: String = (0..count)
                    .map(|index| {
                        let variant = if mode == "distinct-variants" { index } else { 0 };
                        let selector = if mode == "distinct-selectors" { index } else { 0 };
                        let value = if mode == "conflicting" { index + 1 } else { 8 };
                        format!("@supports (width: {variant}px) {{ .a{selector} {{ padding: {value}px; }} }}\n")
                    })
                    .collect();
                let mut times = Vec::new();
                for _ in 0..3 {
                    let start = Instant::now();
                    let rules = parse(&source);
                    times.push(start.elapsed());
                    assert_eq!(rules.len(), count);
                    let expected = (mode == "conflicting").then_some("unsupported-overlap");
                    assert!(rules.iter().all(|rule| rule.warning == expected));
                }
                times.sort();
                eprintln!("{mode}: rules={count}, median={:?}", times[1]);
            }
        }
    }
}

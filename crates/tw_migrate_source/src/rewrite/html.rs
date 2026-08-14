use std::collections::{BTreeSet, HashMap};

use tw_migrate_css::{SelectorKey, utility_conflict};

use crate::{
    CandidateMatch, Edit, HtmlAttribute, SourceFile, SourcePlan, Warning, element_classes,
    element_has_context, element_ids,
};

pub fn plan_html_file(
    file: &SourceFile,
    css_path: &str,
    candidates: &HashMap<SelectorKey, Vec<String>>,
    utility_prefix: Option<&str>,
) -> SourcePlan {
    let contexts = file.analyzable_contexts(css_path);
    if contexts.is_empty() {
        return SourcePlan::default();
    }

    let live_attributes = rebased_attributes(file);
    let mut edits = Vec::new();
    let mut emitted = BTreeSet::new();
    let mut matches = Vec::new();
    let mut module_refs = HashMap::new();
    let mut matched_module_refs = HashMap::new();
    let mut warnings = Vec::new();
    for element in &file.html_elements {
        if !element_has_context(element, css_path) {
            continue;
        }
        let Some(class_attribute) = element
            .class_attribute
            .as_ref()
            .filter(|attribute| attribute.writable)
            .and_then(|attribute| live_attributes.get(&attribute.start))
        else {
            // A read-only effective-root record receives the generated
            // utility through fallthrough at runtime, so the promised
            // conflict warning must still fire against its own classes.
            if let Some((generated, existing)) = readonly_conflict(element, candidates) {
                let span = element
                    .class_attribute
                    .as_ref()
                    .map_or((0, 0), |attribute| (attribute.start, attribute.end));
                warnings.push(Warning::existing_tailwind_conflict(
                    &file.path, span, &generated, &existing,
                ));
            }
            continue;
        };
        let mut additions = Vec::new();

        let quote = attribute_quote(&file.source, class_attribute);
        let classes = element_classes(element);
        for class in &classes {
            let class = (*class).to_string();
            let key = SelectorKey::Class(class.clone());
            let matched = candidates.contains_key(&key);
            if matched {
                *module_refs.entry(class.clone()).or_default() += 1;
            }
            let appended = collect_candidates(
                key,
                class_attribute,
                quote,
                &contexts,
                candidates,
                utility_prefix,
                &mut additions,
                &mut emitted,
                &mut matches,
            );
            // A candidate that cannot be written into this attribute leaves
            // the reference unmigrated, which must block rule removal.
            if matched && appended {
                *matched_module_refs.entry(class).or_default() += 1;
            }
        }
        for id in element_ids(element) {
            collect_candidates(
                SelectorKey::Id(id.to_string()),
                class_attribute,
                quote,
                &contexts,
                candidates,
                utility_prefix,
                &mut additions,
                &mut emitted,
                &mut matches,
            );
        }
        // Parity with the JS rewrite path: a generated utility that overlaps
        // an existing Tailwind class on the rendered element is appended with
        // a warning, and Tailwind's output order decides between them.
        let existing_classes = classes
            .into_iter()
            .chain(class_attribute.value.split_whitespace())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let authored = element
            .class_attribute
            .as_ref()
            .map_or((0, 0), |attribute| (attribute.start, attribute.end));
        push_utility_conflict(
            &mut warnings,
            &file.path,
            authored,
            &additions,
            &existing_classes,
        );
        if let Some(edit) = merge_class_attribute(class_attribute, &additions) {
            edits.push(edit);
        }
    }

    SourcePlan {
        edits,
        candidates: emitted.into_iter().collect(),
        matches,
        module_refs,
        matched_module_refs,
        warnings,
        ..Default::default()
    }
}

/// Collect the appendable candidates for one selector key, returning whether
/// every candidate could be appended. A candidate containing the attribute's
/// own quote delimiter has no writable form inside that attribute value and
/// is skipped, so the caller retains the owning rule instead.
#[allow(clippy::too_many_arguments)]
fn collect_candidates(
    key: SelectorKey,
    attribute: &HtmlAttribute,
    quote: Option<u8>,
    contexts: &[&crate::HtmlStylesheet],
    candidates: &HashMap<SelectorKey, Vec<String>>,
    utility_prefix: Option<&str>,
    additions: &mut Vec<String>,
    emitted: &mut BTreeSet<String>,
    matches: &mut Vec<CandidateMatch>,
) -> bool {
    let Some(origin_candidates) = candidates.get(&key) else {
        return true;
    };
    let mut appended_all = true;
    for origin_candidate in origin_candidates {
        for context in contexts {
            let candidate =
                contextual_candidate(origin_candidate, &context.variants, utility_prefix);
            if candidate_breaks_attribute(&candidate, quote) {
                appended_all = false;
                continue;
            }
            emitted.insert(candidate.clone());
            additions.push(candidate.clone());
            matches.push(CandidateMatch {
                start: attribute.start,
                end: attribute.end,
                key: key.clone(),
                candidate,
                origin_candidate: origin_candidate.clone(),
            });
        }
    }
    appended_all
}

/// Merge the missing additions into a class attribute value, materializing a
/// synthetic attribute, and return the edit when the value changes.
fn merge_class_attribute(attribute: &HtmlAttribute, additions: &[String]) -> Option<Edit> {
    let mut classes = attribute
        .value
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    for candidate in additions {
        if !classes.contains(candidate) {
            classes.push(candidate.clone());
        }
    }
    let value = classes.join(" ");
    let replacement = if attribute.synthetic {
        format!(" class=\"{value}\"")
    } else {
        value
    };
    ((!attribute.synthetic || !classes.is_empty()) && replacement != attribute.value).then_some(
        Edit {
            start: attribute.start,
            end: attribute.end,
            replacement,
        },
    )
}

/// The first (generated, existing) utility conflict on a record whose
/// classes cannot be edited but still receive fallthrough utilities.
fn readonly_conflict(
    element: &crate::HtmlElement,
    candidates: &HashMap<SelectorKey, Vec<String>>,
) -> Option<(String, String)> {
    let classes = element_classes(element);
    let ids = element_ids(element);
    let keys = classes
        .iter()
        .map(|class| SelectorKey::Class((*class).to_string()))
        .chain(ids.iter().map(|id| SelectorKey::Id((*id).to_string())));
    for key in keys {
        let Some(generated) = candidates.get(&key) else {
            continue;
        };
        if let Some(conflict) = utility_conflict(generated, &classes) {
            return Some(conflict);
        }
    }
    None
}

/// The quote delimiter enclosing a live attribute value: the byte before the
/// value span, or `"` for a synthetic attribute the planner itself inserts
/// with double quotes.
fn attribute_quote(source: &str, attribute: &HtmlAttribute) -> Option<u8> {
    if attribute.synthetic {
        return Some(b'"');
    }
    attribute
        .start
        .checked_sub(1)
        .and_then(|index| source.as_bytes().get(index))
        .copied()
        .filter(|byte| matches!(byte, b'"' | b'\''))
}

/// Plan the `<style module>` entry of an SFC: proven `$style` binding sites
/// are rewritten to static classes so fully-referenced module rules can be
/// deleted. Literal class sites belong to the scoped and unscoped entries
/// and are ignored here.
pub fn plan_vue_module_file(
    file: &SourceFile,
    css_path: &str,
    candidates: &HashMap<SelectorKey, Vec<String>>,
    module_rule_classes: Option<&BTreeSet<String>>,
    utility_prefix: Option<&str>,
) -> SourcePlan {
    let contexts = file.analyzable_contexts(css_path);
    if contexts.is_empty() {
        return SourcePlan::default();
    }

    // A direct member naming a class no module rule defines still needs the
    // runtime `$style` injection, which only survives while the block itself
    // has content; converting sibling rules could empty the block, drop the
    // injection, and turn the remaining access into a render error. Retain
    // the whole module instead.
    if let Some(rule_classes) = module_rule_classes {
        let unresolved = file
            .html_elements
            .iter()
            .filter(|element| element_has_context(element, css_path) && !is_shadow_only(element))
            .filter_map(|element| element.module_binding.as_ref())
            .any(|binding| !rule_classes.contains(&binding.name));
        if unresolved {
            let mut plan = SourcePlan::default();
            for element in &file.html_elements {
                if !element_has_context(element, css_path) || is_shadow_only(element) {
                    continue;
                }
                let Some(binding) = &element.module_binding else {
                    continue;
                };
                *plan.module_refs.entry(binding.name.clone()).or_default() += 1;
                if !rule_classes.contains(&binding.name) {
                    plan.warnings.push(Warning::new(
                        "unsupported-css-module-reference",
                        file.path.clone(),
                        (binding.start, binding.end),
                        format!(
                            "`$style.{}` names a class the module does not define, so the module is retained.",
                            binding.name
                        ),
                    ));
                }
            }
            return plan;
        }
    }

    let live_attributes = rebased_attributes(file);
    let mut edits = Vec::new();
    let mut emitted = BTreeSet::new();
    let mut matches = Vec::new();
    let mut module_refs = HashMap::new();
    let mut matched_module_refs = HashMap::new();
    let mut warnings = Vec::new();
    for element in &file.html_elements {
        if !element_has_context(element, css_path) || is_shadow_only(element) {
            continue;
        }
        let Some(binding) = &element.module_binding else {
            continue;
        };
        // Every proven binding is a module reference, matched or not; the
        // deletion machinery must see unmatched ones.
        *module_refs.entry(binding.name.clone()).or_default() += 1;
        let key = SelectorKey::Class(binding.name.clone());
        if !candidates.contains_key(&key) {
            continue;
        }
        let Some(binding_span) = rebase_span(binding.start, binding.end, &file.prior_edits) else {
            continue;
        };
        let mut additions = Vec::new();
        for origin_candidate in &candidates[&key] {
            for context in &contexts {
                let candidate =
                    contextual_candidate(origin_candidate, &context.variants, utility_prefix);
                if !additions.contains(&candidate) {
                    additions.push(candidate);
                }
            }
        }

        let merge_target = element
            .class_attribute
            .as_ref()
            .filter(|attribute| attribute.writable)
            .and_then(|attribute| live_attributes.get(&attribute.start));
        let (site_start, site_end) = match merge_target {
            Some(class_attribute) => {
                if !candidates_fit_attribute(&file.source, class_attribute, &additions) {
                    continue;
                }
                if let Some(edit) = merge_class_attribute(class_attribute, &additions) {
                    edits.push(edit);
                }
                edits.push(Edit {
                    start: binding_span.0,
                    end: binding_span.1,
                    replacement: String::new(),
                });
                (class_attribute.start, class_attribute.end)
            }
            None => {
                // A class attribute that exists but cannot be rewritten (not
                // writable, or no longer live after prior edits) must not be
                // duplicated by a second one; the unmatched reference retains
                // the module instead.
                if element.class_attribute.is_some() {
                    continue;
                }
                // The rewritten attribute is double-quoted.
                if additions.iter().any(|candidate| candidate.contains('"')) {
                    continue;
                }
                // Inline spans include their separator; multiline spans start
                // at the directive so their existing indentation stays intact.
                let separator = if file
                    .source
                    .as_bytes()
                    .get(binding_span.0)
                    .is_some_and(u8::is_ascii_whitespace)
                {
                    " "
                } else {
                    ""
                };
                edits.push(Edit {
                    start: binding_span.0,
                    end: binding_span.1,
                    replacement: format!("{separator}class=\"{}\"", additions.join(" ")),
                });
                (binding_span.0, binding_span.1)
            }
        };
        for candidate in &additions {
            emitted.insert(candidate.clone());
            matches.push(CandidateMatch {
                start: site_start,
                end: site_end,
                key: key.clone(),
                candidate: candidate.clone(),
                origin_candidate: candidate.clone(),
            });
        }
        // Parity with the other rewrite paths: warn when a generated utility
        // overlaps a Tailwind class already on the element. Warnings anchor
        // to authored coordinates, so use the element's analysis-time spans
        // rather than the rebased live site.
        let authored = element
            .class_attribute
            .as_ref()
            .map_or((binding.start, binding.end), |attribute| {
                (attribute.start, attribute.end)
            });
        push_utility_conflict(
            &mut warnings,
            &file.path,
            authored,
            &additions,
            &element_classes(element),
        );
        *matched_module_refs.entry(binding.name.clone()).or_default() += 1;
    }

    SourcePlan {
        edits,
        candidates: emitted.into_iter().collect(),
        matches,
        module_refs,
        matched_module_refs,
        warnings,
        ..Default::default()
    }
}

/// Warn when a generated utility overlaps a Tailwind class already present at
/// the rewrite site; parity across the HTML and Vue module rewrite paths.
fn push_utility_conflict(
    warnings: &mut Vec<Warning>,
    path: &str,
    span: (usize, usize),
    additions: &[String],
    existing: &[&str],
) {
    if let Some((generated, existing)) = utility_conflict(additions, existing) {
        warnings.push(Warning::existing_tailwind_conflict(
            path, span, &generated, &existing,
        ));
    }
}

/// An effective child-root shadow record mirrors a binding that is counted
/// on its own element; it exists only so cascade checks can see the caller's
/// merged classes, marked by its read-only class attribute.
fn is_shadow_only(element: &crate::HtmlElement) -> bool {
    element
        .class_attribute
        .as_ref()
        .is_some_and(|attribute| !attribute.writable)
}

/// Rebase an arbitrary span through the prior sequential edit rounds; `None`
/// when an earlier entry edited inside it.
pub fn rebase_span(start: usize, end: usize, prior_edits: &[Vec<Edit>]) -> Option<(usize, usize)> {
    let mut span = (start, end);
    for edits in prior_edits {
        let (start, end, exact) = shift_span(span.0, span.1, edits)?;
        span = match exact {
            Some(edit) => (start, start + edit.replacement.len()),
            None => (start, end),
        };
    }
    Some(span)
}

pub fn candidates_fit_attribute(
    source: &str,
    attribute: &HtmlAttribute,
    candidates: &[String],
) -> bool {
    let quote = attribute_quote(source, attribute);
    candidates
        .iter()
        .all(|candidate| !candidate_breaks_attribute(candidate, quote))
}

fn candidate_breaks_attribute(candidate: &str, quote: Option<u8>) -> bool {
    match quote {
        Some(quote) => candidate.bytes().any(|byte| byte == quote),
        // An unquoted or unrecognized site cannot safely hold either quote.
        None => candidate.bytes().any(|byte| matches!(byte, b'"' | b'\'')),
    }
}

fn rebased_attributes(file: &SourceFile) -> HashMap<usize, HtmlAttribute> {
    // Only class attributes are queried back out of the returned map, and id
    // attributes are never edited, so they contribute nothing to `delta`.
    let mut attributes = file
        .html_elements
        .iter()
        .filter_map(|element| element.class_attribute.as_ref())
        .filter(|attribute| attribute.writable)
        .filter_map(|attribute| {
            let original_start = attribute.start;
            let mut attribute = attribute.clone();
            for edits in &file.prior_edits {
                attribute = rebase_attribute(attribute, edits)?;
            }
            Some((original_start, attribute))
        })
        .collect::<Vec<_>>();
    attributes.sort_by_key(|(_, attribute)| attribute.start);
    let mut delta = 0isize;
    let mut rebased = HashMap::new();
    for (original_start, attribute) in attributes {
        let Some(start) = attribute.start.checked_add_signed(delta) else {
            continue;
        };
        if attribute.synthetic {
            let Some((live, inserted)) = live_synthetic_class(&file.source, start) else {
                continue;
            };
            delta += inserted as isize;
            rebased.insert(original_start, live);
            continue;
        }
        let Some(end) = live_attribute_end(&file.source, start) else {
            continue;
        };
        let value = file.source[start..end].to_string();
        delta += (end - start) as isize - (attribute.end - attribute.start) as isize;
        rebased.insert(
            original_start,
            HtmlAttribute {
                value,
                start,
                end,
                synthetic: false,
                writable: true,
            },
        );
    }
    rebased
}

/// Shift a span through one edit round; `None` when an edit lands inside it.
/// Returns the exact-cover edit when one replaces the span wholesale.
fn shift_span(start: usize, end: usize, edits: &[Edit]) -> Option<(usize, usize, Option<&Edit>)> {
    let exact = edits
        .iter()
        .find(|edit| edit.start == start && edit.end == end);
    if edits.iter().any(|edit| {
        !(edit.start == start && edit.end == end)
            && edit.start < end
            && (edit.end > start || edit.start == edit.end)
    }) {
        return None;
    }
    let delta = edits
        .iter()
        .filter(|edit| !(edit.start == start && edit.end == end) && edit.end <= start)
        .map(|edit| edit.replacement.len() as isize - (edit.end - edit.start) as isize)
        .sum::<isize>();
    Some((
        start.checked_add_signed(delta)?,
        end.checked_add_signed(delta)?,
        exact,
    ))
}

fn rebase_attribute(mut attribute: HtmlAttribute, edits: &[Edit]) -> Option<HtmlAttribute> {
    let (start, end, exact) = shift_span(attribute.start, attribute.end, edits)?;
    attribute.start = start;
    attribute.end = end;
    if let Some(edit) = exact {
        if attribute.synthetic {
            let value = edit
                .replacement
                .strip_prefix(" class=\"")?
                .strip_suffix('"')?;
            attribute.start += " class=\"".len();
            attribute.end = attribute.start + value.len();
            attribute.value = value.to_string();
            attribute.synthetic = false;
        } else {
            attribute.end = attribute.start + edit.replacement.len();
            attribute.value.clone_from(&edit.replacement);
        }
    }
    Some(attribute)
}

fn live_synthetic_class(source: &str, start: usize) -> Option<(HtmlAttribute, usize)> {
    const PREFIX: &str = " class=\"";
    if !source.get(start..)?.starts_with(PREFIX) {
        return Some((
            HtmlAttribute {
                value: String::new(),
                start,
                end: start,
                synthetic: true,
                writable: true,
            },
            0,
        ));
    }
    let value_start = start + PREFIX.len();
    let value_end = source[value_start..].find('"')? + value_start;
    Some((
        HtmlAttribute {
            value: source[value_start..value_end].to_string(),
            start: value_start,
            end: value_end,
            synthetic: false,
            writable: true,
        },
        value_end + 1 - start,
    ))
}

fn live_attribute_end(source: &str, start: usize) -> Option<usize> {
    if start > source.len() || !source.is_char_boundary(start) {
        return None;
    }
    let quote = start
        .checked_sub(1)
        .and_then(|index| source.as_bytes().get(index));
    if matches!(quote, Some(b'\'' | b'"')) {
        let offset = source.as_bytes()[start..]
            .iter()
            .position(|byte| Some(byte) == quote)?;
        return Some(start + offset);
    }
    let offset = source.as_bytes()[start..]
        .iter()
        .position(|byte| byte.is_ascii_whitespace() || *byte == b'>')
        .unwrap_or(source.len() - start);
    Some(start + offset)
}

fn contextual_candidate(
    candidate: &str,
    variants: &[String],
    utility_prefix: Option<&str>,
) -> String {
    if variants.is_empty() {
        return candidate.to_string();
    }
    let variants = variants.join(":");
    if let Some(prefix) = utility_prefix.filter(|prefix| !prefix.is_empty())
        && let Some(rest) = candidate
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix(':'))
    {
        return format!("{prefix}:{variants}:{rest}");
    }
    format!("{variants}:{candidate}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HtmlElement, HtmlStylesheet};

    fn quoted_fixture(source: &str, value_start: usize, value: &str) -> SourceFile {
        SourceFile {
            path: "/project/index.html".to_string(),
            source: source.to_string(),
            writable: true,
            html_elements: vec![HtmlElement {
                class_attribute: Some(HtmlAttribute {
                    value: value.to_string(),
                    start: value_start,
                    end: value_start + value.len(),
                    synthetic: false,
                    writable: true,
                }),
                id_attribute: None,
                match_classes: None,
                match_ids: None,
                match_tag: None,
                tag: None,
                module_binding: None,
                css_paths: Vec::new(),
            }],
            html_stylesheets: vec![HtmlStylesheet {
                css_path: "/project/site.css".to_string(),
                variants: Vec::new(),
                direct: true,
                analyzable: true,
            }],
            html_references_safe: true,
            html_script_text: String::new(),
            prior_edits: Vec::new(),
        }
    }

    #[test]
    fn skips_candidates_containing_the_enclosing_quote_and_blocks_removal() {
        let source = "<div class=\"card\"></div>";
        let file = quoted_fixture(source, source.find("card").unwrap(), "card");
        let candidates = HashMap::from([(
            SelectorKey::Class("card".to_string()),
            vec![
                "[font-family:\"My_Font\"]".to_string(),
                "p-[13px]".to_string(),
            ],
        )]);
        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);
        // The quote-bearing candidate is withheld; the safe one still lands.
        assert_eq!(plan.candidates, vec!["p-[13px]".to_string()]);
        assert_eq!(plan.edits.len(), 1);
        assert!(!plan.edits[0].replacement.contains("My_Font"));
        // The reference stays partially unmigrated, so removal must stay blocked.
        assert_eq!(plan.module_refs.get("card"), Some(&1));
        assert_eq!(plan.matched_module_refs.get("card"), None);
    }

    #[test]
    fn warns_when_a_generated_utility_conflicts_with_an_existing_class() {
        let source = "<div class=\"card p-4\"></div>";
        let file = quoted_fixture(source, source.find("card").unwrap(), "card p-4");
        let candidates = HashMap::from([(
            SelectorKey::Class("card".to_string()),
            vec!["p-[13px]".to_string()],
        )]);
        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);
        // Parity with the JS path: append with a warning, do not block.
        assert_eq!(plan.edits.len(), 1);
        assert_eq!(plan.edits[0].replacement, "card p-4 p-[13px]");
        assert_eq!(plan.warnings.len(), 1);
        assert_eq!(plan.warnings[0].code, "existing-tailwind-conflict");
    }

    #[test]
    fn appends_double_quoted_candidates_inside_single_quoted_attributes() {
        let source = "<div class='card'></div>";
        let file = quoted_fixture(source, source.find("card").unwrap(), "card");
        let candidates = HashMap::from([(
            SelectorKey::Class("card".to_string()),
            vec!["[font-family:\"My_Font\"]".to_string()],
        )]);
        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);
        assert_eq!(plan.edits.len(), 1);
        assert_eq!(plan.edits[0].replacement, "card [font-family:\"My_Font\"]");
        assert_eq!(plan.matched_module_refs.get("card"), Some(&1));
    }

    #[test]
    fn preserves_html_bytes_around_literal_value_edits() {
        let file = SourceFile {
            path: "/project/index.html".to_string(),
            source: "<main class='card featured' id=hero></main>".to_string(),
            writable: true,
            html_elements: vec![HtmlElement {
                class_attribute: Some(HtmlAttribute {
                    value: "card featured".to_string(),
                    start: 13,
                    end: 26,
                    synthetic: false,
                    writable: true,
                }),
                id_attribute: Some(HtmlAttribute {
                    value: "hero".to_string(),
                    start: 31,
                    end: 35,
                    synthetic: false,
                    writable: true,
                }),
                match_classes: None,
                match_ids: None,
                match_tag: None,
                tag: None,
                module_binding: None,
                css_paths: Vec::new(),
            }],
            html_stylesheets: vec![HtmlStylesheet {
                css_path: "/project/site.css".to_string(),
                variants: vec!["print".to_string()],
                direct: true,
                analyzable: true,
            }],
            html_references_safe: true,
            html_script_text: String::new(),
            prior_edits: Vec::new(),
        };
        let candidates = HashMap::from([
            (
                SelectorKey::Class("card".to_string()),
                vec!["p-4".to_string()],
            ),
            (
                SelectorKey::Id("hero".to_string()),
                vec!["h-screen".to_string()],
            ),
        ]);
        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);
        let edit = &plan.edits[0];
        let mut output = file.source.clone();
        output.replace_range(edit.start..edit.end, &edit.replacement);
        assert_eq!(
            output,
            "<main class='card featured print:p-4 print:h-screen' id=hero></main>"
        );
    }
}

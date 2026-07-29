use std::collections::{BTreeSet, HashMap};

use crate::{
    css_plan::SelectorKey,
    js_rewrite::{CandidateMatch, SourcePlan},
    planner::{element_classes, element_ids, Edit, HtmlAttribute, SourceFile, Warning},
    utilities::tailwind_utilities_conflict,
};

pub(crate) fn plan_html_file(
    file: &SourceFile,
    css_path: &str,
    candidates: &HashMap<SelectorKey, Vec<String>>,
    utility_prefix: Option<&str>,
) -> SourcePlan {
    let contexts = file
        .html_stylesheets
        .iter()
        .filter(|context| context.analyzable && context.css_path == css_path)
        .collect::<Vec<_>>();
    if contexts.is_empty() {
        return empty_plan();
    }

    let live_attributes = rebased_attributes(file);
    let mut edits = Vec::new();
    let mut emitted = BTreeSet::new();
    let mut matches = Vec::new();
    let mut module_refs = HashMap::new();
    let mut matched_module_refs = HashMap::new();
    let mut warnings = Vec::new();
    for element in &file.html_elements {
        if !element.css_paths.is_empty()
            && !element.css_paths.iter().any(|path| path == css_path)
        {
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
                    .map(|attribute| (attribute.start, attribute.end))
                    .unwrap_or((0, 0));
                warnings.push(Warning {
                    code: "existing-tailwind-conflict",
                    file: file.path.clone(),
                    start: span.0,
                    end: span.1,
                    message: format!(
                        "Generated utility `{generated}` may conflict with existing `{existing}`."
                    ),
                });
            }
            continue;
        };
        let mut classes = class_attribute
            .value
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut additions = Vec::new();

        let quote = attribute_quote(&file.source, class_attribute);
        for class in element_classes(element) {
            let class = class.to_string();
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
        let existing_classes = element_classes(element)
            .into_iter()
            .chain(classes.iter().map(String::as_str))
            .collect::<BTreeSet<_>>();
        if let Some((generated, existing)) = additions.iter().find_map(|candidate| {
            existing_classes
                .iter()
                .find(|existing| tailwind_utilities_conflict(candidate, existing))
                .map(|existing| (candidate.clone(), (*existing).to_string()))
        }) {
            let authored = element
                .class_attribute
                .as_ref()
                .map(|attribute| (attribute.start, attribute.end))
                .unwrap_or((0, 0));
            warnings.push(Warning {
                code: "existing-tailwind-conflict",
                file: file.path.clone(),
                start: authored.0,
                end: authored.1,
                message: format!(
                    "Generated utility `{generated}` may conflict with existing `{existing}`."
                ),
            });
        }
        for candidate in additions {
            if !classes.contains(&candidate) {
                classes.push(candidate);
            }
        }
        let value = classes.join(" ");
        let replacement = if class_attribute.synthetic {
            format!(" class=\"{value}\"")
        } else {
            value
        };
        if (!class_attribute.synthetic || !classes.is_empty())
            && replacement != class_attribute.value
        {
            edits.push(Edit {
                start: class_attribute.start,
                end: class_attribute.end,
                replacement,
            });
        }
    }

    SourcePlan {
        edits,
        removable_import_edits: Vec::new(),
        candidates: emitted.into_iter().collect(),
        matches,
        module_refs,
        matched_module_refs,
        module_references_safe: true,
        warnings,
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
    contexts: &[&crate::planner::HtmlStylesheet],
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

/// The quote delimiter enclosing a live attribute value: the byte before the
/// value span, or `"` for a synthetic attribute the planner itself inserts
/// with double quotes.
/// The first (generated, existing) utility conflict on a record whose
/// classes cannot be edited but still receive fallthrough utilities.
fn readonly_conflict(
    element: &crate::planner::HtmlElement,
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
        for candidate in generated {
            if let Some(existing) = classes
                .iter()
                .find(|existing| tailwind_utilities_conflict(candidate, existing))
            {
                return Some((candidate.clone(), (*existing).to_string()));
            }
        }
    }
    None
}

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

pub(crate) fn candidates_fit_attribute(
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
    let mut attributes = file
        .html_elements
        .iter()
        .flat_map(|element| {
            [
                element.class_attribute.as_ref(),
                element.id_attribute.as_ref(),
            ]
            .into_iter()
            .flatten()
            .filter(|attribute| attribute.writable)
        })
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

fn rebase_attribute(mut attribute: HtmlAttribute, edits: &[Edit]) -> Option<HtmlAttribute> {
    let original_start = attribute.start;
    let original_end = attribute.end;
    let exact = edits
        .iter()
        .find(|edit| edit.start == original_start && edit.end == original_end);
    if edits.iter().any(|edit| {
        !(edit.start == original_start && edit.end == original_end)
            && edit.start < original_end
            && (edit.end > original_start || edit.start == edit.end)
    }) {
        return None;
    }
    let delta = edits
        .iter()
        .filter(|edit| {
            !(edit.start == original_start && edit.end == original_end)
                && edit.end <= original_start
        })
        .map(|edit| edit.replacement.len() as isize - (edit.end - edit.start) as isize)
        .sum::<isize>();
    attribute.start = original_start.checked_add_signed(delta)?;
    attribute.end = original_end.checked_add_signed(delta)?;
    if let Some(edit) = exact {
        if attribute.synthetic {
            let value = edit.replacement.strip_prefix(" class=\"")?.strip_suffix('"')?;
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
        && let Some(rest) = candidate.strip_prefix(&format!("{prefix}:"))
    {
        return format!("{prefix}:{variants}:{rest}");
    }
    format!("{variants}:{candidate}")
}

pub(crate) fn empty_source_plan() -> SourcePlan {
    empty_plan()
}

fn empty_plan() -> SourcePlan {
    SourcePlan {
        edits: Vec::new(),
        removable_import_edits: Vec::new(),
        candidates: Vec::new(),
        matches: Vec::new(),
        module_refs: HashMap::new(),
        matched_module_refs: HashMap::new(),
        module_references_safe: true,
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::{HtmlElement, HtmlStylesheet};

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
            vec!["[font-family:\"My_Font\"]".to_string(), "p-[13px]".to_string()],
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

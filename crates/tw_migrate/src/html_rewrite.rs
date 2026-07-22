use std::collections::{BTreeSet, HashMap};

use crate::{
    css_plan::SelectorKey,
    js_rewrite::{CandidateMatch, SourcePlan},
    planner::{Edit, HtmlAttribute, SourceFile},
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
        .filter(|context| context.css_path == css_path)
        .collect::<Vec<_>>();
    if contexts.is_empty() {
        return empty_plan();
    }

    let live_attributes = rebased_attributes(file);
    let mut edits = Vec::new();
    let mut emitted = BTreeSet::new();
    let mut matches = Vec::new();
    for element in &file.html_elements {
        let Some(class_attribute) = element
            .class_attribute
            .as_ref()
            .and_then(|attribute| live_attributes.get(&attribute.start))
        else {
            continue;
        };
        let mut classes = class_attribute
            .value
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut additions = Vec::new();

        for class in classes.clone() {
            collect_candidates(
                SelectorKey::Class(class),
                class_attribute,
                &contexts,
                candidates,
                utility_prefix,
                &mut additions,
                &mut emitted,
                &mut matches,
            );
        }
        if let Some(id) = element
            .id_attribute
            .as_ref()
            .and_then(|attribute| live_attributes.get(&attribute.start))
        {
            collect_candidates(
                SelectorKey::Id(id.value.clone()),
                class_attribute,
                &contexts,
                candidates,
                utility_prefix,
                &mut additions,
                &mut emitted,
                &mut matches,
            );
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
        if replacement != class_attribute.value {
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
        module_refs: HashMap::new(),
        matched_module_refs: HashMap::new(),
        module_references_safe: true,
        warnings: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_candidates(
    key: SelectorKey,
    attribute: &HtmlAttribute,
    contexts: &[&crate::planner::HtmlStylesheet],
    candidates: &HashMap<SelectorKey, Vec<String>>,
    utility_prefix: Option<&str>,
    additions: &mut Vec<String>,
    emitted: &mut BTreeSet<String>,
    matches: &mut Vec<CandidateMatch>,
) {
    let Some(origin_candidates) = candidates.get(&key) else {
        return;
    };
    for origin_candidate in origin_candidates {
        for context in contexts {
            let candidate =
                contextual_candidate(origin_candidate, &context.variants, utility_prefix);
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
        })
        .collect::<Vec<_>>();
    attributes.sort_by_key(|attribute| attribute.start);
    let mut delta = 0isize;
    let mut rebased = HashMap::new();
    for attribute in attributes {
        let Some(start) = attribute.start.checked_add_signed(delta) else {
            continue;
        };
        if attribute.synthetic {
            let Some((live, inserted)) = live_synthetic_class(&file.source, start) else {
                continue;
            };
            delta += inserted as isize;
            rebased.insert(attribute.start, live);
            continue;
        }
        let Some(end) = live_attribute_end(&file.source, start) else {
            continue;
        };
        let value = file.source[start..end].to_string();
        delta += (end - start) as isize - (attribute.end - attribute.start) as isize;
        rebased.insert(
            attribute.start,
            HtmlAttribute {
                value,
                start,
                end,
                synthetic: false,
            },
        );
    }
    rebased
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
                }),
                id_attribute: Some(HtmlAttribute {
                    value: "hero".to_string(),
                    start: 31,
                    end: 35,
                    synthetic: false,
                }),
            }],
            html_stylesheets: vec![HtmlStylesheet {
                css_path: "/project/site.css".to_string(),
                variants: vec!["print".to_string()],
            }],
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

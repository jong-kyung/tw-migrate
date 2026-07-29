use std::collections::{BTreeSet, HashMap};

use crate::{
    css_plan::SelectorKey,
    js_rewrite::{CandidateMatch, SourcePlan},
    planner::{
        class_tokens, element_classes, element_ids, writable_element_classes, Edit, HtmlAttribute,
        SourceFile, Warning,
    },
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
        let writable_classes = writable_element_classes(element);
        let js_site = directive_site(&file.source, class_attribute);
        if has_directive_metadata(class_attribute) && js_site.is_none() {
            for class in writable_classes {
                if candidates.contains_key(&SelectorKey::Class(class.to_string())) {
                    *module_refs.entry(class.to_string()).or_default() += 1;
                }
            }
            continue;
        }
        let mut classes = class_tokens(&class_attribute.value)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut additions = Vec::new();

        let quote = attribute_outer_quote(&file.source, class_attribute);
        for class in writable_classes {
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
                element.node_start,
                js_site.is_some(),
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
        if element
            .write_classes
            .as_ref()
            .is_none_or(Vec::is_empty)
        {
            for id in element_ids(element) {
                collect_candidates(
                    SelectorKey::Id(id.to_string()),
                    class_attribute,
                    quote,
                    &contexts,
                    candidates,
                    utility_prefix,
                    element.node_start,
                    js_site.is_some(),
                    &mut additions,
                    &mut emitted,
                    &mut matches,
                );
            }
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
        let mut new_candidates = Vec::new();
        for candidate in additions {
            if !classes.contains(&candidate) && !new_candidates.contains(&candidate) {
                new_candidates.push(candidate);
            }
        }
        classes.extend(new_candidates.iter().cloned());
        let value = classes.join(" ");
        let replacement = if class_attribute.synthetic {
            format!(" class=\"{value}\"")
        } else if let Some((js_quote, raw)) = js_site {
            render_js_site(
                raw,
                &class_attribute.value,
                js_quote,
                class_attribute.quote_key,
                class_attribute.object_shorthand,
                &new_candidates,
            )
        } else {
            value
        };
        if (!class_attribute.synthetic || !classes.is_empty())
            && (!new_candidates.is_empty() || class_attribute.js_quote.is_none())
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
    element_start: Option<usize>,
    directive: bool,
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
            if candidate_breaks_attribute(&candidate, quote)
                || (directive && candidate.contains('&'))
            {
                appended_all = false;
                continue;
            }
            emitted.insert(candidate.clone());
            additions.push(candidate.clone());
            matches.push(CandidateMatch {
                start: attribute.start,
                end: attribute.end,
                element_start,
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

fn attribute_outer_quote(source: &str, attribute: &HtmlAttribute) -> Option<u8> {
    if let Some(quote) = attribute.html_quote.as_deref() {
        return single_quote_byte(quote);
    }
    if attribute.js_quote.is_some() {
        return None;
    }
    attribute_quote(source, attribute)
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
    if has_directive_metadata(attribute) {
        let Some((_js_quote, _raw)) = directive_site(source, attribute) else {
            return false;
        };
        return candidates.iter().all(|candidate| {
            !candidate.contains('&')
                && !candidate_breaks_attribute(
                    candidate,
                    attribute.html_quote.as_deref().and_then(single_quote_byte),
                )
        });
    }
    let quote = attribute_quote(source, attribute);
    candidates
        .iter()
        .all(|candidate| !candidate_breaks_attribute(candidate, quote))
}

fn has_directive_metadata(attribute: &HtmlAttribute) -> bool {
    attribute.js_quote.is_some()
        || attribute.html_quote.is_some()
        || attribute.raw_value.is_some()
        || attribute.quote_key
        || attribute.object_shorthand
}

fn directive_site<'a>(source: &str, attribute: &'a HtmlAttribute) -> Option<(u8, &'a str)> {
    let js_quote = single_quote_byte(attribute.js_quote.as_deref()?)?;
    let html_quote = single_quote_byte(attribute.html_quote.as_deref()?)?;
    if !matches!(html_quote, b'\'' | b'"') || js_quote == html_quote {
        return None;
    }
    let raw = attribute.raw_value.as_deref()?;
    (source.get(attribute.start..attribute.end) == Some(raw)).then_some((js_quote, raw))
}

fn single_quote_byte(value: &str) -> Option<u8> {
    let bytes = value.as_bytes();
    (bytes.len() == 1 && matches!(bytes[0], b'\'' | b'"' | b'`')).then_some(bytes[0])
}

fn render_js_site(
    raw: &str,
    runtime_value: &str,
    quote: u8,
    quote_key: bool,
    object_shorthand: bool,
    additions: &[String],
) -> String {
    let mut value = if quote_key { runtime_value } else { raw }.to_string();
    for candidate in additions {
        value.push(' ');
        value.push_str(&escape_js_content(candidate, quote));
    }
    if !quote_key {
        return value;
    }
    let quoted = format!("{}{}{}", char::from(quote), value, char::from(quote));
    if object_shorthand {
        format!("{quoted}: {raw}")
    } else {
        quoted
    }
}

fn escape_js_content(value: &str, quote: u8) -> String {
    let mut escaped = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\'
            || character as u32 == u32::from(quote)
            || (quote == b'`' && character == '$' && chars.peek() == Some(&'{'))
        {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn appended_runtime_value(
    attribute: &HtmlAttribute,
    replacement: &str,
    quote: u8,
) -> Option<String> {
    let raw = attribute.raw_value.as_deref()?;
    let prefix = if attribute.quote_key {
        attribute.value.as_str()
    } else {
        raw
    };
    let suffix = replacement.strip_prefix(prefix)?;
    if suffix.is_empty() {
        return Some(attribute.value.clone());
    }
    let mut value = attribute.value.clone();
    for candidate in suffix.strip_prefix(' ')?.split(' ') {
        value.push(' ');
        value.push_str(&unescape_js_content(candidate, quote)?);
    }
    Some(value)
}

fn unescape_js_content(value: &str, quote: u8) -> Option<String> {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        let escaped = chars.next()?;
        if escaped == '\\'
            || escaped as u32 == u32::from(quote)
            || (quote == b'`' && matches!(escaped, '`' | '$'))
        {
            output.push(escaped);
        } else {
            return None;
        }
    }
    Some(output)
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
        let end = if attribute.js_quote.is_some() {
            start.checked_add(attribute.end - attribute.start)
        } else {
            live_attribute_end(&file.source, start)
        };
        let Some(end) = end.filter(|end| *end <= file.source.len()) else {
            continue;
        };
        let Some(raw) = file.source.get(start..end).map(str::to_string) else {
            continue;
        };
        delta += (end - start) as isize - (attribute.end - attribute.start) as isize;
        let value = if attribute.js_quote.is_some() {
            attribute.value.clone()
        } else {
            raw.clone()
        };
        rebased.insert(
            original_start,
            HtmlAttribute {
                value,
                start,
                end,
                synthetic: false,
                writable: true,
                raw_value: attribute.raw_value,
                js_quote: attribute.js_quote,
                html_quote: attribute.html_quote,
                quote_key: attribute.quote_key,
                object_shorthand: attribute.object_shorthand,
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
        } else if let Some(quote) = attribute
            .js_quote
            .as_deref()
            .and_then(single_quote_byte)
        {
            let replacement = if attribute.quote_key {
                let shorthand_suffix = if attribute.object_shorthand {
                    Some(format!(": {}", attribute.raw_value.as_deref()?))
                } else {
                    None
                };
                let quoted = shorthand_suffix
                    .as_deref()
                    .map_or(edit.replacement.as_str(), |suffix| {
                        edit.replacement.strip_suffix(suffix).unwrap_or("")
                    });
                let quoted = quoted.as_bytes();
                if quoted.first() != Some(&quote) || quoted.last() != Some(&quote) {
                    return None;
                }
                let content_end = edit.replacement.len()
                    - shorthand_suffix.as_deref().map_or(0, str::len)
                    - 1;
                &edit.replacement[1..content_end]
            } else {
                edit.replacement.as_str()
            };
            attribute.value = appended_runtime_value(&attribute, replacement, quote)?;
            if attribute.quote_key {
                attribute.start += 1;
                attribute.quote_key = false;
                attribute.object_shorthand = false;
            }
            attribute.raw_value = Some(replacement.to_string());
            attribute.end = attribute.start + replacement.len();
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
                raw_value: None,
                js_quote: None,
                html_quote: None,
                quote_key: false,
                object_shorthand: false,
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
            raw_value: None,
            js_quote: None,
            html_quote: None,
            quote_key: false,
            object_shorthand: false,
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
                    raw_value: None,
                    js_quote: None,
                    html_quote: None,
                    quote_key: false,
                    object_shorthand: false,
                }),
                id_attribute: None,
                node_start: None,
                match_classes: None,
                write_classes: None,
                match_ids: None,
                match_tag: None,
                tag: None,
                css_paths: Vec::new(),
                class_opaque: false,
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
    fn treats_only_dom_ascii_whitespace_as_class_separators() {
        let source = "<div class=\"a\u{a0}b\"></div>";
        let value = "a\u{a0}b";
        let file = quoted_fixture(source, source.find(value).unwrap(), value);
        let candidates = HashMap::from([(
            SelectorKey::Class("a".to_string()),
            vec!["m-2".to_string()],
        )]);

        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);

        assert!(plan.edits.is_empty());
        assert!(plan.module_refs.is_empty());
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
    fn keeps_conditional_class_candidates_on_their_own_sites() {
        let source = r#"<p :class="{ first: a, second: b }"></p>"#;
        let first = source.find("first").unwrap();
        let second = source.find("second").unwrap();
        let site = |value: &str, start| HtmlElement {
            class_attribute: Some(HtmlAttribute {
                value: value.to_string(),
                start,
                end: start + value.len(),
                synthetic: false,
                writable: true,
                raw_value: Some(value.to_string()),
                js_quote: Some("'".to_string()),
                html_quote: Some("\"".to_string()),
                quote_key: true,
                object_shorthand: false,
            }),
            id_attribute: None,
            node_start: Some(source.find("<p").unwrap()),
            match_classes: Some(vec!["first".to_string(), "second".to_string()]),
            write_classes: Some(vec![value.to_string()]),
            match_ids: None,
            match_tag: None,
            tag: Some("p".to_string()),
            css_paths: Vec::new(),
            class_opaque: false,
        };
        let mut file = quoted_fixture(source, first, "first");
        file.html_elements = vec![site("first", first), site("second", second)];
        let candidates = HashMap::from([
            (SelectorKey::Class("first".to_string()), vec!["p-4".to_string()]),
            (SelectorKey::Class("second".to_string()), vec!["m-2".to_string()]),
        ]);

        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);

        assert_eq!(plan.edits.len(), 2);
        assert_eq!(plan.edits[0].replacement, "'first p-4'");
        assert_eq!(plan.edits[1].replacement, "'second m-2'");
        assert_eq!(plan.matches[0].start, first);
        assert_eq!(plan.matches[0].end, first + "first".len());
        assert_eq!(plan.matches[0].element_start, Some(source.find("<p").unwrap()));
        assert_eq!(plan.matches[1].start, second);
        assert_eq!(plan.matches[1].end, second + "second".len());
        assert_eq!(plan.matches[1].element_start, Some(source.find("<p").unwrap()));
    }

    #[test]
    fn rejects_object_key_quotes_that_match_the_html_attribute() {
        let source = r#"<p :class="{ btn: ok }"></p>"#;
        let btn = source.find("btn").unwrap();
        let mut file = quoted_fixture(source, btn, "btn");
        file.html_elements = vec![HtmlElement {
            class_attribute: Some(HtmlAttribute {
                value: "btn".to_string(),
                start: btn,
                end: btn + "btn".len(),
                synthetic: false,
                writable: true,
                raw_value: Some("btn".to_string()),
                js_quote: Some("\"".to_string()),
                html_quote: Some("\"".to_string()),
                quote_key: true,
                object_shorthand: false,
            }),
            id_attribute: None,
            node_start: None,
            match_classes: None,
            write_classes: Some(vec!["btn".to_string()]),
            match_ids: None,
            match_tag: None,
            tag: Some("p".to_string()),
            css_paths: Vec::new(),
            class_opaque: false,
        }];
        let candidates = HashMap::from([(
            SelectorKey::Class("btn".to_string()),
            vec!["p-4".to_string()],
        )]);

        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);

        assert!(plan.edits.is_empty());
        assert!(plan.matches.is_empty());
        assert_eq!(plan.module_refs.get("btn"), Some(&1));
        assert_eq!(plan.matched_module_refs.get("btn"), None);
    }

    #[test]
    fn rejects_html_entity_candidates_inside_directive_expressions() {
        let source = r#"<p :class="['btn']"></p>"#;
        let btn = source.find("btn").unwrap();
        let mut file = quoted_fixture(source, btn, "btn");
        file.html_elements = vec![HtmlElement {
            class_attribute: Some(HtmlAttribute {
                value: "btn".to_string(),
                start: btn,
                end: btn + "btn".len(),
                synthetic: false,
                writable: true,
                raw_value: Some("btn".to_string()),
                js_quote: Some("'".to_string()),
                html_quote: Some("\"".to_string()),
                quote_key: false,
                object_shorthand: false,
            }),
            id_attribute: None,
            node_start: None,
            match_classes: None,
            write_classes: Some(vec!["btn".to_string()]),
            match_ids: None,
            match_tag: None,
            tag: Some("p".to_string()),
            css_paths: Vec::new(),
            class_opaque: false,
        }];
        let candidates = HashMap::from([(
            SelectorKey::Class("btn".to_string()),
            vec!["content-['&quot;']".to_string()],
        )]);

        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);

        assert!(plan.edits.is_empty());
        assert!(plan.matches.is_empty());
        assert_eq!(plan.module_refs.get("btn"), Some(&1));
        assert_eq!(plan.matched_module_refs.get("btn"), None);
    }

    #[test]
    fn invalid_directive_metadata_never_counts_as_a_matched_reference() {
        let source = r#"<p :class="['btn']"></p>"#;
        let btn = source.find("btn").unwrap();
        let mut file = quoted_fixture(source, btn, "btn");
        file.html_elements = vec![HtmlElement {
            class_attribute: Some(HtmlAttribute {
                value: "btn".to_string(),
                start: btn,
                end: btn + "btn".len(),
                synthetic: false,
                writable: true,
                raw_value: None,
                js_quote: Some("'".to_string()),
                html_quote: Some("\"".to_string()),
                quote_key: false,
                object_shorthand: false,
            }),
            id_attribute: None,
            node_start: None,
            match_classes: None,
            write_classes: Some(vec!["btn".to_string()]),
            match_ids: None,
            match_tag: None,
            tag: Some("p".to_string()),
            css_paths: Vec::new(),
            class_opaque: false,
        }];
        let candidates = HashMap::from([(
            SelectorKey::Class("btn".to_string()),
            vec!["p-4".to_string()],
        )]);

        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);

        assert!(plan.edits.is_empty());
        assert!(plan.matches.is_empty());
        assert_eq!(plan.module_refs.get("btn"), Some(&1));
        assert_eq!(plan.matched_module_refs.get("btn"), None);
    }

    #[test]
    fn a_distinct_unconditional_site_receives_id_candidates_only() {
        let source = r#"<p id="hero" :class="{ btn: ok }"></p>"#;
        let hero = source.find("hero").unwrap();
        let btn = source.find("btn").unwrap();
        let insertion = source.find(" :class").unwrap();
        let id = HtmlAttribute {
            value: "hero".to_string(),
            start: hero,
            end: hero + "hero".len(),
            synthetic: false,
            writable: true,
            raw_value: None,
            js_quote: None,
            html_quote: None,
            quote_key: false,
            object_shorthand: false,
        };
        let conditional = HtmlElement {
            class_attribute: Some(HtmlAttribute {
                value: "btn".to_string(),
                start: btn,
                end: btn + "btn".len(),
                synthetic: false,
                writable: true,
                raw_value: Some("btn".to_string()),
                js_quote: Some("'".to_string()),
                html_quote: Some("\"".to_string()),
                quote_key: true,
                object_shorthand: false,
            }),
            id_attribute: Some(id.clone()),
            node_start: Some(source.find("<p").unwrap()),
            match_classes: Some(vec!["btn".to_string()]),
            write_classes: Some(vec!["btn".to_string()]),
            match_ids: Some(vec!["hero".to_string()]),
            match_tag: None,
            tag: Some("p".to_string()),
            css_paths: Vec::new(),
            class_opaque: false,
        };
        let unconditional = HtmlElement {
            class_attribute: Some(HtmlAttribute {
                value: String::new(),
                start: insertion,
                end: insertion,
                synthetic: true,
                writable: true,
                raw_value: None,
                js_quote: None,
                html_quote: None,
                quote_key: false,
                object_shorthand: false,
            }),
            id_attribute: Some(id),
            node_start: Some(source.find("<p").unwrap()),
            match_classes: Some(vec!["btn".to_string()]),
            write_classes: Some(Vec::new()),
            match_ids: Some(vec!["hero".to_string()]),
            match_tag: None,
            tag: Some("p".to_string()),
            css_paths: Vec::new(),
            class_opaque: false,
        };
        let mut file = quoted_fixture(source, btn, "btn");
        file.html_elements = vec![conditional, unconditional];
        let candidates = HashMap::from([
            (SelectorKey::Class("btn".to_string()), vec!["p-4".to_string()]),
            (SelectorKey::Id("hero".to_string()), vec!["m-2".to_string()]),
        ]);

        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);

        assert_eq!(plan.edits.len(), 2);
        assert_eq!(
            plan.edits
                .iter()
                .find(|edit| edit.start == insertion)
                .unwrap()
                .replacement,
            " class=\"m-2\""
        );
        assert_eq!(
            plan.edits.iter().find(|edit| edit.start == btn).unwrap().replacement,
            "'btn p-4'"
        );
        assert!(plan
            .edits
            .iter()
            .all(|edit| !edit.replacement.contains("p-4 m-2")));
    }

    #[test]
    fn escapes_javascript_literal_delimiters_without_touching_authored_content() {
        let source = r#"<p :class="['card\\x', `tick`] "></p>"#;
        let card = source.find("card").unwrap();
        let tick = source.find("tick").unwrap();
        let mut file = quoted_fixture(source, card, "card\\x");
        file.html_elements = vec![
            HtmlElement {
                class_attribute: Some(HtmlAttribute {
                    value: "card\\x".to_string(),
                    start: card,
                    end: card + r"card\\x".len(),
                    synthetic: false,
                    writable: true,
                    raw_value: Some(r"card\\x".to_string()),
                    js_quote: Some("'".to_string()),
                    html_quote: Some("\"".to_string()),
                    quote_key: false,
                    object_shorthand: false,
                }),
                id_attribute: None,
                node_start: None,
                match_classes: None,
                write_classes: Some(vec!["card\\x".to_string()]),
                match_ids: None,
                match_tag: None,
                tag: Some("p".to_string()),
                css_paths: Vec::new(),
                class_opaque: false,
            },
            HtmlElement {
                class_attribute: Some(HtmlAttribute {
                    value: "tick".to_string(),
                    start: tick,
                    end: tick + "tick".len(),
                    synthetic: false,
                    writable: true,
                    raw_value: Some("tick".to_string()),
                    js_quote: Some("`".to_string()),
                    html_quote: Some("\"".to_string()),
                    quote_key: false,
                    object_shorthand: false,
                }),
                id_attribute: None,
                node_start: None,
                match_classes: None,
                write_classes: Some(vec!["tick".to_string()]),
                match_ids: None,
                match_tag: None,
                tag: Some("p".to_string()),
                css_paths: Vec::new(),
                class_opaque: false,
            },
        ];
        let candidates = HashMap::from([
            (
                SelectorKey::Class("card\\x".to_string()),
                vec![
                    "content-['x']".to_string(),
                    "content-[\"blocked\"]".to_string(),
                ],
            ),
            (
                SelectorKey::Class("tick".to_string()),
                vec!["content-[`${x}`]".to_string()],
            ),
        ]);

        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);

        assert_eq!(plan.edits[0].replacement, r"card\\x content-[\'x\']");
        assert!(!plan.edits[0].replacement.contains("blocked"));
        assert_eq!(plan.edits[1].replacement, r"tick content-[\`\${x}\`]" );
        assert_eq!(plan.matched_module_refs.get("card\\x"), None);
    }

    #[test]
    fn preserves_object_shorthand_values_when_quoting_keys() {
        let source = r#"<p :class="{ active }"></p>"#;
        let start = source.find("active").unwrap();
        let mut file = quoted_fixture(source, start, "active");
        file.html_elements[0].class_attribute = Some(HtmlAttribute {
            value: "active".to_string(),
            start,
            end: start + "active".len(),
            synthetic: false,
            writable: true,
            raw_value: Some("active".to_string()),
            js_quote: Some("'".to_string()),
            html_quote: Some("\"".to_string()),
            quote_key: true,
            object_shorthand: true,
        });
        file.html_elements[0].write_classes = Some(vec!["active".to_string()]);
        let candidates = HashMap::from([(
            SelectorKey::Class("active".to_string()),
            vec!["m-2".to_string()],
        )]);

        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);
        assert_eq!(plan.edits[0].replacement, "'active m-2': active");
        let rebased = rebase_attribute(
            file.html_elements[0].class_attribute.clone().unwrap(),
            &plan.edits,
        )
        .unwrap();
        assert_eq!(rebased.value, "active m-2");
        assert_eq!(rebased.raw_value.as_deref(), Some("active m-2"));
        assert!(!rebased.object_shorthand);
    }

    #[test]
    fn canonicalizes_numeric_object_keys_before_rebasing() {
        let source = r#"<p :class="{ [0x10]: on }"></p>"#;
        let start = source.find("0x10").unwrap();
        let mut file = quoted_fixture(source, start, "16");
        file.html_elements[0].class_attribute = Some(HtmlAttribute {
            value: "16".to_string(),
            start,
            end: start + "0x10".len(),
            synthetic: false,
            writable: true,
            raw_value: Some("0x10".to_string()),
            js_quote: Some("'".to_string()),
            html_quote: Some("\"".to_string()),
            quote_key: true,
            object_shorthand: false,
        });
        file.html_elements[0].write_classes = Some(vec!["16".to_string()]);
        let candidates = HashMap::from([(
            SelectorKey::Class("16".to_string()),
            vec!["m-2".to_string()],
        )]);

        let plan = plan_html_file(&file, "/project/site.css", &candidates, None);
        assert_eq!(plan.edits[0].replacement, "'16 m-2'");
        let rebased = rebase_attribute(
            file.html_elements[0].class_attribute.clone().unwrap(),
            &plan.edits,
        )
        .unwrap();
        assert_eq!(rebased.value, "16 m-2");
        assert_eq!(rebased.raw_value.as_deref(), Some("16 m-2"));
    }

    #[test]
    fn rebases_quoted_object_keys_without_losing_runtime_classes_or_spans() {
        let attribute = HtmlAttribute {
            value: "btn".to_string(),
            start: 10,
            end: 13,
            synthetic: false,
            writable: true,
            raw_value: Some("btn".to_string()),
            js_quote: Some("'".to_string()),
            html_quote: Some("\"".to_string()),
            quote_key: true,
            object_shorthand: false,
        };
        let first = rebase_attribute(
            attribute,
            &[Edit {
                start: 10,
                end: 13,
                replacement: "'btn p-4'".to_string(),
            }],
        )
        .unwrap();
        assert_eq!((first.start, first.end), (11, 18));
        assert_eq!(first.value, "btn p-4");
        assert_eq!(first.raw_value.as_deref(), Some("btn p-4"));
        assert!(!first.quote_key);

        let second = rebase_attribute(
            first,
            &[Edit {
                start: 11,
                end: 18,
                replacement: "btn p-4 m-2".to_string(),
            }],
        )
        .unwrap();
        assert_eq!((second.start, second.end), (11, 22));
        assert_eq!(second.value, "btn p-4 m-2");
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
                    raw_value: None,
                    js_quote: None,
                    html_quote: None,
                    quote_key: false,
                    object_shorthand: false,
                }),
                id_attribute: Some(HtmlAttribute {
                    value: "hero".to_string(),
                    start: 31,
                    end: 35,
                    synthetic: false,
                    writable: true,
                    raw_value: None,
                    js_quote: None,
                    html_quote: None,
                    quote_key: false,
                    object_shorthand: false,
                }),
                node_start: None,
                match_classes: None,
                write_classes: None,
                match_ids: None,
                match_tag: None,
                tag: None,
                css_paths: Vec::new(),
                class_opaque: false,
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

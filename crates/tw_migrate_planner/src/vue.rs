use super::*;

pub(super) fn is_vue_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension == "vue")
}

/// One plain-CSS `<style scoped>` block of a Vue SFC, in absolute byte
/// offsets of the `.vue` file. The outer span covers the whole block
/// including its tags; the content span covers only the CSS text.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VueBlock {
    pub(super) outer_start: usize,
    pub(super) outer_end: usize,
    pub(super) content_start: usize,
    pub(super) content_end: usize,
    #[serde(default)]
    pub(super) syntax: StylesheetSyntax,
    #[serde(default)]
    pub(super) analysis_source: Option<String>,
    #[serde(default)]
    pub(super) source_path: Option<String>,
    #[serde(default)]
    pub(super) source_mappings: Vec<SourceMapping>,
}

impl VueBlock {
    /// Shift every block boundary through one round of applied edits.
    fn shift(&mut self, edits: &[Edit]) {
        self.outer_start = shift_offset(edits, self.outer_start);
        self.outer_end = shift_offset(edits, self.outer_end);
        self.content_start = shift_offset(edits, self.content_start);
        self.content_end = shift_offset(edits, self.content_end);
    }
}

/// Same-length copy of a `.vue` source with every byte outside the scoped
/// block contents replaced by a space, so parsing it as CSS yields spans in
/// absolute `.vue` byte offsets with no rebasing.
pub(super) fn mask_vue_source(source: &str, blocks: &[VueBlock]) -> MigrationResult<String> {
    let mut bytes = vec![b' '; source.len()];
    for block in blocks {
        if block.content_start > block.content_end
            || block.content_end > source.len()
            || block.outer_start > block.content_start
            || block.content_end > block.outer_end
            || block.outer_end > source.len()
            || !source.is_char_boundary(block.content_start)
            || !source.is_char_boundary(block.content_end)
            || !source.is_char_boundary(block.outer_start)
            || !source.is_char_boundary(block.outer_end)
        {
            return Err(MigrationError::Invariant {
                message: "Invalid Vue style block span".to_string(),
            });
        }
        bytes[block.content_start..block.content_end]
            .copy_from_slice(&source.as_bytes()[block.content_start..block.content_end]);
    }
    String::from_utf8(bytes).map_err(|_| MigrationError::Invariant {
        message: "Invalid Vue style block span".to_string(),
    })
}

/// Map a caller-supplied Vue retention code onto the static warning code and
/// per-rule message used when an open template surface retains a scoped rule.
pub(super) fn rebase_vue_blocks(
    prior_edits: &[Vec<Edit>],
    blocks: &mut [VueBlock],
) -> MigrationResult<()> {
    // Earlier same-path entries edit template attributes and their own
    // blocks; those ranges are disjoint from this entry's blocks, so each
    // boundary shifts exactly through the applied edit rounds. An edit
    // reaching inside one of these blocks is a genuine conflict.
    for round in prior_edits {
        let mut round = round.clone();
        round.sort_by_key(|edit| (edit.start, edit.end));
        for block in blocks.iter_mut() {
            if round.iter().any(|edit| {
                edit.start < block.outer_end
                    && (edit.end > block.outer_start
                        || (edit.start == edit.end && edit.start > block.outer_start))
            }) {
                return Err(MigrationError::PlanCollision {
                    message: "A Vue style block changed during batch planning".to_string(),
                });
            }
            block.shift(&round);
        }
    }
    Ok(())
}

pub(super) fn vue_retention_warning(code: &str) -> MigrationResult<(&'static str, &'static str)> {
    match code {
        "dynamic-template-class" => Ok((
            "dynamic-template-class",
            "A dynamic class binding makes the template's class set unprovable, so the scoped rule is retained.",
        )),
        "component-class-target" => Ok((
            "component-class-target",
            "A child component's root element can carry classes this scoped rule matches, so it is retained.",
        )),
        "open-root-fallthrough" => Ok((
            "open-root-fallthrough",
            "A parent component can merge classes onto the single root element, so the scoped rule is retained.",
        )),
        other => Err(MigrationError::Invariant {
            message: format!("Unknown Vue retention code: {other}"),
        }),
    }
}

/// Whether one of `rule`'s template sites (elements carrying the rule's
/// class or id) satisfies `reachable`, which receives the site's class list
/// and element.
pub(super) fn rule_site_reachable(
    rule: &RulePlan,
    file: &SourceFile,
    css_path: &str,
    vue_module: bool,
    reachable: impl Fn(&[&str], &HtmlElement) -> bool,
) -> bool {
    file.html_elements
        .iter()
        .filter(|element| element_has_context(element, css_path))
        .any(|element| {
            let classes = element_classes(element);
            let ids = element_ids(element);
            // Module class names are hashed at runtime, so a module rule
            // reaches a site only through its proven `$style` binding.
            let site_matches_rule = if vue_module {
                element
                    .module_binding
                    .as_ref()
                    .is_some_and(|binding| rule.related_classes.contains(&binding.name))
            } else {
                rule.related_classes
                    .iter()
                    .any(|class| classes.contains(&class.as_str()))
                    || matches!(
                        &rule.key,
                        Some(SelectorKey::Id(name)) if ids.contains(&name.as_str())
                    )
            };
            site_matches_rule && reachable(&classes, element)
        })
}

/// A retained rule in the same scoped block is itself an unlayered
/// competitor: deleting a sibling rule that shares one of its sites would
/// expose it over the layered replacement utility. Retention can cascade, so
/// stamp to a fixpoint.
pub(super) fn stamp_in_file_shadow(
    rules: &mut [RulePlan],
    vue_files: &[&SourceFile],
    css_path: &str,
    vue_module: bool,
    additionally_retained: &HashSet<RuleId>,
) {
    loop {
        // Retained conditionals may contain selectors that are no longer
        // available on their synthetic plan.
        let retained_at_rule_unverifiable = rules.iter().any(|rule| {
            (rule.warning.is_some() || additionally_retained.contains(&rule_id(rule)))
                && rule.selector.starts_with('@')
                && rule.contains_selectors
        });
        let retained_selectors = rules
            .iter()
            .filter(|rule| {
                (rule.warning.is_some() || additionally_retained.contains(&rule_id(rule)))
                    && !rule.selector.starts_with('@')
            })
            .map(|rule| format!("{} {{}}", rule.selector))
            .collect::<Vec<_>>();
        let retained = index_shadow_selectors(&retained_selectors, &[]);
        let mut changed = false;
        for rule in rules.iter_mut() {
            if rule.warning.is_some() || additionally_retained.contains(&rule_id(rule)) {
                continue;
            }
            let shadowed = retained_at_rule_unverifiable
                || retained.unverifiable
                || vue_files.iter().any(|file| {
                    rule_site_reachable(rule, file, css_path, vue_module, |classes, element| {
                        classes
                            .iter()
                            .any(|class| retained.classes.contains(*class))
                            || element_tag(element).is_some_and(|tag| {
                                retained.types.contains(&tag.to_ascii_lowercase())
                            })
                            || element_ids(element)
                                .iter()
                                .any(|id| retained.ids.contains(*id))
                            // A module site carries its retained sibling's
                            // hashed class through the binding, not through
                            // a literal class.
                            || (vue_module
                                && element.module_binding.as_ref().is_some_and(|binding| {
                                    retained.classes.contains(binding.name.as_str())
                                }))
                    })
                });
            if shadowed {
                rule.warning = Some("shadowed-scoped-rule");
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// Apply the merged template and scoped-block edits to a `.vue` source, drop
/// conditional at-rules emptied by rule removal, delete blocks whose CSS is
/// gone entirely, and validate that the remaining scoped CSS still parses.
/// The masked copy stays byte-aligned with the real source throughout so
/// masked-domain spans remain valid for both.
pub(super) fn finish_vue_stylesheet(
    request: &PlanRequest,
    masked: &str,
    mut edits: Vec<Edit>,
) -> MigrationResult<(String, Vec<Vec<Edit>>)> {
    if request
        .sheet
        .vue_blocks
        .iter()
        .any(|block| block.syntax != StylesheetSyntax::Css)
    {
        return finish_vue_preprocessor_stylesheet(request, edits);
    }
    edits.sort_by_key(|edit| (edit.start, edit.end));
    let mut edit_batches = vec![edits.clone()];
    // Template replacements must not leak into the masked CSS view; replace
    // them with same-length whitespace to keep the two strings aligned.
    let masked_edits = edits
        .iter()
        .map(|edit| {
            let in_block =
                request.sheet.vue_blocks.iter().any(|block| {
                    edit.start >= block.content_start && edit.end <= block.content_end
                });
            Edit {
                start: edit.start,
                end: edit.end,
                replacement: if in_block {
                    edit.replacement.clone()
                } else {
                    " ".repeat(edit.replacement.len())
                },
            }
        })
        .collect::<Vec<_>>();
    let mut blocks = request.sheet.vue_blocks.clone();
    for block in &mut blocks {
        block.shift(&edits);
    }
    // Conditionals that were already empty in the authored source are
    // untouched user bytes (often comment-only) and must survive; only
    // conditionals the migration itself empties may be removed.
    let mut preexisting_empty = {
        let allocator = oxc_css_parser::Allocator::default();
        let stylesheet = parse_css(&allocator, masked, Syntax::Css).map_err(|error| {
            MigrationError::EditedStylesheetParse {
                message: format!("Failed to parse edited CSS: {error}"),
            }
        })?;
        let mut already_empty = Vec::new();
        collect_empty_conditionals(&stylesheet.statements, &mut already_empty);
        already_empty
            .into_iter()
            .map(|edit| {
                (
                    shift_offset(&edits, edit.start),
                    shift_offset(&edits, edit.end),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut source = apply_edits(&request.sheet.css_source, edits)?;
    let mut masked = apply_edits(masked, masked_edits)?;

    loop {
        let allocator = oxc_css_parser::Allocator::default();
        let stylesheet = parse_css(&allocator, &masked, Syntax::Css).map_err(|error| {
            MigrationError::EditedStylesheetParse {
                message: format!("Failed to parse edited CSS: {error}"),
            }
        })?;
        let mut conditional_edits = Vec::new();
        collect_empty_conditionals(&stylesheet.statements, &mut conditional_edits);
        conditional_edits.retain(|edit| !preexisting_empty.contains(&(edit.start, edit.end)));
        if conditional_edits.is_empty() {
            break;
        }
        conditional_edits.sort_by_key(|edit| (edit.start, edit.end));
        for block in &mut blocks {
            block.shift(&conditional_edits);
        }
        for span in &mut preexisting_empty {
            span.0 = shift_offset(&conditional_edits, span.0);
            span.1 = shift_offset(&conditional_edits, span.1);
        }
        source = apply_edits(&source, conditional_edits.clone())?;
        masked = apply_edits(&masked, conditional_edits.clone())?;
        edit_batches.push(conditional_edits);
    }

    let removal_edits = emptied_block_removals(request, &blocks, &masked, &source, false)?;
    if !removal_edits.is_empty() {
        source = apply_edits(&source, removal_edits.clone())?;
        masked = apply_edits(&masked, removal_edits.clone())?;
        edit_batches.push(removal_edits);
    }
    validate_stylesheet(&masked, Syntax::Css)?;
    Ok((source, edit_batches))
}

fn finish_vue_preprocessor_stylesheet(
    request: &PlanRequest,
    mut edits: Vec<Edit>,
) -> MigrationResult<(String, Vec<Vec<Edit>>)> {
    edits.sort_by_key(|edit| (edit.start, edit.end));
    let mut edit_batches = vec![edits.clone()];
    let mut blocks = request.sheet.vue_blocks.clone();
    for block in &mut blocks {
        block.shift(&edits);
    }
    let mut source = apply_edits(&request.sheet.css_source, edits)?;
    let removals = emptied_block_removals(request, &blocks, &source, &source, true)?;
    if !removals.is_empty() {
        source = apply_edits(&source, removals.clone())?;
        edit_batches.push(removals);
    }
    Ok((source, edit_batches))
}

/// Removal edits for blocks whose CSS the migration emptied entirely, each
/// swallowing one trailing line break so no blank line is left behind.
/// `content_view` supplies the block contents (the masked copy for plain-CSS
/// blocks); kept blocks are validated when `validate_kept` is set.
fn emptied_block_removals(
    request: &PlanRequest,
    blocks: &[VueBlock],
    content_view: &str,
    source: &str,
    validate_kept: bool,
) -> MigrationResult<Vec<Edit>> {
    let mut removals = Vec::new();
    for (block, original) in blocks.iter().zip(&request.sheet.vue_blocks) {
        let originally_empty = request.sheet.css_source
            [original.content_start..original.content_end]
            .trim()
            .is_empty();
        let content = content_view
            .get(block.content_start..block.content_end)
            .ok_or_else(|| MigrationError::Invariant {
                message: "Invalid Vue style block span".to_string(),
            })?;
        if originally_empty || !content.trim().is_empty() {
            if validate_kept {
                validate_stylesheet(content, block.syntax.parser_syntax())?;
            }
            continue;
        }
        let mut end = block.outer_end;
        // Swallow one trailing line break so the removed block does not leave
        // a blank line behind.
        if source.as_bytes().get(end) == Some(&b'\r') {
            end += 1;
        }
        if source.as_bytes().get(end) == Some(&b'\n') {
            end += 1;
        }
        removals.push(Edit {
            start: block.outer_start,
            end,
            replacement: String::new(),
        });
    }
    Ok(removals)
}

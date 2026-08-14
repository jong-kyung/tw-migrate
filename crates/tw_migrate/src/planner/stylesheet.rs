use super::*;

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StylesheetSyntax {
    #[default]
    Css,
    Scss,
    Sass,
    Less,
}

impl StylesheetSyntax {
    pub(crate) fn parser_syntax(self) -> Syntax {
        match self {
            Self::Css => Syntax::Css,
            Self::Scss => Syntax::Scss,
            Self::Sass => Syntax::Sass,
            Self::Less => Syntax::Less,
        }
    }
}

pub(super) fn is_stylesheet_module(path: &str) -> bool {
    matches!(
        path.rsplit_once(".module."),
        Some((_, "css" | "scss" | "sass" | "less"))
    )
}

/// Run the JSX-graph proofs for every proof-needing rule against `files` (the
/// request's immutable snapshot) and return the rules that must be retained
/// with `unproven-css-module-relationship`, keyed by rule with their message.
// ponytail: the world is rebuilt once per stylesheet; share it across a
// batch's stylesheets if proof volume ever matters.
fn unproven_relationship_rules(
    rules: &[RulePlan],
    css_path: &str,
    files: &[SourceFile],
) -> HashMap<RuleId, String> {
    let proof_files = files
        .iter()
        .map(|file| (file.path.as_str(), file.source.as_str()))
        .collect::<Vec<_>>();
    let mut prepared = None;
    let mut unproven = HashMap::new();
    for rule in rules {
        let Some(relationship) = &rule.relationship else {
            continue;
        };
        if rule.warning.is_some() {
            continue;
        }
        let rule_id = rule_id(rule);
        if relationship.ancestor_state {
            unproven.insert(
                rule_id,
                format!(
                    "Ancestor-state selectors like `{}` are not convertible yet, so the rule is retained.",
                    rule.selector
                ),
            );
            continue;
        }
        for (index, step) in relationship.steps.iter().enumerate() {
            let prepared =
                prepared.get_or_insert_with(|| jsx_graph::prepare(&proof_files, css_path));
            let outcome =
                jsx_graph::prove_prepared(prepared, &step.ancestor, step.relation, &step.target);
            if !outcome.aggregate_proven {
                let reason = outcome.reason.unwrap_or("unproven");
                let site = outcome
                    .usages
                    .iter()
                    .find(|usage| !usage.proven)
                    .map(|usage| format!(" at {}:{}", usage.file, usage.span.0))
                    .unwrap_or_default();
                unproven.insert(
                    rule_id,
                    format!(
                        "The selector `{}` requires a relationship that could not be proven for every usage ({reason}{site}), so the rule is retained.",
                        rule.selector
                    ),
                );
                break;
            }
            // The first step's target is the rule's own key: its usage sites
            // are the ones conversion would edit, so a non-writable site
            // makes the proven rule unconvertible.
            if index == 0
                && let Some(usage) = outcome.usages.iter().find(|usage| {
                    files
                        .iter()
                        .any(|file| !file.writable && file.path == usage.file)
                })
            {
                unproven.insert(
                    rule_id,
                    format!(
                        "The selector `{}` matches a usage in the reference-only file {}, so the rule is retained.",
                        rule.selector, usage.file
                    ),
                );
                break;
            }
        }
    }
    unproven
}

fn stamp_unproven_rules(rules: &mut [RulePlan], unproven: &HashMap<RuleId, String>) {
    for rule in rules {
        let rule_id = rule_id(rule);
        if rule.warning.is_none() && unproven.contains_key(&rule_id) {
            rule.warning = Some("unproven-css-module-relationship");
        }
    }
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

pub(super) fn batch_stylesheet_request(
    batch: &BatchPlanRequest,
    stylesheet: &BatchStylesheet,
    files: Vec<SourceFile>,
) -> PlanRequest {
    PlanRequest {
        sheet: stylesheet.clone(),
        tailwind_path: batch.tailwind_path.clone(),
        tailwind_source: batch.tailwind_source.clone(),
        utility_prefix: batch.utility_prefix.clone(),
        theme_tokens: batch.theme_tokens.clone(),
        media_names: batch.media_names.clone(),
        entry_writable: batch.entry_writable,
        global_at_rule_moves: batch.global_at_rule_moves,
        files,
    }
}

/// Shared head of the candidate-map and main planning passes: derive the
/// request flags, parse the stylesheet, and apply the utility prefix, so
/// rule-selection behavior cannot silently diverge between the two paths.
fn parse_request_rules(request: &PlanRequest) -> Result<(bool, ParsedCss, Option<String>), String> {
    let is_module = request
        .sheet
        .is_module
        .unwrap_or_else(|| is_stylesheet_module(&request.sheet.css_path));
    let vue_masked = if request.sheet.vue_blocks.is_empty() {
        None
    } else {
        Some(mask_vue_source(
            &request.sheet.css_source,
            &request.sheet.vue_blocks,
        )?)
    };
    // Vue keyframes and at-rules stay inside their scoped block; moving them
    // to the Tailwind entry would change their scope.
    let can_move_at_rules = request.entry_writable
        && vue_masked.is_none()
        && request.sheet.syntax == StylesheetSyntax::Css
        && request
            .tailwind_path
            .as_ref()
            .zip(request.tailwind_source.as_ref())
            .is_some_and(|(path, _)| path != &request.sheet.css_path);
    let relative_urls_stable = request.tailwind_path.as_ref().is_some_and(|path| {
        Path::new(path).parent() == Path::new(&request.sheet.css_path).parent()
    });
    let keyframe_scope = request
        .sheet
        .css_module_id
        .as_deref()
        .unwrap_or(&request.sheet.css_path);
    let mut parsed = if vue_masked.is_some() {
        parse_vue_rules(request, is_module, keyframe_scope)?
    } else {
        let analysis_source = request
            .sheet
            .analysis_source
            .as_deref()
            .unwrap_or(&request.sheet.css_source);
        let analysis_syntax = if request.sheet.analysis_source.is_some() {
            Syntax::Css
        } else {
            request.sheet.syntax.parser_syntax()
        };
        let mut parsed = parse_css_rules(
            &request.sheet.css_path,
            keyframe_scope,
            analysis_source,
            &request.theme_tokens,
            request.media_names.as_ref(),
            ParseOptions {
                syntax: analysis_syntax,
                is_module,
                can_move_at_rules,
                can_move_global_at_rules: request
                    .sheet
                    .global_at_rule_moves
                    .unwrap_or(request.global_at_rule_moves),
                relative_urls_stable,
            },
        )?;
        if request.sheet.analysis_source.is_some() {
            map_rule_spans(
                &request.sheet.css_source,
                request.sheet.syntax,
                &request.sheet.css_path,
                &request.sheet.source_mappings,
                analysis_source,
                &mut parsed.rules,
                0,
            )?;
            if is_module {
                for rule in &mut parsed.rules {
                    if rule.warning.is_none() && rule.authored_span.is_none() {
                        rule.warning = Some("unproven-source-map");
                    }
                }
            }
        } else {
            for rule in &mut parsed.rules {
                rule.authored_span = Some(rule.span.clone());
            }
        }
        parsed
    };
    if request.sheet.is_partial {
        for rule in &mut parsed.rules {
            rule.warning = Some("shared-preprocessor-source");
        }
    }
    // A removable Vue rule is unlayered and can outrank non-scoped CSS that a
    // layered Tailwind utility would lose to. Retain any rule whose reachable
    // template site the package's non-scoped corpus can also target.
    if vue_masked.is_some() && is_module {
        let shadow = index_shadow_selectors(
            &request.sheet.vue_shadow_css,
            &request.sheet.vue_shadow_module_css,
        );
        let unverifiable = request.sheet.vue_shadow_unverifiable || shadow.unverifiable;
        let vue_files = request
            .files
            .iter()
            .filter(|file| file.has_analyzable_context(&request.sheet.css_path))
            .collect::<Vec<_>>();
        for rule in &mut parsed.rules {
            if rule.warning.is_some() {
                continue;
            }
            // The rule is shadowed when non-scoped CSS targets one of its
            // classes directly, or can match one of its template sites
            // through the site's tag, id, or co-occurring classes.
            let shadowed = unverifiable
                || (!request.sheet.vue_module
                    && rule
                        .related_classes
                        .iter()
                        .any(|class| shadow.classes.contains(class)))
                || vue_files.iter().any(|file| {
                    rule_site_reachable(
                        rule,
                        file,
                        &request.sheet.css_path,
                        request.sheet.vue_module,
                        |classes, element| {
                            classes.iter().any(|class| shadow.classes.contains(*class))
                            || element_tag(element).is_some_and(|tag| {
                                shadow.types.contains(&tag.to_ascii_lowercase())
                            })
                            || element_ids(element)
                                .iter()
                                .any(|id| shadow.ids.contains(*id))
                            // A module binding the module entry (planned
                            // first) did not replace stays live: its hashed
                            // class lands on this site at runtime and the
                            // retained module rule is an unlayered
                            // competitor the shadow index cannot name. An
                            // exact replacement rebases in place, so check
                            // the current text; a span an edit only touched
                            // (preserved-binding insertion) rebases to None
                            // and counts as live conservatively.
                            || (!request.sheet.vue_module
                                && element.module_binding.as_ref().is_some_and(|binding| {
                                    match rebase_span(
                                        binding.start,
                                        binding.end,
                                        &file.prior_edits,
                                    ) {
                                        None => true,
                                        Some((start, end)) => file
                                            .source
                                            .get(start..end)
                                            .is_some_and(|text| text.contains("$style")),
                                    }
                                }))
                        },
                    )
                });
            if shadowed {
                rule.warning = Some("shadowed-scoped-rule");
            }
        }
        stamp_in_file_shadow(
            &mut parsed.rules,
            &vue_files,
            &request.sheet.css_path,
            request.sheet.vue_module,
            &HashSet::new(),
        );
    }
    if let Some(prefix) = request
        .utility_prefix
        .as_deref()
        .filter(|prefix| !prefix.is_empty())
    {
        prefix_rule_candidates(&mut parsed.rules, prefix);
    }
    Ok((is_module, parsed, vue_masked))
}

fn parse_vue_rules(
    request: &PlanRequest,
    is_module: bool,
    keyframe_scope: &str,
) -> Result<ParsedCss, String> {
    let mut rules = Vec::new();
    let mut analysis_base = 0;
    for block in &request.sheet.vue_blocks {
        let authored = request
            .sheet
            .css_source
            .get(block.content_start..block.content_end)
            .ok_or_else(|| "Invalid Vue style block span".to_string())?;
        let analysis = block.analysis_source.as_deref().unwrap_or(authored);
        let mut parsed = parse_css_rules(
            &request.sheet.css_path,
            keyframe_scope,
            analysis,
            &request.theme_tokens,
            request.media_names.as_ref(),
            ParseOptions {
                syntax: if block.analysis_source.is_some() {
                    Syntax::Css
                } else {
                    block.syntax.parser_syntax()
                },
                is_module,
                can_move_at_rules: false,
                // Inert while `can_move_at_rules` is false: global at-rules
                // are never built on the Vue path.
                can_move_global_at_rules: false,
                relative_urls_stable: false,
            },
        )?;
        if block.analysis_source.is_some() {
            map_rule_spans(
                authored,
                block.syntax,
                block
                    .source_path
                    .as_deref()
                    .unwrap_or(&request.sheet.css_path),
                &block.source_mappings,
                analysis,
                &mut parsed.rules,
                block.content_start,
            )?;
            for rule in &mut parsed.rules {
                if rule.warning.is_none() && rule.authored_span.is_none() {
                    rule.warning = Some("unproven-source-map");
                }
            }
        } else {
            for rule in &mut parsed.rules {
                rule.authored_span = Some(
                    block.content_start + rule.span.start..block.content_start + rule.span.end,
                );
            }
        }
        for rule in &mut parsed.rules {
            rule.span = analysis_base + rule.span.start..analysis_base + rule.span.end;
        }
        analysis_base += analysis.len() + 1;
        rules.extend(parsed.rules);
    }
    Ok(ParsedCss {
        rules,
        keyframes: Vec::new(),
        global_at_rules: Vec::new(),
    })
}

fn dedup_candidate_map(candidate_map: &mut HashMap<SelectorKey, Vec<String>>) {
    for candidates in candidate_map.values_mut() {
        candidates.sort();
        candidates.dedup();
    }
}

pub(super) fn candidate_map_for_request(
    request: &PlanRequest,
    externally_blocked: &HashSet<RuleId>,
) -> Result<CandidateMaps, String> {
    let (_, ParsedCss { mut rules, .. }, _) = parse_request_rules(request)?;
    let unproven = unproven_relationship_rules(&rules, &request.sheet.css_path, &request.files);
    stamp_unproven_rules(&mut rules, &unproven);
    let blocked_classes = rules
        .iter()
        .filter(|rule| rule.warning.is_some())
        .flat_map(|rule| rule.related_classes.iter().cloned())
        .collect::<BTreeSet<_>>();
    let rule_selectors = rules
        .iter()
        .map(|rule| (rule_id(rule), rule.selector.clone()))
        .collect::<HashMap<_, _>>();
    let retained_rules = rules
        .iter()
        .filter(|rule| {
            rule.warning.is_some()
                || externally_blocked.contains(&rule_id(rule))
                || rule
                    .related_classes
                    .iter()
                    .any(|class| blocked_classes.contains(class))
        })
        .map(rule_id)
        .collect::<HashSet<_>>();
    let mut candidate_map: HashMap<SelectorKey, Vec<String>> = HashMap::new();
    let mut origins: HashMap<(SelectorKey, String), Vec<RuleOrigin>> = HashMap::new();
    for rule in rules {
        let rule_id = rule_id(&rule);
        // Externally blocked rules never apply their candidates, so they must
        // not create cross-stylesheet conflicts.
        if externally_blocked.contains(&rule_id) {
            continue;
        }
        if let Some(key) = rule.key
            && rule.warning.is_none()
            && !matches!(&key, SelectorKey::Class(name) if blocked_classes.contains(name))
        {
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
        rule_selectors,
        retained_rules,
        unproven,
    })
}

/// Warnings that retain a single rule during batch planning without blocking
/// the rest of its class's rules from converting.
fn is_batch_retained(warning: Option<&str>) -> bool {
    matches!(
        warning,
        Some("batch-stylesheet-conflict" | "candidate-compilation-failure")
    )
}

pub(super) fn plan_request(
    request: PlanRequest,
    blocked_rules: &RuleConflicts,
    externally_blocked: &HashSet<RuleId>,
    unproven_rules: &HashMap<RuleId, String>,
) -> Result<PlanResponse, String> {
    let (
        is_module,
        ParsedCss {
            mut rules,
            keyframes,
            global_at_rules,
        },
        vue_masked,
    ) = parse_request_rules(&request)?;
    let vue_mode = vue_masked.is_some();
    let vue_retention = request
        .sheet
        .vue_retention
        .as_deref()
        .map(vue_retention_warning)
        .transpose()?;
    for rule in &mut rules {
        let rule_id = rule_id(rule);
        // The externally-blocked stamp wins over conflict stamping so a
        // blocked rule surfaces only the caller-attributed
        // candidate-compilation-failure warning.
        if rule.warning.is_none() && externally_blocked.contains(&rule_id) {
            rule.warning = Some("candidate-compilation-failure");
        } else if blocked_rules.contains_key(&rule_id) {
            rule.warning = Some("batch-stylesheet-conflict");
        }
    }
    stamp_unproven_rules(&mut rules, unproven_rules);
    // Late retention stamps (blocked candidates, unproven relationships) can
    // expose in-file cascade competitors the parse-time pass could not see.
    if vue_mode {
        let vue_files = request
            .files
            .iter()
            .filter(|file| file.has_analyzable_context(&request.sheet.css_path))
            .collect::<Vec<_>>();
        let quote_blocked = rules
            .iter()
            .filter(|rule| rule.warning.is_none())
            .filter_map(|rule| {
                let Some(SelectorKey::Class(class)) = &rule.key else {
                    return None;
                };
                vue_files
                    .iter()
                    .any(|file| {
                        file.html_elements
                            .iter()
                            .filter(|element| element_has_context(element, &request.sheet.css_path))
                            .any(|element| {
                                element_classes(element).contains(&class.as_str())
                                    && element.class_attribute.as_ref().is_some_and(|attribute| {
                                        attribute.writable
                                            && !candidates_fit_attribute(
                                                &file.source,
                                                attribute,
                                                &rule.candidates,
                                            )
                                    })
                            })
                    })
                    .then(|| rule_id(rule))
            })
            .collect::<HashSet<_>>();
        stamp_in_file_shadow(
            &mut rules,
            &vue_files,
            &request.sheet.css_path,
            request.sheet.vue_module,
            &quote_blocked,
        );
    }

    let preserved_module_classes = rules
        .iter()
        .filter(|rule| is_module && is_batch_retained(rule.warning))
        .flat_map(|rule| rule.related_classes.iter().cloned())
        .collect::<BTreeSet<_>>();
    let blocked_classes = rules
        .iter()
        .filter(|rule| rule.warning.is_some() && !is_batch_retained(rule.warning))
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

    if is_module && !request.sheet.css_dependents.is_empty() {
        // Another stylesheet depends on this module (composes/@import), so
        // deleting it or removing imports would break that consumer.
        module_references_safe = false;
        for dependent in &request.sheet.css_dependents {
            warnings.push(Warning::new(
                "unsupported-css-module-reference",
                dependent.clone(),
                (0, 0),
                "Another stylesheet references the CSS Module, so it is retained.".to_string(),
            ));
        }
    }

    let module_rule_classes = request.sheet.vue_module.then(|| {
        rules
            .iter()
            .flat_map(|rule| rule.related_classes.iter().cloned())
            .collect::<BTreeSet<_>>()
    });
    for file in &request.files {
        let mut result = plan_consumer_file(
            file,
            &request.sheet.css_path,
            is_module,
            &candidate_map,
            &preserved_module_classes,
            module_rule_classes.as_ref(),
            request.utility_prefix.as_deref(),
            request.sheet.vue_unscoped,
            request.sheet.vue_module,
        )?;

        module_references_safe &= result.module_references_safe;
        let direct_html_link = file
            .html_stylesheets
            .iter()
            .any(|context| context.direct && context.css_path == request.sheet.css_path);
        let unsafe_html_link = file.html_stylesheets.iter().any(|context| {
            context.direct && !context.analyzable && context.css_path == request.sheet.css_path
        });
        if is_module
            && !request.sheet.vue_module
            && (unsafe_html_link || (direct_html_link && !file.html_references_safe))
        {
            module_references_safe = false;
        }
        // Inline scripts are never analyzed, so a script that names one of the
        // module's classes may create consumers at runtime; retain the module.
        let any_html_context = file
            .html_stylesheets
            .iter()
            .any(|context| context.css_path == request.sheet.css_path);
        if is_module
            && !request.sheet.vue_module
            && any_html_context
            && !file.html_script_text.is_empty()
            && rules.iter().any(|rule| {
                rule.related_classes
                    .iter()
                    .any(|class| mentions_word(&file.html_script_text, class))
            })
        {
            module_references_safe = false;
            warnings.push(Warning::new(
                "unproven-script-reference",
                file.path.clone(),
                (0, 0),
                "An inline script names a CSS Module class, so the module is retained.".to_string(),
            ));
        }
        if !file.writable {
            if is_module
                && (direct_html_link
                    || !result.module_refs.is_empty()
                    || !result.removable_import_edits.is_empty())
            {
                module_references_safe = false;
                warnings.push(Warning::new(
                    "reference-only-css-module-consumer",
                    file.path.clone(),
                    (0, 0),
                    "A reference-only source uses this CSS Module, so it is retained.".to_string(),
                ));
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
    let prior_edits = request
        .files
        .iter()
        .find(|file| file.path == request.sheet.css_path)
        .map(|file| file.prior_edits.as_slice())
        .unwrap_or_default();

    for rule in rules {
        let can_remove = is_module
            && module_references_safe
            && rule.warning.is_none()
            && match &rule.key {
                Some(SelectorKey::Class(name)) => {
                    let refs = module_refs.get(name).copied().unwrap_or(0);
                    refs > 0 && matched_module_refs.get(name).copied().unwrap_or(0) == refs
                }
                _ => false,
            };

        let rule_id = rule_id(&rule);
        let report_authored_span =
            rule.authored_span
                .as_ref()
                .map_or(RuleId { start: 0, end: 0 }, |span| RuleId {
                    start: original_offset(prior_edits, span.start),
                    end: original_offset(prior_edits, span.end),
                });
        let status = if can_remove {
            converted_rules += 1;
            let authored_span = rule
                .authored_span
                .clone()
                .expect("removable rules must have proven authored spans");
            css_edits.push(Edit {
                start: authored_span.start,
                end: authored_span.end,
                replacement: String::new(),
            });
            "converted"
        } else if rule.warning == Some("candidate-compilation-failure") {
            // The caller blocked this rule after a Tailwind compilation
            // failure and attributes the warning itself.
            retained_rules += 1;
            "retained"
        } else {
            retained_rules += 1;
            let (code, message) = match rule.warning {
                Some(code @ "batch-stylesheet-conflict") => {
                    let conflicts = blocked_rules
                        .get(&rule_id)
                        .expect("conflicting rule must retain its candidates")
                        .iter()
                        .map(|(left, right)| format!("`{left}` and `{right}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    (
                        code,
                        format!(
                            "Generated utilities {conflicts} conflict on the same source element, so the contributing rule is retained."
                        ),
                    )
                }
                Some(code @ "unproven-css-module-relationship") => (
                    code,
                    unproven_rules.get(&rule_id).cloned().unwrap_or_else(|| {
                        "The CSS Module selector relationship could not be proven for every usage."
                            .to_string()
                    }),
                ),
                Some(code @ "unproven-source-map") => (
                    code,
                    "The generated rule does not map uniquely to one authored source rule, so it is retained."
                        .to_string(),
                ),
                Some(code @ "shared-preprocessor-source") => (
                    code,
                    "A Sass partial must be analyzed through every consuming entry, so it is retained."
                        .to_string(),
                ),
                Some(code @ "shadowed-scoped-rule") => (
                    code,
                    "Other package CSS also targets a class this scoped rule matches, so deleting it could change the cascade; the rule is retained."
                        .to_string(),
                ),
                Some(code) => (
                    code,
                    "The rule is outside the supported declaration or selector subset.".to_string(),
                ),
                None => {
                    if let Some((code, message)) = vue_retention {
                        (code, message.to_string())
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
                    }
                }
            };
            warnings.push(Warning::new(
                code,
                request.sheet.css_path.clone(),
                (report_authored_span.start, report_authored_span.end),
                message,
            ));
            "retained"
        };
        rule_reports.push(RuleReport {
            selector: rule.selector,
            status,
            candidates: rule.candidates,
            file: request.sheet.css_path.clone(),
            rule_id,
            authored_span: report_authored_span,
            stylesheet: 0,
        });
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

    // A Vue SFC is stylesheet and consumer at once: its template edits and
    // scoped-block edits are all absolute `.vue` offsets, so they merge into
    // one edit list producing one planned file.
    if vue_mode {
        for (file, result) in &mut source_plans {
            if file.path == request.sheet.css_path {
                css_edits.append(&mut result.edits);
            }
        }
    }
    // A module file may only disappear when every reference is matched and
    // safe; an emptied stylesheet with a dangling member reference must stay
    // on disk so the consumer's retained import keeps resolving. This is the
    // same condition that allows removing the module's at-rules.
    let module_removable = remove_at_rules;
    let stylesheet_changed = !css_edits.is_empty();
    let mut deleted_files = Vec::new();
    let mut applied_edits = HashMap::new();
    if stylesheet_changed {
        if let Some(masked) = vue_masked.as_deref() {
            let (source, edit_batches) = finish_vue_stylesheet(&request, masked, css_edits)?;
            applied_edits.insert(request.sheet.css_path.clone(), edit_batches);
            planned_files.push(PlannedFile {
                path: request.sheet.css_path.clone(),
                source,
            });
        } else {
            applied_edits.insert(request.sheet.css_path.clone(), vec![css_edits.clone()]);
            let source = apply_edits(&request.sheet.css_source, css_edits)?;
            let source = if is_module {
                remove_empty_conditionals(source, request.sheet.syntax.parser_syntax())?
            } else {
                source
            };
            validate_stylesheet(&source, request.sheet.syntax.parser_syntax())?;
            if module_removable && source.trim().is_empty() {
                deleted_files.push(request.sheet.css_path.clone());
            } else {
                planned_files.push(PlannedFile {
                    path: request.sheet.css_path.clone(),
                    source,
                });
            }
        }
    }

    let css_module_deleted = deleted_files.contains(&request.sheet.css_path);
    let module_import_is_unused = !vue_mode && module_removable;
    for (file, mut result) in source_plans {
        if css_module_deleted || module_import_is_unused {
            result.edits.append(&mut result.removable_import_edits);
        }
        if !result.edits.is_empty() {
            applied_edits
                .entry(file.path.clone())
                .or_insert_with(Vec::new)
                .push(result.edits.clone());
            let source = apply_edits(&file.source, result.edits)?;
            if Path::new(&file.path)
                .extension()
                .is_none_or(|extension| extension != "html" && extension != "vue")
            {
                validate_js(&file.path, &source)?;
            }
            planned_files.push(PlannedFile {
                path: file.path.clone(),
                source,
            });
        }
    }

    if stylesheet_changed
        && matches!(
            request.sheet.syntax,
            StylesheetSyntax::Scss | StylesheetSyntax::Sass | StylesheetSyntax::Less
        )
    {
        warnings.push(Warning::new(
            "rebuild-required",
            request.sheet.css_path.clone(),
            (0, 0),
            "Rebuild this preprocessor entry to refresh its generated CSS.".to_string(),
        ));
    }

    Ok(PlanResponse {
        files: planned_files,
        deleted_files,
        unlinked_files: if module_import_is_unused {
            vec![request.sheet.css_path]
        } else {
            Vec::new()
        },
        candidates: candidates.into_iter().collect(),
        converted_rules,
        retained_rules,
        rules: rule_reports,
        warnings,
        applied_edits,
    })
}

fn merge_counts(target: &mut HashMap<String, usize>, source: &HashMap<String, usize>) {
    for (key, count) in source {
        *target.entry(key.clone()).or_default() += *count;
    }
}

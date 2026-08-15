use super::*;

struct BatchMatch {
    stylesheet: usize,
    candidate: String,
    rule: RuleId,
    properties: BTreeSet<String>,
}

/// Single-stylesheet planning for unit tests: a thin wrapper that reshapes
/// the flat request into a one-stylesheet batch, the only production path.
#[cfg(test)]
pub(super) fn plan_json(request: &str) -> MigrationResult<String> {
    let mut request: serde_json::Value =
        serde_json::from_str(request).map_err(|error| MigrationError::InvalidRequest {
            message: error.to_string(),
        })?;
    let stylesheet = request
        .as_object_mut()
        .ok_or_else(|| MigrationError::InvalidRequest {
            message: "Plan request must be an object".to_string(),
        })?;
    let mut batch = serde_json::Map::new();
    for field in [
        "candidateAliases",
        "entryWritable",
        "files",
        "globalAtRuleMoves",
        "mediaNames",
        "tailwindPath",
        "tailwindSource",
        "utilityPrefix",
        "themeTokens",
    ] {
        if let Some(value) = stylesheet.remove(field) {
            batch.insert(field.to_string(), value);
        }
    }
    batch.insert(
        "stylesheets".to_string(),
        serde_json::Value::Array(vec![request]),
    );
    plan_batch_json(&serde_json::Value::Object(batch).to_string())
}

/// One provable width constraint of a media variant chain, in a single
/// authored unit; bounds in other units stay unprovable and widen the
/// interval conservatively.
struct WidthConstraint {
    value: f64,
    unit: String,
    inclusive: bool,
    lower: bool,
}

/// Recognizes media-condition variant segments and proves the RFC's
/// ordering-sensitive conflict gate: two candidates that set conflicting
/// declarations on one element under distinct media conditions migrate
/// only when the conditions are provably mutually exclusive by width, or
/// when the pair is a base rule and a later media rule from the same
/// stylesheet, whose variant CSS always follows base utilities.
struct MediaVariantContext<'a> {
    /// True when a map was supplied at all: an explicitly empty map still
    /// means extraction ran and every condition fell back to arbitrary
    /// variants, which the gate must keep covering.
    enabled: bool,
    keys_by_name: HashMap<&'a str, &'a str>,
    theme_tokens: &'a HashMap<String, String>,
}

impl<'a> MediaVariantContext<'a> {
    fn new(request: &'a BatchPlanRequest) -> Self {
        let mut keys_by_name = HashMap::new();
        if let Some(names) = &request.media_names {
            for (key, name) in names {
                keys_by_name.insert(name.as_str(), key.as_str());
            }
        }
        Self {
            enabled: request.media_names.is_some(),
            keys_by_name,
            theme_tokens: &request.theme_tokens,
        }
    }

    fn is_media_segment(&self, segment: &str) -> bool {
        self.keys_by_name.contains_key(segment)
            || segment == "dark"
            || segment.starts_with("min-[")
            || segment.starts_with("max-[")
            || segment.starts_with("[@media")
            || self
                .theme_tokens
                .contains_key(&format!("breakpoint-{segment}"))
            || segment.strip_prefix("max-").is_some_and(|rest| {
                self.theme_tokens
                    .contains_key(&format!("breakpoint-{rest}"))
            })
    }

    /// The candidate's media variant segments and its residual variants.
    fn split_candidate<'c>(&self, candidate: &'c str) -> (Vec<&'c str>, Vec<&'c str>) {
        let (variants, _) = tailwind_utility_parts(candidate);
        let mut media = Vec::new();
        let mut residual = Vec::new();
        for segment in variant_segments(variants) {
            if self.is_media_segment(segment) {
                media.push(segment);
            } else {
                residual.push(segment);
            }
        }
        (media, residual)
    }

    fn component_constraints(component: &MediaComponent, constraints: &mut Vec<WidthConstraint>) {
        let Some(bound) = &component.width_bound else {
            return;
        };
        let Some((value, unit)) = parse_dimension(&bound.value) else {
            return;
        };
        constraints.push(WidthConstraint {
            value,
            unit: unit.to_string(),
            inclusive: bound.inclusive,
            lower: bound.lower,
        });
    }

    /// Provable width constraints of one side's media segments. Segments
    /// without a width bound contribute nothing, which only widens the
    /// side's interval and weakens exclusivity claims.
    fn constraints(&self, segments: &[&str]) -> Vec<WidthConstraint> {
        let mut constraints = Vec::new();
        for segment in segments {
            if let Some(key) = self.keys_by_name.get(segment) {
                match parse_media_condition(key) {
                    Some(ParsedMediaCondition::Components(components)) => {
                        for component in &components {
                            Self::component_constraints(component, &mut constraints);
                        }
                    }
                    Some(ParsedMediaCondition::Whole(_)) | None => {}
                }
                continue;
            }
            if let Some(token) = self.theme_tokens.get(&format!("breakpoint-{segment}")) {
                if let Some((value, unit)) = parse_dimension(token) {
                    constraints.push(WidthConstraint {
                        value,
                        unit: unit.to_string(),
                        inclusive: true,
                        lower: true,
                    });
                }
                continue;
            }
            if let Some(rest) = segment.strip_prefix("max-") {
                if let Some(token) = self.theme_tokens.get(&format!("breakpoint-{rest}")) {
                    if let Some((value, unit)) = parse_dimension(token) {
                        constraints.push(WidthConstraint {
                            value,
                            unit: unit.to_string(),
                            inclusive: false,
                            lower: false,
                        });
                    }
                    continue;
                }
                if let Some(value) = rest
                    .strip_prefix('[')
                    .and_then(|inner| inner.strip_suffix(']'))
                    && let Some((value, unit)) = parse_dimension(value)
                {
                    constraints.push(WidthConstraint {
                        value,
                        unit: unit.to_string(),
                        inclusive: true,
                        lower: false,
                    });
                }
                continue;
            }
            if let Some(value) = segment
                .strip_prefix("min-[")
                .and_then(|inner| inner.strip_suffix(']'))
                && let Some((value, unit)) = parse_dimension(value)
            {
                constraints.push(WidthConstraint {
                    value,
                    unit: unit.to_string(),
                    inclusive: true,
                    lower: true,
                });
            }
        }
        constraints
    }

    /// True when the two sides can never match together: some unit has one
    /// side's upper bound strictly below the other side's lower bound.
    fn provably_exclusive(&self, left: &[&str], right: &[&str]) -> bool {
        let left_constraints = self.constraints(left);
        let right_constraints = self.constraints(right);
        let disjoint = |uppers: &[&WidthConstraint], lowers: &[&WidthConstraint]| {
            uppers.iter().any(|upper| {
                lowers.iter().any(|lower| {
                    upper.unit == lower.unit
                        && (upper.value < lower.value
                            || (upper.value == lower.value
                                && !(upper.inclusive && lower.inclusive)))
                })
            })
        };
        fn split(
            constraints: &[WidthConstraint],
        ) -> (Vec<&WidthConstraint>, Vec<&WidthConstraint>) {
            let uppers: Vec<&WidthConstraint> =
                constraints.iter().filter(|bound| !bound.lower).collect();
            let lowers: Vec<&WidthConstraint> =
                constraints.iter().filter(|bound| bound.lower).collect();
            (uppers, lowers)
        }
        let (left_uppers, left_lowers) = split(&left_constraints);
        let (right_uppers, right_lowers) = split(&right_constraints);
        disjoint(&left_uppers, &right_lowers) || disjoint(&right_uppers, &left_lowers)
    }

    /// The RFC's ordering-sensitive pair: conflicting declarations under
    /// distinct, possibly co-matching media conditions whose emitted order
    /// is unproven.
    fn ordering_sensitive(&self, left: &BatchMatch, right: &BatchMatch) -> bool {
        if !self.enabled {
            return false;
        }
        let (left_media, left_residual) = self.split_candidate(&left.candidate);
        let (right_media, right_residual) = self.split_candidate(&right.candidate);
        if left_media == right_media || left_residual != right_residual {
            return false;
        }
        // Transferred source properties are authoritative when both sides
        // carry them; the spelling heuristic only covers metadata-free
        // candidates so a canonical alias cannot be misclassified by its
        // class prefix.
        let string_fallback = left.properties.is_empty() || right.properties.is_empty();
        let conflicting = (string_fallback
            && tailwind_utilities_conflict(
                &format!("probe:{}", tailwind_utility_parts(&left.candidate).1),
                &format!("probe:{}", tailwind_utility_parts(&right.candidate).1),
            ))
            || left.properties.iter().any(|left_property| {
                right
                    .properties
                    .iter()
                    .any(|right_property| css_properties_conflict(left_property, right_property))
            });
        if !conflicting {
            return false;
        }
        if self.provably_exclusive(&left_media, &right_media) {
            return false;
        }
        // A base rule paired with a later media rule from the same
        // stylesheet keeps its original winner: variant CSS always follows
        // base utilities.
        if left.stylesheet == right.stylesheet {
            let (base, media) = if left_media.is_empty() {
                (Some(left), Some(right))
            } else if right_media.is_empty() {
                (Some(right), Some(left))
            } else {
                (None, None)
            };
            if let (Some(base), Some(media)) = (base, media)
                && media.rule.start > base.rule.start
            {
                return false;
            }
        }
        true
    }
}

pub fn plan_batch_json(request: &str) -> MigrationResult<String> {
    let request: BatchPlanRequest =
        serde_json::from_str(request).map_err(|error| MigrationError::InvalidRequest {
            message: error.to_string(),
        })?;
    if request.stylesheets.is_empty() {
        return Err(MigrationError::InvalidRequest {
            message: "Batch migration requires at least one stylesheet".to_string(),
        });
    }

    let mut match_groups: HashMap<(String, usize, usize), Vec<BatchMatch>> = HashMap::new();
    // Relationship proofs run here against the request's immutable file set,
    // so every stylesheet is proven on the same snapshot regardless of the
    // edits earlier stylesheets make during the main pass.
    let mut candidate_maps = Vec::new();
    // The snapshot passes below never edit the corpus, so one clone travels
    // through every per-stylesheet request instead of a clone per stylesheet.
    let mut snapshot_files = request.files.clone();
    let externally_blocked = request
        .stylesheets
        .iter()
        .map(|stylesheet| {
            stylesheet
                .blocked_rules
                .iter()
                .copied()
                .collect::<HashSet<_>>()
        })
        .collect::<Vec<_>>();
    for (index, stylesheet) in request.stylesheets.iter().enumerate() {
        let plan_request = batch_stylesheet_request(&request, stylesheet, snapshot_files);
        let maps = candidate_map_for_request(&plan_request, &externally_blocked[index])?;
        let map_properties = candidate_property_union(maps.origins.iter().flat_map(
            |((_, candidate), origins)| {
                origins
                    .iter()
                    .map(move |origin| (candidate, &origin.properties))
            },
        ));
        snapshot_files = plan_request.files;
        for file in request.files.iter().filter(|file| file.writable) {
            let result = plan_consumer_file(
                file,
                &stylesheet.css_path,
                stylesheet
                    .is_module
                    .unwrap_or_else(|| is_stylesheet_module(&stylesheet.css_path)),
                &maps.candidates,
                &map_properties,
                &BTreeSet::new(),
                // The conflict pass must keep collecting matches; the main
                // pass applies the unresolved-member retention itself.
                None,
                request.utility_prefix.as_deref(),
                stylesheet.vue_unscoped,
                stylesheet.vue_module,
            )?;
            for matched in result.matches {
                if let Some(origins) = maps
                    .origins
                    .get(&(matched.key, matched.origin_candidate.clone()))
                {
                    match_groups
                        .entry((file.path.clone(), matched.start, matched.end))
                        .or_default()
                        .extend(origins.iter().map(|origin| BatchMatch {
                            stylesheet: index,
                            candidate: matched.candidate.clone(),
                            rule: origin.rule,
                            properties: origin.properties.clone(),
                        }));
                }
            }
        }
        candidate_maps.push(maps);
    }

    let media_context = MediaVariantContext::new(&request);
    let mut blocked_rules: Vec<RuleConflicts> = vec![HashMap::new(); request.stylesheets.len()];
    for matches in match_groups.values() {
        for (left_index, left) in matches.iter().enumerate() {
            for right in &matches[left_index + 1..] {
                if (left.stylesheet != right.stylesheet
                    && (((left.properties.is_empty() || right.properties.is_empty())
                        && tailwind_utilities_conflict(&left.candidate, &right.candidate))
                        // Identical spellings emit one class and never
                        // compete, matching the heuristic's equality exit.
                        || (left.candidate != right.candidate
                            && tailwind_variants_match(&left.candidate, &right.candidate)
                            && left.properties.iter().any(|left_property| {
                                right.properties.iter().any(|right_property| {
                                    css_properties_conflict(left_property, right_property)
                                })
                            }))))
                    || media_context.ordering_sensitive(left, right)
                {
                    let pair = if left.candidate <= right.candidate {
                        (left.candidate.clone(), right.candidate.clone())
                    } else {
                        (right.candidate.clone(), left.candidate.clone())
                    };
                    blocked_rules[left.stylesheet]
                        .entry(left.rule)
                        .or_default()
                        .insert(pair.clone());
                    blocked_rules[right.stylesheet]
                        .entry(right.rule)
                        .or_default()
                        .insert(pair);
                }
            }
        }
    }

    // A retained scoped or unscoped rule in the same SFC remains unlayered.
    // Feed only those surviving selectors into the module entry's shadow gate
    // so fully migratable sibling and module blocks can still convert together.
    let mut co_located_retained_css: HashMap<String, Vec<String>> = HashMap::new();
    for (index, stylesheet) in request.stylesheets.iter().enumerate() {
        if stylesheet.vue_module || stylesheet.vue_blocks.is_empty() {
            continue;
        }
        // The immutable batch snapshot still carries the module binding, which
        // makes scoped rules look shadowed before the module entry gets its
        // chance to remove it. Hide that one circular surface while deciding
        // which scoped rules survive independently.
        let mut hidden_bindings = Vec::new();
        for (file_index, file) in snapshot_files.iter_mut().enumerate() {
            if file.path != stylesheet.css_path {
                continue;
            }
            for (element_index, element) in file.html_elements.iter_mut().enumerate() {
                if let Some(binding) = element.module_binding.take() {
                    hidden_bindings.push((file_index, element_index, binding));
                }
            }
        }
        let plan_request = batch_stylesheet_request(&request, stylesheet, snapshot_files);
        let maps = candidate_map_for_request(&plan_request, &externally_blocked[index])?;
        snapshot_files = plan_request.files;
        for (file_index, element_index, binding) in hidden_bindings {
            snapshot_files[file_index].html_elements[element_index].module_binding = Some(binding);
        }
        let mut retained = maps.retained_rules;
        retained.extend(blocked_rules[index].keys().copied());
        if stylesheet.vue_retention.is_some() {
            retained.extend(maps.rule_selectors.keys().copied());
        }
        let css = co_located_retained_css
            .entry(stylesheet.css_path.clone())
            .or_default();
        css.extend(retained.into_iter().filter_map(|rule| {
            maps.rule_selectors
                .get(&rule)
                .map(|selector| format!("{selector} {{}}"))
        }));
    }
    for css in co_located_retained_css.values_mut() {
        css.sort();
        css.dedup();
    }

    let mut originals = HashMap::new();
    for file in &request.files {
        originals.insert(file.path.clone(), file.source.clone());
    }
    for stylesheet in &request.stylesheets {
        originals.insert(stylesheet.css_path.clone(), stylesheet.css_source.clone());
    }
    if let Some((path, source)) = request
        .tailwind_path
        .as_ref()
        .zip(request.tailwind_source.as_ref())
    {
        originals.insert(path.clone(), source.clone());
    }
    let mut current = originals.clone();
    let mut applied_edits: HashMap<String, Vec<Vec<Edit>>> = HashMap::new();
    let mut deleted = HashSet::new();
    let mut unlinked = HashSet::new();
    let mut candidates = BTreeSet::new();
    let mut candidate_probes = BTreeSet::new();
    let mut converted_rules = 0;
    let mut retained_rules = 0;
    let mut rules = Vec::new();
    let mut warnings = Vec::new();
    let mut order = (0..request.stylesheets.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        request.stylesheets[*left]
            .css_path
            .cmp(&request.stylesheets[*right].css_path)
    });

    for index in order {
        let stylesheet = &request.stylesheets[index];
        let mut files = request.files.clone();
        for file in &mut files {
            if let Some(source) = current.get(&file.path) {
                file.source.clone_from(source);
            }
            file.prior_edits = applied_edits.get(&file.path).cloned().unwrap_or_default();
        }
        let mut stylesheet_request = batch_stylesheet_request(&request, stylesheet, files);
        if stylesheet.vue_module
            && let Some(css) = co_located_retained_css.get(&stylesheet.css_path)
        {
            stylesheet_request
                .sheet
                .vue_shadow_css
                .extend(css.iter().cloned());
        }
        stylesheet_request.sheet.css_source = current
            .get(&stylesheet.css_path)
            .cloned()
            .unwrap_or_else(|| stylesheet.css_source.clone());
        if !stylesheet_request.sheet.vue_blocks.is_empty() {
            rebase_vue_blocks(
                applied_edits
                    .get(&stylesheet.css_path)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                &mut stylesheet_request.sheet.vue_blocks,
            )?;
        }
        stylesheet_request.tailwind_source = request
            .tailwind_path
            .as_ref()
            .and_then(|path| current.get(path).cloned());
        let response = plan_request(
            stylesheet_request,
            &blocked_rules[index],
            &externally_blocked[index],
            &candidate_maps[index].unproven,
        )?;

        for (path, batches) in response.applied_edits {
            applied_edits.entry(path).or_default().extend(batches);
        }
        for file in response.files {
            deleted.remove(&file.path);
            current.insert(file.path, file.source);
        }
        for path in response.deleted_files {
            current.remove(&path);
            deleted.insert(path);
        }
        unlinked.extend(response.unlinked_files);
        candidates.extend(response.candidates);
        candidate_probes.extend(response.candidate_probes);
        converted_rules += response.converted_rules;
        retained_rules += response.retained_rules;
        rules.extend(response.rules.into_iter().map(|mut rule| {
            rule.stylesheet = index;
            rule
        }));
        warnings.extend(response.warnings);
    }

    let mut files = current
        .into_iter()
        .filter(|(path, source)| originals.get(path).is_some_and(|before| before != source))
        .map(|(path, source)| PlannedFile { path, source })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let mut deleted_files = deleted.into_iter().collect::<Vec<_>>();
    deleted_files.sort();
    let mut unlinked_files = unlinked.into_iter().collect::<Vec<_>>();
    unlinked_files.sort();
    warnings.sort_by(|left, right| {
        (&left.file, left.start, left.end, left.code).cmp(&(
            &right.file,
            right.start,
            right.end,
            right.code,
        ))
    });

    serde_json::to_string(&PlanResponse {
        files,
        deleted_files,
        unlinked_files,
        candidates: candidates.into_iter().collect(),
        candidate_probes: candidate_probes.into_iter().collect(),
        converted_rules,
        retained_rules,
        rules,
        warnings,
        applied_edits,
    })
    .map_err(|error| MigrationError::Serialization {
        message: error.to_string(),
    })
}

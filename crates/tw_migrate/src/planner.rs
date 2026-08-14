use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
};

use oxc_css_parser::{Syntax, ast::Statement};
use serde::{Deserialize, Serialize};

use crate::{
    animations::append_keyframes,
    at_rules::{append_global_at_rules, is_conditional, parse_css},
    css_plan::{
        ParseOptions, ParsedCss, RulePlan, SelectorKey, index_shadow_selectors, parse_css_rules,
    },
    html_rewrite::{candidates_fit_attribute, plan_html_file, plan_vue_module_file, rebase_span},
    js_rewrite::{SourcePlan, opaque_reference_plan, plan_batch_source_file, validate_js},
    jsx_graph,
    media::{MediaComponent, ParsedMediaCondition, parse_media_condition},
    theme::parse_dimension,
    utilities::{
        css_properties_conflict, tailwind_utilities_conflict, tailwind_utility_parts,
        tailwind_variants_match, variant_segments,
    },
};

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

fn is_stylesheet_module(path: &str) -> bool {
    matches!(
        path.rsplit_once(".module."),
        Some((_, "css" | "scss" | "sass" | "less"))
    )
}

fn is_vue_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension == "vue")
}

/// One plain-CSS `<style scoped>` block of a Vue SFC, in absolute byte
/// offsets of the `.vue` file. The outer span covers the whole block
/// including its tags; the content span covers only the CSS text.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VueBlock {
    outer_start: usize,
    outer_end: usize,
    content_start: usize,
    content_end: usize,
    #[serde(default)]
    syntax: StylesheetSyntax,
    #[serde(default)]
    analysis_source: Option<String>,
    #[serde(default)]
    source_path: Option<String>,
    #[serde(default)]
    source_mappings: Vec<SourceMapping>,
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
fn mask_vue_source(source: &str, blocks: &[VueBlock]) -> Result<String, String> {
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
            return Err("Invalid Vue style block span".to_string());
        }
        bytes[block.content_start..block.content_end]
            .copy_from_slice(&source.as_bytes()[block.content_start..block.content_end]);
    }
    String::from_utf8(bytes).map_err(|_| "Invalid Vue style block span".to_string())
}

/// Map a caller-supplied Vue retention code onto the static warning code and
/// per-rule message used when an open template surface retains a scoped rule.
fn rebase_vue_blocks(prior_edits: &[Vec<Edit>], blocks: &mut [VueBlock]) -> Result<(), String> {
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
                return Err("A Vue style block changed during batch planning".to_string());
            }
            block.shift(&round);
        }
    }
    Ok(())
}

fn vue_retention_warning(code: &str) -> Result<(&'static str, &'static str), String> {
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
        other => Err(format!("Unknown Vue retention code: {other}")),
    }
}

pub(crate) fn element_classes(element: &HtmlElement) -> Vec<&str> {
    element.match_classes.as_ref().map_or_else(
        || {
            element
                .class_attribute
                .as_ref()
                .map(|attribute| attribute.value.split_whitespace().collect())
                .unwrap_or_default()
        },
        |classes| classes.iter().map(String::as_str).collect(),
    )
}

pub(crate) fn element_ids(element: &HtmlElement) -> Vec<&str> {
    element.match_ids.as_ref().map_or_else(
        || {
            element
                .id_attribute
                .as_ref()
                .map(|attribute| vec![attribute.value.as_str()])
                .unwrap_or_default()
        },
        |ids| ids.iter().map(String::as_str).collect(),
    )
}

fn element_tag(element: &HtmlElement) -> Option<&str> {
    element.match_tag.as_deref().or(element.tag.as_deref())
}

pub(crate) fn element_has_context(element: &HtmlElement, css_path: &str) -> bool {
    element.css_paths.is_empty() || element.css_paths.iter().any(|path| path == css_path)
}

/// Whether one of `rule`'s template sites (elements carrying the rule's
/// class or id) satisfies `reachable`, which receives the site's class list
/// and element.
fn rule_site_reachable(
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
fn stamp_in_file_shadow(
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

/// Rebase an offset that lies outside every edited range onto the post-edit
/// string produced by [`apply_edits`] with `edits` (sorted, non-overlapping).
fn shift_offset(edits: &[Edit], offset: usize) -> usize {
    let mut delta = 0isize;
    for edit in edits {
        if edit.end <= offset {
            delta += edit.replacement.len() as isize - (edit.end - edit.start) as isize;
        }
    }
    offset.checked_add_signed(delta).unwrap_or(offset)
}

pub(crate) fn original_offset(edit_batches: &[Vec<Edit>], mut offset: usize) -> usize {
    for edits in edit_batches.iter().rev() {
        let mut edits = edits.iter().collect::<Vec<_>>();
        edits.sort_by_key(|edit| (edit.start, edit.end));
        let mut delta = 0isize;
        for edit in edits {
            let Some(post_start) = edit.start.checked_add_signed(delta) else {
                continue;
            };
            let post_end = post_start + edit.replacement.len();
            if offset < post_start {
                break;
            }
            if offset < post_end {
                offset = edit.start;
                delta = 0;
                break;
            }
            delta += edit.replacement.len() as isize - (edit.end - edit.start) as isize;
        }
        offset = offset.checked_add_signed(-delta).unwrap_or(offset);
    }
    offset
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanRequest {
    #[serde(flatten)]
    sheet: BatchStylesheet,
    #[serde(default)]
    tailwind_path: Option<String>,
    #[serde(default)]
    tailwind_source: Option<String>,
    #[serde(default)]
    utility_prefix: Option<String>,
    #[serde(default)]
    theme_tokens: HashMap<String, String>,
    /// The entry group's fixed key-to-name map: normalized media condition
    /// keys to resolved variant names. `None` when extraction is disabled,
    /// in which case media handling is unchanged. An explicitly supplied
    /// empty map stays authoritative: every condition then uses the
    /// arbitrary fallback, never the legacy conversions.
    #[serde(default)]
    media_names: Option<HashMap<String, String>>,
    /// False when the resolved Tailwind entry must not be edited: keyframe
    /// and global at-rule movement is disabled so their rules retain with
    /// the existing warnings, and no entry file is planned.
    #[serde(default = "default_entry_writable")]
    entry_writable: bool,
    /// False for members that reuse an ancestor-shared entry: actively
    /// applied global at-rules stay in their modules while renamed
    /// keyframes may still move.
    #[serde(default = "default_entry_writable")]
    global_at_rule_moves: bool,
    files: Vec<SourceFile>,
}

fn default_entry_writable() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchPlanRequest {
    stylesheets: Vec<BatchStylesheet>,
    #[serde(default)]
    tailwind_path: Option<String>,
    #[serde(default)]
    tailwind_source: Option<String>,
    #[serde(default)]
    utility_prefix: Option<String>,
    #[serde(default)]
    theme_tokens: HashMap<String, String>,
    #[serde(default)]
    media_names: Option<HashMap<String, String>>,
    #[serde(default = "default_entry_writable")]
    entry_writable: bool,
    #[serde(default = "default_entry_writable")]
    global_at_rule_moves: bool,
    files: Vec<SourceFile>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchStylesheet {
    /// Per-stylesheet override of the request-level flag: false for a
    /// shared-entry member's stylesheet inside a group batch, or for a
    /// stylesheet whose registrations collide with another member's.
    #[serde(default)]
    global_at_rule_moves: Option<bool>,
    css_path: String,
    css_source: String,
    #[serde(default)]
    analysis_source: Option<String>,
    #[serde(default)]
    source_mappings: Vec<SourceMapping>,
    #[serde(default)]
    syntax: StylesheetSyntax,
    #[serde(default)]
    is_module: Option<bool>,
    #[serde(default)]
    is_partial: bool,
    #[serde(default)]
    css_module_id: Option<String>,
    #[serde(default)]
    css_dependents: Vec<String>,
    /// Present only for Vue SFC stylesheets: the plain-CSS `<style scoped>`
    /// blocks of the `.vue` file named by `css_path`, whose `css_source` is
    /// the whole SFC source.
    #[serde(default)]
    vue_blocks: Vec<VueBlock>,
    /// True for the `<style module>` entry of an SFC: consumers match proven
    /// `$style` binding sites instead of literal classes.
    #[serde(default)]
    vue_module: bool,
    /// Present only for Vue SFC stylesheets with an open template surface:
    /// the retention warning code every otherwise-unwarned rule receives.
    #[serde(default)]
    vue_retention: Option<String>,
    #[serde(default)]
    vue_unscoped: bool,
    /// Present only for closed Vue SFC stylesheets: the package's non-scoped
    /// CSS corpus as parseable pieces (other stylesheets, retained SFC
    /// blocks, scope-escape selector fragments). Their parsed selector
    /// surface decides which scoped rules may be deleted without handing the
    /// cascade to an unlayered competitor.
    #[serde(default)]
    vue_shadow_css: Vec<String>,
    /// CSS Module sources: their class and id names are localized at build
    /// time, so only their bare type and attribute selectors join the index.
    #[serde(default)]
    vue_shadow_module_css: Vec<String>,
    /// True when the shadow corpus contains selectors that cannot be proven
    /// (preprocessor interpolation or concatenation, inline HTML styles,
    /// unextractable escapes); every closed rule is then retained.
    #[serde(default)]
    vue_shadow_unverifiable: bool,
    /// Rules whose candidates failed Tailwind compilation in a previous
    /// planning pass; they are retained without converting anything.
    #[serde(default)]
    blocked_rules: Vec<RuleId>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceMapping {
    generated_line: usize,
    generated_column: usize,
    source_path: String,
    original_line: usize,
    original_column: usize,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HtmlAttribute {
    pub(crate) value: String,
    pub(crate) start: usize,
    pub(crate) end: usize,
    #[serde(default)]
    pub(crate) synthetic: bool,
    #[serde(default = "default_writable")]
    pub(crate) writable: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HtmlElement {
    pub(crate) class_attribute: Option<HtmlAttribute>,
    pub(crate) id_attribute: Option<HtmlAttribute>,
    /// Selector surface to match when the writable site is a Vue component
    /// call whose attributes fall through to a different root element.
    #[serde(default)]
    pub(crate) match_classes: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) match_ids: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) match_tag: Option<String>,
    /// Element tag name, provided by the Vue lowering for shadow matching.
    #[serde(default)]
    pub(crate) tag: Option<String>,
    /// A proven `:class="$style.x"` site: the module class name and the full
    /// attribute span to remove when the reference is rewritten.
    #[serde(default)]
    pub(crate) module_binding: Option<ModuleBinding>,
    /// Optional per-element stylesheet reachability used by cross-file Vue
    /// component proofs. Empty means every file-level HTML context applies.
    #[serde(default)]
    pub(crate) css_paths: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModuleBinding {
    pub(crate) name: String,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HtmlStylesheet {
    pub(crate) css_path: String,
    pub(crate) variants: Vec<String>,
    #[serde(default)]
    pub(crate) direct: bool,
    #[serde(default = "default_writable")]
    pub(crate) analyzable: bool,
}

fn default_writable() -> bool {
    true
}

#[derive(Clone, Deserialize)]
pub(crate) struct SourceFile {
    pub(crate) path: String,
    pub(crate) source: String,
    #[serde(default = "default_writable")]
    pub(crate) writable: bool,
    #[serde(default, rename = "htmlElements")]
    pub(crate) html_elements: Vec<HtmlElement>,
    #[serde(default, rename = "htmlStylesheets")]
    pub(crate) html_stylesheets: Vec<HtmlStylesheet>,
    #[serde(default = "default_writable", rename = "htmlReferencesSafe")]
    pub(crate) html_references_safe: bool,
    #[serde(default, rename = "htmlScriptText")]
    pub(crate) html_script_text: String,
    #[serde(skip)]
    pub(crate) prior_edits: Vec<Vec<Edit>>,
}

impl SourceFile {
    /// The stylesheet-link contexts through which this file consumes
    /// `css_path` with an analyzable relationship.
    pub(crate) fn analyzable_contexts(&self, css_path: &str) -> Vec<&HtmlStylesheet> {
        self.html_stylesheets
            .iter()
            .filter(|context| context.analyzable && context.css_path == css_path)
            .collect()
    }

    pub(crate) fn has_analyzable_context(&self, css_path: &str) -> bool {
        self.html_stylesheets
            .iter()
            .any(|context| context.analyzable && context.css_path == css_path)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanResponse {
    files: Vec<PlannedFile>,
    deleted_files: Vec<String>,
    unlinked_files: Vec<String>,
    candidates: Vec<String>,
    converted_rules: usize,
    retained_rules: usize,
    rules: Vec<RuleReport>,
    warnings: Vec<Warning>,
    #[serde(skip)]
    applied_edits: HashMap<String, Vec<Vec<Edit>>>,
}

#[derive(Serialize)]
struct PlannedFile {
    path: String,
    source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleReport {
    selector: String,
    status: &'static str,
    candidates: Vec<String>,
    file: String,
    rule_id: RuleId,
    /// Authored-domain rule span for anchoring caller-side warnings, or
    /// (0, 0) when the rule has no unique authored mapping.
    authored_span: RuleId,
    /// Index of the owning batch stylesheet entry. Same-path entries (a Vue
    /// SFC's scoped and module blocks) reuse local rule spans, so compile
    /// failures must be attributed per entry, not per path; the JS caller
    /// strips this before the public report.
    stylesheet: usize,
}

#[derive(Serialize)]
pub(crate) struct Warning {
    pub(crate) code: &'static str,
    pub(crate) file: String,
    /// Byte offsets into the authored file, or (0, 0) when a preprocessor
    /// rule has no unique authored mapping.
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) message: String,
}

impl Warning {
    pub(crate) fn new(
        code: &'static str,
        file: String,
        (start, end): (usize, usize),
        message: String,
    ) -> Self {
        Self {
            code,
            file,
            start,
            end,
            message,
        }
    }

    /// The shared warning for a generated utility overlapping a Tailwind
    /// class already present at the rewrite site.
    pub(crate) fn existing_tailwind_conflict(
        file: &str,
        span: (usize, usize),
        generated: &str,
        existing: &str,
    ) -> Self {
        Self::new(
            "existing-tailwind-conflict",
            file.to_string(),
            span,
            format!("Generated utility `{generated}` may conflict with existing `{existing}`."),
        )
    }
}

/// Every warning code the migration can emit, including reason strings that
/// flow through `rule.warning` and the JS-side `candidate-compilation-failure`
/// stamped in index.js. Adding or removing a code requires updating this list
/// and the README warning table; `tests::warning_codes_are_pinned_to_the_readme`
/// enforces both.
#[cfg(test)]
const WARNING_CODES: &[&str] = &[
    "aliased-css-module-reference",
    "batch-stylesheet-conflict",
    "candidate-compilation-failure",
    "component-class-target",
    "computed-css-module-reference",
    "cross-package-stylesheet-link",
    "css-module-composes",
    "dynamic-class-name",
    "dynamic-html-attribute",
    "dynamic-template-class",
    "existing-tailwind-conflict",
    "inferred-preprocessor-source",
    "media-query-definition-fallback",
    "module-utilities-conflict",
    "non-classname-css-module-reference",
    "open-root-fallthrough",
    "preprocessor-style-block",
    "rebuild-required",
    "reference-only-css-module-consumer",
    "retained-global-rule",
    "shadowed-scoped-rule",
    "shared-preprocessor-source",
    "unproven-css-module-relationship",
    "unproven-script-reference",
    "unproven-shared-entry-flow",
    "unproven-source-map",
    "unresolved-selector-target",
    "unscoped-style-block",
    "unsupported-animation",
    "unsupported-at-rule",
    "unsupported-container-query",
    "unsupported-css-module-reference",
    "unsupported-declaration",
    "unsupported-html-base",
    "unsupported-html-stylesheet-link",
    "unsupported-important",
    "unsupported-link-media",
    "unsupported-media-query",
    "unsupported-nested-at-rule",
    "unsupported-overlap",
    "unsupported-rule-content",
    "unsupported-selector",
    "unsupported-sfc-block",
    "unsupported-starting-style",
    "unsupported-supports-query",
    "unsupported-value",
    "unsupported-vue-version",
];

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Edit {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) replacement: String,
}

/// Single-stylesheet planning for unit tests: a thin wrapper that reshapes
/// the flat request into a one-stylesheet batch, the only production path.
#[cfg(test)]
pub fn plan_json(request: &str) -> Result<String, String> {
    let mut request: serde_json::Value =
        serde_json::from_str(request).map_err(|error| error.to_string())?;
    let stylesheet = request
        .as_object_mut()
        .ok_or_else(|| "Plan request must be an object".to_string())?;
    let mut batch = serde_json::Map::new();
    for field in [
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
            || segment
                .strip_prefix("max-")
                .is_some_and(|rest| self.theme_tokens.contains_key(&format!("breakpoint-{rest}")))
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
                if let Some(value) = rest.strip_prefix('[').and_then(|inner| inner.strip_suffix(']'))
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
                            || (upper.value == lower.value && !(upper.inclusive && lower.inclusive)))
                })
            })
        };
        fn split(constraints: &[WidthConstraint]) -> (Vec<&WidthConstraint>, Vec<&WidthConstraint>) {
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
        let conflicting = tailwind_utilities_conflict(
            &format!("probe:{}", tailwind_utility_parts(&left.candidate).1),
            &format!("probe:{}", tailwind_utility_parts(&right.candidate).1),
        ) || left.properties.iter().any(|left_property| {
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

/// Span of a rule in the analysis source (the compiled CSS for preprocessor
/// stylesheets), stable only across plans over identical `analysisSource`;
/// it round-trips through `blockedRules` in that domain.
#[derive(Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
struct RuleId {
    start: usize,
    end: usize,
}

fn rule_id(rule: &RulePlan) -> RuleId {
    RuleId {
        start: rule.span.start,
        end: rule.span.end,
    }
}

type RuleConflicts = HashMap<RuleId, BTreeSet<(String, String)>>;

struct RuleOrigin {
    rule: RuleId,
    properties: BTreeSet<String>,
}

struct CandidateMaps {
    candidates: HashMap<SelectorKey, Vec<String>>,
    origins: HashMap<(SelectorKey, String), Vec<RuleOrigin>>,
    rule_selectors: HashMap<RuleId, String>,
    retained_rules: HashSet<RuleId>,
    /// Multi-compound module rules whose relationship proof failed, with the
    /// retained-rule message.
    unproven: HashMap<RuleId, String>,
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

struct BatchMatch {
    stylesheet: usize,
    candidate: String,
    rule: RuleId,
    properties: BTreeSet<String>,
}

#[allow(clippy::too_many_arguments)]
fn plan_consumer_file(
    file: &SourceFile,
    css_path: &str,
    is_module: bool,
    candidates: &HashMap<SelectorKey, Vec<String>>,
    preserved_module_classes: &BTreeSet<String>,
    module_rule_classes: Option<&BTreeSet<String>>,
    utility_prefix: Option<&str>,
    vue_unscoped: bool,
    vue_module: bool,
) -> Result<SourcePlan, String> {
    // Vue scoped styles never apply outside their own SFC, and a `.vue` file
    // is not parseable JS: the only live pairing is an SFC consuming its own
    // scoped blocks through the HTML contract. A `.vue` consumer of any other
    // stylesheet is an opaque reference that can only retain a module.
    let stylesheet_is_vue = is_vue_path(css_path);
    let file_is_vue = is_vue_path(&file.path);
    if stylesheet_is_vue && vue_module {
        if file_is_vue && file.has_analyzable_context(css_path) {
            return Ok(plan_vue_module_file(
                file,
                css_path,
                candidates,
                module_rule_classes,
                utility_prefix,
            ));
        }
        return Ok(SourcePlan::default());
    }
    if stylesheet_is_vue && vue_unscoped {
        if file_is_vue
            || Path::new(&file.path)
                .extension()
                .is_some_and(|ext| ext == "html")
        {
            return Ok(plan_html_file(file, css_path, candidates, utility_prefix));
        }
        return plan_batch_source_file(file, css_path, false, candidates, preserved_module_classes);
    }
    if stylesheet_is_vue || file_is_vue {
        if file_is_vue && file.has_analyzable_context(css_path) {
            return Ok(plan_html_file(file, css_path, candidates, utility_prefix));
        }
        if file_is_vue && !stylesheet_is_vue {
            return Ok(opaque_reference_plan(file, css_path, is_module));
        }
        return Ok(SourcePlan::default());
    }
    if Path::new(&file.path)
        .extension()
        .is_some_and(|extension| extension == "html")
    {
        return Ok(plan_html_file(file, css_path, candidates, utility_prefix));
    }
    plan_batch_source_file(
        file,
        css_path,
        is_module,
        candidates,
        preserved_module_classes,
    )
}

pub(crate) fn is_recoverable_input_error(error: &str) -> bool {
    (!error.starts_with("Failed to parse edited CSS") && error.starts_with("Failed to parse "))
        || error.starts_with("Failed to analyze ")
        || error.starts_with("Unsupported source file ")
}

pub fn plan_batch_json(request: &str) -> Result<String, String> {
    let request: BatchPlanRequest =
        serde_json::from_str(request).map_err(|error| error.to_string())?;
    if request.stylesheets.is_empty() {
        return Err("Batch migration requires at least one stylesheet".to_string());
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
        snapshot_files = plan_request.files;
        for file in request.files.iter().filter(|file| file.writable) {
            let result = plan_consumer_file(
                file,
                &stylesheet.css_path,
                stylesheet
                    .is_module
                    .unwrap_or_else(|| is_stylesheet_module(&stylesheet.css_path)),
                &maps.candidates,
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
                    && (tailwind_utilities_conflict(&left.candidate, &right.candidate)
                        || (tailwind_variants_match(&left.candidate, &right.candidate)
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
        converted_rules,
        retained_rules,
        rules,
        warnings,
        applied_edits,
    })
    .map_err(|error| error.to_string())
}

fn batch_stylesheet_request(
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

fn map_rule_spans(
    authored_source: &str,
    syntax: StylesheetSyntax,
    source_path: &str,
    source_mappings: &[SourceMapping],
    analysis_source: &str,
    rules: &mut [RulePlan],
    authored_base: usize,
) -> Result<(), String> {
    let allocator = oxc_css_parser::Allocator::default();
    let stylesheet = parse_css(&allocator, authored_source, syntax.parser_syntax())
        .map_err(|error| format!("Failed to parse {source_path}: {error}"))?;
    let mut authored_rules = Vec::new();
    collect_qualified_rule_spans(&stylesheet.statements, &mut authored_rules);
    let mappings = source_mappings
        .iter()
        .map(|mapping| ((mapping.generated_line, mapping.generated_column), mapping))
        .collect::<HashMap<_, _>>();

    for rule in rules.iter_mut() {
        let mut original_offsets = Vec::new();
        for generated_offset in &rule.provenance_offsets {
            let Some(position) = offset_to_line_column(analysis_source, *generated_offset) else {
                original_offsets.clear();
                break;
            };
            let Some(mapping) = mappings.get(&position) else {
                original_offsets.clear();
                break;
            };
            if mapping.source_path != source_path {
                original_offsets.clear();
                break;
            }
            let Some(offset) = line_column_to_offset(
                authored_source,
                mapping.original_line,
                mapping.original_column,
            ) else {
                original_offsets.clear();
                break;
            };
            original_offsets.push(offset);
        }
        if original_offsets.is_empty() {
            continue;
        }
        rule.authored_span = authored_rules
            .iter()
            .filter(|(span, _)| {
                original_offsets
                    .iter()
                    .all(|offset| span.start <= *offset && *offset < span.end)
            })
            .min_by_key(|(span, _)| span.end - span.start)
            .map(|(span, _)| authored_base + span.start..authored_base + span.end);
    }

    let mut shared_spans: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (index, rule) in rules.iter().enumerate() {
        if let Some(span) = &rule.authored_span {
            shared_spans
                .entry((span.start, span.end))
                .or_default()
                .push(index);
        }
    }
    let mut ambiguous = BTreeSet::new();
    for indices in shared_spans.values().filter(|indices| indices.len() > 1) {
        ambiguous.extend(indices.iter().copied());
    }
    for left in 0..rules.len() {
        let Some(left_span) = &rules[left].authored_span else {
            continue;
        };
        for (right, right_rule) in rules.iter().enumerate().skip(left + 1) {
            let Some(right_span) = &right_rule.authored_span else {
                continue;
            };
            if left_span.start < right_span.end && right_span.start < left_span.end {
                ambiguous.extend([left, right]);
            }
        }
    }
    for index in ambiguous {
        rules[index].authored_span = None;
    }
    let interpolation = match syntax {
        StylesheetSyntax::Scss | StylesheetSyntax::Sass => Some("#{"),
        StylesheetSyntax::Less => Some("@{"),
        StylesheetSyntax::Css => None,
    };
    if let Some(interpolation) = interpolation {
        for rule in rules {
            let interpolated = rule.authored_span.as_ref().is_some_and(|authored_span| {
                authored_rules.iter().any(|(span, selector_span)| {
                    authored_span.start == authored_base + span.start
                        && authored_span.end == authored_base + span.end
                        && authored_source[selector_span.clone()].contains(interpolation)
                })
            });
            if interpolated {
                rule.authored_span = None;
            }
        }
    }
    Ok(())
}

fn collect_qualified_rule_spans(
    statements: &[Statement<'_>],
    spans: &mut Vec<(std::ops::Range<usize>, std::ops::Range<usize>)>,
) {
    for statement in statements {
        match statement {
            Statement::QualifiedRule(rule) => {
                spans.push((
                    rule.span.start..rule.span.end,
                    rule.selector.span.start..rule.selector.span.end,
                ));
                collect_qualified_rule_spans(&rule.block.statements, spans);
            }
            Statement::AtRule(at_rule) => {
                if let Some(block) = &at_rule.block {
                    collect_qualified_rule_spans(&block.statements, spans);
                }
            }
            _ => {}
        }
    }
}

fn offset_to_line_column(source: &str, offset: usize) -> Option<(usize, usize)> {
    if offset > source.len() || !source.is_char_boundary(offset) {
        return None;
    }
    let mut line = 0;
    let mut column = 0;
    for character in source[..offset].chars() {
        if character == '\n' {
            line += 1;
            column = 0;
        } else {
            column += character.len_utf16();
        }
    }
    Some((line, column))
}

fn line_column_to_offset(source: &str, target_line: usize, target_column: usize) -> Option<usize> {
    let mut line = 0;
    let mut column = 0;
    for (offset, character) in source.char_indices() {
        if line == target_line && column == target_column {
            return Some(offset);
        }
        if character == '\n' {
            line += 1;
            column = 0;
        } else {
            column += character.len_utf16();
            if line == target_line && column > target_column {
                return None;
            }
        }
    }
    (line == target_line && column == target_column).then_some(source.len())
}

fn mentions_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let bytes = text.as_bytes();
    text.match_indices(word).any(|(start, _)| {
        let end = start + word.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
        before_ok && after_ok
    })
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn dedup_candidate_map(candidate_map: &mut HashMap<SelectorKey, Vec<String>>) {
    for candidates in candidate_map.values_mut() {
        candidates.sort();
        candidates.dedup();
    }
}

fn candidate_map_for_request(
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

fn plan_request(
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

/// Apply the merged template and scoped-block edits to a `.vue` source, drop
/// conditional at-rules emptied by rule removal, delete blocks whose CSS is
/// gone entirely, and validate that the remaining scoped CSS still parses.
/// The masked copy stays byte-aligned with the real source throughout so
/// masked-domain spans remain valid for both.
fn finish_vue_stylesheet(
    request: &PlanRequest,
    masked: &str,
    mut edits: Vec<Edit>,
) -> Result<(String, Vec<Vec<Edit>>), String> {
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
        let stylesheet = parse_css(&allocator, masked, Syntax::Css)
            .map_err(|error| format!("Failed to parse edited CSS: {error}"))?;
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
        let stylesheet = parse_css(&allocator, &masked, Syntax::Css)
            .map_err(|error| format!("Failed to parse edited CSS: {error}"))?;
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
) -> Result<(String, Vec<Vec<Edit>>), String> {
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
) -> Result<Vec<Edit>, String> {
    let mut removals = Vec::new();
    for (block, original) in blocks.iter().zip(&request.sheet.vue_blocks) {
        let originally_empty = request.sheet.css_source
            [original.content_start..original.content_end]
            .trim()
            .is_empty();
        let content = content_view
            .get(block.content_start..block.content_end)
            .ok_or_else(|| "Invalid Vue style block span".to_string())?;
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

fn apply_edits(source: &str, mut edits: Vec<Edit>) -> Result<String, String> {
    edits.sort_by_key(|edit| (edit.start, edit.end));
    for pair in edits.windows(2) {
        if pair[0].end > pair[1].start {
            return Err("Overlapping source edits were produced".to_string());
        }
    }
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    for edit in edits {
        if edit.end > source.len() || edit.start > edit.end {
            return Err("Invalid source edit span".to_string());
        }
        output.push_str(&source[cursor..edit.start]);
        output.push_str(&edit.replacement);
        cursor = edit.end;
    }
    output.push_str(&source[cursor..]);
    Ok(output)
}

fn remove_empty_conditionals(mut source: String, syntax: Syntax) -> Result<String, String> {
    loop {
        let allocator = oxc_css_parser::Allocator::default();
        let stylesheet = parse_css(&allocator, &source, syntax)
            .map_err(|error| format!("Failed to parse edited CSS: {error}"))?;
        let mut edits = Vec::new();
        collect_empty_conditionals(&stylesheet.statements, &mut edits);
        if edits.is_empty() {
            return Ok(source);
        }
        source = apply_edits(&source, edits)?;
    }
}

fn collect_empty_conditionals(statements: &[Statement<'_>], edits: &mut Vec<Edit>) {
    for statement in statements {
        let Statement::AtRule(at_rule) = statement else {
            continue;
        };
        let Some(block) = &at_rule.block else {
            continue;
        };
        if is_conditional(at_rule.name.name) && block.statements.is_empty() {
            edits.push(Edit {
                start: at_rule.span.start,
                end: at_rule.span.end,
                replacement: String::new(),
            });
        } else {
            collect_empty_conditionals(&block.statements, edits);
        }
    }
}

pub(crate) fn validate_css(source: &str) -> Result<(), String> {
    validate_stylesheet(source, Syntax::Css)
}

fn validate_stylesheet(source: &str, syntax: Syntax) -> Result<(), String> {
    let allocator = oxc_css_parser::Allocator::default();
    parse_css(&allocator, source, syntax)
        .map(|_| ())
        .map_err(|error| format!("Edited stylesheet no longer parses: {error}"))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use super::{SourceFile, apply_edits, plan_batch_json, plan_json};
    use crate::animations::{KeyframePlan, animation_candidate, append_keyframes};
    use crate::css_plan::SelectorKey;
    use crate::js_rewrite::plan_batch_source_file;
    use crate::utilities::{
        css_properties_conflict, declaration_to_candidate, tailwind_utilities_conflict,
    };

    fn plan(request: serde_json::Value) -> serde_json::Value {
        serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap()
    }

    fn plan_batch(request: serde_json::Value) -> serde_json::Value {
        serde_json::from_str(&plan_batch_json(&request.to_string()).unwrap()).unwrap()
    }

    mod batch;
    mod css_modules;
    mod expressions;
    mod media;
    mod preprocessors;
    mod relationships;
    mod utilities;
    mod vue;

    #[test]
    fn warning_codes_are_pinned_to_the_readme() {
        let readme = include_str!("../../../README.md");
        let documented = readme
            .lines()
            .filter_map(|line| line.strip_prefix("| `")?.split('`').next())
            .collect::<Vec<_>>();
        assert_eq!(
            documented,
            super::WARNING_CODES,
            "the README warning table must list exactly the emitted codes, sorted"
        );

        // Strip the canonical list itself so it cannot satisfy its own check.
        let planner = include_str!("planner.rs");
        let const_start = planner.find("const WARNING_CODES").unwrap();
        let const_end = const_start + planner[const_start..].find("];").unwrap();
        // Scan every crate source and every repo src/ TypeScript file,
        // recursively so a new module or subdirectory cannot silently escape
        // the pinning check.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sources = format!("{}\n{}", &planner[..const_start], &planner[const_end..]);
        for (dir, extension) in [
            (manifest.join("src"), "rs"),
            (manifest.join("../../src"), "ts"),
        ] {
            let mut pending = vec![dir];
            while let Some(directory) = pending.pop() {
                for entry in std::fs::read_dir(directory).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        pending.push(path);
                    } else if path.extension().is_some_and(|ext| ext == extension)
                        && path.file_name().is_some_and(|name| name != "planner.rs")
                    {
                        sources.push('\n');
                        sources.push_str(&std::fs::read_to_string(path).unwrap());
                    }
                }
            }
        }
        for code in super::WARNING_CODES {
            assert!(
                sources.contains(&format!("\"{code}\"")) || sources.contains(&format!("'{code}'")),
                "documented warning code `{code}` no longer appears in the sources"
            );
        }

        // Every directly constructed warning code must be documented, whether
        // stamped as a `code:` field or passed positionally to `htmlWarning`.
        // Reason strings flowing through `rule.warning` are covered by the
        // check above plus the comment on WARNING_CODES. The patterns are
        // built at runtime so this test's own source cannot match them.
        let field_sites = format!("{}: ", "code");
        let helper_sites = format!("{}(", "htmlWarning");
        for pattern in [field_sites.as_str(), helper_sites.as_str()] {
            for site in sources.split(pattern).skip(1) {
                let site = site.trim_start();
                let Some(quote) = site.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
                    continue;
                };
                let code = site[1..].split(quote).next().unwrap();
                assert!(
                    super::WARNING_CODES.contains(&code),
                    "emitted warning code `{code}` is missing from WARNING_CODES and the README"
                );
            }
        }
    }
}

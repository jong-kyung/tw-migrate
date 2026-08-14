use super::*;

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

pub(super) fn element_tag(element: &HtmlElement) -> Option<&str> {
    element.match_tag.as_deref().or(element.tag.as_deref())
}

pub(crate) fn element_has_context(element: &HtmlElement, css_path: &str) -> bool {
    element.css_paths.is_empty() || element.css_paths.iter().any(|path| path == css_path)
}

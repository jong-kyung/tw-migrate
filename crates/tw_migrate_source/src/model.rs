use serde::Deserialize;

use crate::Edit;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HtmlAttribute {
    pub value: String,
    pub start: usize,
    pub end: usize,
    #[serde(default)]
    pub synthetic: bool,
    #[serde(default = "default_writable")]
    pub writable: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HtmlElement {
    pub class_attribute: Option<HtmlAttribute>,
    pub id_attribute: Option<HtmlAttribute>,
    /// Selector surface to match when the writable site is a Vue component
    /// call whose attributes fall through to a different root element.
    #[serde(default)]
    pub match_classes: Option<Vec<String>>,
    #[serde(default)]
    pub match_ids: Option<Vec<String>>,
    #[serde(default)]
    pub match_tag: Option<String>,
    /// Element tag name, provided by the Vue lowering for shadow matching.
    #[serde(default)]
    pub tag: Option<String>,
    /// A proven `:class="$style.x"` site: the module class name and the full
    /// attribute span to remove when the reference is rewritten.
    #[serde(default)]
    pub module_binding: Option<ModuleBinding>,
    /// Optional per-element stylesheet reachability used by cross-file Vue
    /// component proofs. Empty means every file-level HTML context applies.
    #[serde(default)]
    pub css_paths: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleBinding {
    pub name: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HtmlStylesheet {
    pub css_path: String,
    pub variants: Vec<String>,
    #[serde(default)]
    pub direct: bool,
    #[serde(default = "default_writable")]
    pub analyzable: bool,
}

fn default_writable() -> bool {
    true
}

#[derive(Clone, Deserialize)]
pub struct SourceFile {
    pub path: String,
    pub source: String,
    #[serde(default = "default_writable")]
    pub writable: bool,
    #[serde(default, rename = "htmlElements")]
    pub html_elements: Vec<HtmlElement>,
    #[serde(default, rename = "htmlStylesheets")]
    pub html_stylesheets: Vec<HtmlStylesheet>,
    #[serde(default = "default_writable", rename = "htmlReferencesSafe")]
    pub html_references_safe: bool,
    #[serde(default, rename = "htmlScriptText")]
    pub html_script_text: String,
    #[serde(skip)]
    pub prior_edits: Vec<Vec<Edit>>,
}

impl SourceFile {
    /// The stylesheet-link contexts through which this file consumes
    /// `css_path` with an analyzable relationship.
    pub fn analyzable_contexts(&self, css_path: &str) -> Vec<&HtmlStylesheet> {
        self.html_stylesheets
            .iter()
            .filter(|context| context.analyzable && context.css_path == css_path)
            .collect()
    }

    pub fn has_analyzable_context(&self, css_path: &str) -> bool {
        self.html_stylesheets
            .iter()
            .any(|context| context.analyzable && context.css_path == css_path)
    }
}

pub fn element_classes(element: &HtmlElement) -> Vec<&str> {
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

pub fn element_ids(element: &HtmlElement) -> Vec<&str> {
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

pub fn element_tag(element: &HtmlElement) -> Option<&str> {
    element.match_tag.as_deref().or(element.tag.as_deref())
}

pub fn element_has_context(element: &HtmlElement, css_path: &str) -> bool {
    element.css_paths.is_empty() || element.css_paths.iter().any(|path| path == css_path)
}

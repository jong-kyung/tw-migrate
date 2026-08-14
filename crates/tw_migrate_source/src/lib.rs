mod diagnostic;
mod edit;
#[path = "rewrite/html.rs"]
mod html_rewrite;
#[path = "rewrite/js/mod.rs"]
mod js_rewrite;
mod jsx;
mod model;
#[path = "analysis/source.rs"]
mod source_analysis;

pub use diagnostic::Warning;
pub use edit::{Edit, original_offset, shift_offset};
pub use html_rewrite::{
    candidates_fit_attribute, plan_html_file, plan_vue_module_file, rebase_span,
};
pub use js_rewrite::{
    CandidateMatch, SourcePlan, opaque_reference_plan, plan_batch_source_file, validate_js,
};
pub use jsx::{PreparedWorld, ProofOutcome, UsageProof, prepare, prove_prepared};
pub use model::{
    HtmlAttribute, HtmlElement, HtmlStylesheet, ModuleBinding, SourceFile, element_classes,
    element_has_context, element_ids, element_tag,
};
pub use source_analysis::{expression_analysis_json, source_analysis_json};

use js_rewrite::{normalize_path, resolve_import, source_type_for_path};

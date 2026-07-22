mod animations;
mod arbitrary;
mod at_rules;
mod css_plan;
mod js_rewrite;
mod jsx_graph;
mod planner;
mod theme;
mod utilities;

use napi_derive::napi;

#[napi]
pub fn plan_migration(request: String) -> napi::Result<String> {
    planner::plan_json(&request).map_err(|error| napi::Error::from_reason(error.to_string()))
}

const RECOVERABLE_INPUT_ERROR: &str = "TW_MIGRATE_RECOVERABLE_INPUT:";

#[napi]
pub fn plan_batch_migration(request: String) -> napi::Result<String> {
    planner::plan_batch_json(&request).map_err(|error| {
        let reason = if planner::is_recoverable_input_error(&error) {
            format!("{RECOVERABLE_INPUT_ERROR}{error}")
        } else {
            error
        };
        napi::Error::from_reason(reason)
    })
}

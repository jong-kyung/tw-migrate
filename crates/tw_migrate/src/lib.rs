mod animations;
mod arbitrary;
mod at_rules;
mod css_plan;
mod js_rewrite;
mod planner;
mod theme;
mod utilities;

use napi_derive::napi;

#[napi]
pub fn plan_migration(request: String) -> napi::Result<String> {
    planner::plan_json(&request).map_err(|error| napi::Error::from_reason(error.to_string()))
}

#[napi]
pub fn plan_batch_migration(request: String) -> napi::Result<String> {
    planner::plan_batch_json(&request).map_err(|error| napi::Error::from_reason(error.to_string()))
}

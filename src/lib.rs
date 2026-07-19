mod planner;

use napi_derive::napi;

#[napi]
pub fn plan_migration(request: String) -> napi::Result<String> {
    planner::plan_json(&request).map_err(|error| napi::Error::from_reason(error.to_string()))
}

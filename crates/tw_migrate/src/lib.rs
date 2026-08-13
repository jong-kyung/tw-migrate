mod animations;
mod arbitrary;
mod at_rules;
mod css_directives;
mod css_plan;
mod html_rewrite;
mod js_rewrite;
mod jsx_graph;
mod media;
mod planner;
mod source_analysis;
mod stylesheet_analysis;
mod theme;
mod utilities;

use napi_derive::napi;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DecodedSourceMapping {
    generated_line: u32,
    generated_column: u32,
    source: String,
    original_line: u32,
    original_column: u32,
}

fn decode_source_map_json(source_map: &str) -> Result<String, String> {
    let source_map = oxc_sourcemap::SourceMap::from_json_string(source_map)
        .map_err(|error| format!("Failed to decode source map: {error}"))?;
    let mappings = source_map
        .get_tokens()
        .filter_map(|token| {
            let source = source_map.get_source(token.get_source_id()?)?;
            Some(DecodedSourceMapping {
                generated_line: token.get_dst_line(),
                generated_column: token.get_dst_col(),
                source: source.to_owned(),
                original_line: token.get_src_line(),
                original_column: token.get_src_col(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&mappings).map_err(|error| error.to_string())
}

#[napi]
pub fn decode_source_map(source_map: String) -> napi::Result<String> {
    decode_source_map_json(&source_map).map_err(napi::Error::from_reason)
}

#[napi]
pub fn validate_css(source: String) -> napi::Result<()> {
    planner::validate_css(&source).map_err(napi::Error::from_reason)
}

#[napi]
pub fn expression_analysis(path: String, source: String) -> napi::Result<String> {
    source_analysis::expression_analysis_json(&path, &source).map_err(napi::Error::from_reason)
}

#[napi]
pub fn source_analysis(path: String, source: String) -> napi::Result<String> {
    source_analysis::source_analysis_json(&path, &source).map_err(napi::Error::from_reason)
}

#[napi]
pub fn stylesheet_analysis(path: String, source: String) -> napi::Result<String> {
    stylesheet_analysis::stylesheet_analysis_json(&path, &source).map_err(napi::Error::from_reason)
}

#[napi]
pub fn compiled_shape(css: String) -> napi::Result<String> {
    stylesheet_analysis::compiled_shape_json(&css).map_err(napi::Error::from_reason)
}

#[napi]
pub fn collect_css_directives(source: String) -> napi::Result<String> {
    css_directives::collect_css_directives_json(&source).map_err(|error| napi::Error::from_reason(error))
}

#[napi]
pub fn media_probe_key(css: String) -> napi::Result<String> {
    media::media_probe_key_json(&css).map_err(napi::Error::from_reason)
}

#[napi]
pub fn collect_media_conditions(request: String) -> napi::Result<String> {
    media::collect_media_conditions_json(&request).map_err(|error| {
        // Package input failures stay recoverable so `--force` can skip the
        // affected package, matching plan_batch_migration.
        let reason = if planner::is_recoverable_input_error(&error) {
            format!("{RECOVERABLE_INPUT_ERROR}{error}")
        } else {
            error
        };
        napi::Error::from_reason(reason)
    })
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

#[cfg(test)]
mod tests {
    #[test]
    fn decodes_source_map_mappings() {
        let decoded = super::decode_source_map_json(
            r#"{"version":3,"sources":["input.scss"],"names":[],"mappings":"AAAA"}"#,
        )
        .unwrap();
        assert_eq!(
            decoded,
            r#"[{"generatedLine":0,"generatedColumn":0,"source":"input.scss","originalLine":0,"originalColumn":0}]"#
        );
    }
}

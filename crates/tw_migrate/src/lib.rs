use napi_derive::napi;
use tw_migrate_error::MigrationError;

const RECOVERABLE_INPUT_ERROR: &str = "TW_MIGRATE_RECOVERABLE_INPUT:";

fn native_error_reason(error: &MigrationError, prefix_recoverable: bool) -> String {
    if prefix_recoverable && error.is_recoverable() {
        format!("{RECOVERABLE_INPUT_ERROR}{error}")
    } else {
        error.to_string()
    }
}

fn native_error(error: MigrationError, prefix_recoverable: bool) -> napi::Error {
    napi::Error::from_reason(native_error_reason(&error, prefix_recoverable))
}

#[napi]
pub fn decode_source_map(source_map: String) -> napi::Result<String> {
    tw_migrate_planner::decode_source_map_json(&source_map)
        .map_err(|error| native_error(error, false))
}

#[napi]
pub fn validate_css(source: String) -> napi::Result<()> {
    tw_migrate_css::validate_css(&source).map_err(|error| native_error(error, false))
}

#[napi]
pub fn expression_analysis(path: String, source: String) -> napi::Result<String> {
    tw_migrate_source::expression_analysis_json(&path, &source)
        .map_err(|error| native_error(error, false))
}

#[napi]
pub fn source_analysis(path: String, source: String) -> napi::Result<String> {
    tw_migrate_source::source_analysis_json(&path, &source)
        .map_err(|error| native_error(error, false))
}

#[napi]
pub fn stylesheet_analysis(path: String, source: String) -> napi::Result<String> {
    tw_migrate_css::stylesheet_analysis_json(&path, &source)
        .map_err(|error| native_error(error, false))
}

#[napi]
pub fn collect_css_directives(source: String) -> napi::Result<String> {
    tw_migrate_css::collect_css_directives_json(&source).map_err(|error| native_error(error, false))
}

#[napi]
pub fn media_probe_key(css: String) -> napi::Result<String> {
    tw_migrate_css::media_probe_key_json(&css).map_err(|error| native_error(error, false))
}

#[napi]
pub fn collect_media_conditions(request: String) -> napi::Result<String> {
    // Package input failures stay recoverable so `--force` can skip the
    // affected package, matching plan_batch_migration.
    tw_migrate_css::collect_media_conditions_json(&request)
        .map_err(|error| native_error(error, true))
}

#[napi]
pub fn plan_batch_migration(request: String) -> napi::Result<String> {
    tw_migrate_planner::plan_batch_json(&request).map_err(|error| native_error(error, true))
}

#[cfg(test)]
mod tests {
    use super::RECOVERABLE_INPUT_ERROR;

    #[test]
    fn recoverable_prefix_is_endpoint_specific() {
        let media = tw_migrate_css::collect_media_conditions_json(
            r#"{"stylesheets":[{"cssPath":"app.css","cssSource":"@media \u000bscreen {}"}]}"#,
        )
        .unwrap_err();
        assert!(matches!(
            media,
            tw_migrate_error::MigrationError::AuthoredStylesheetParse { .. }
        ));
        assert!(media.is_recoverable());
        assert!(super::native_error_reason(&media, true).starts_with(RECOVERABLE_INPUT_ERROR));

        let plan = tw_migrate_planner::plan_batch_json(
            r#"{"stylesheets":[{"cssPath":"app.css","cssSource":"@media \u000bscreen {}"}],"files":[]}"#,
        )
        .unwrap_err();
        assert!(matches!(
            plan,
            tw_migrate_error::MigrationError::AuthoredStylesheetParse { .. }
        ));
        assert!(plan.is_recoverable());
        assert!(super::native_error_reason(&plan, true).starts_with(RECOVERABLE_INPUT_ERROR));

        let expression =
            tw_migrate_source::expression_analysis_json("app.js", "const =").unwrap_err();
        assert!(matches!(
            expression,
            tw_migrate_error::MigrationError::SourceParse { .. }
        ));
        let expression_reason = super::native_error_reason(&expression, false);
        assert_eq!(expression_reason, expression.to_string());
        assert!(!expression_reason.starts_with(RECOVERABLE_INPUT_ERROR));

        let source = tw_migrate_source::source_analysis_json("app.js", "const =").unwrap_err();
        assert!(matches!(
            source,
            tw_migrate_error::MigrationError::SourceParse { .. }
        ));
        assert_eq!(
            super::native_error_reason(&source, false),
            "Failed to parse app.js"
        );
    }

    #[test]
    fn other_native_boundaries_never_prefix_recoverable_errors() {
        let errors = [
            tw_migrate_css::stylesheet_analysis_json("app.css", "@media \u{b}screen {}")
                .unwrap_err(),
            tw_migrate_css::collect_css_directives_json("@media \u{b}screen {}").unwrap_err(),
            tw_migrate_css::media_probe_key_json("@media \u{b}screen {}").unwrap_err(),
            tw_migrate_planner::decode_source_map_json("{").unwrap_err(),
        ];
        for error in errors {
            let reason = super::native_error_reason(&error, false);
            assert_eq!(reason, error.to_string());
            assert!(!reason.starts_with(RECOVERABLE_INPUT_ERROR));
        }
    }

    #[test]
    fn invalid_requests_are_not_recoverable() {
        let media = tw_migrate_css::collect_media_conditions_json("{").unwrap_err();
        assert!(matches!(
            media,
            tw_migrate_error::MigrationError::InvalidRequest { .. }
        ));
        assert!(!media.is_recoverable());
        assert!(!super::native_error_reason(&media, true).starts_with(RECOVERABLE_INPUT_ERROR));

        let plan = tw_migrate_planner::plan_batch_json("{").unwrap_err();
        assert!(matches!(
            plan,
            tw_migrate_error::MigrationError::InvalidRequest { .. }
        ));
        assert!(!plan.is_recoverable());
        assert!(!super::native_error_reason(&plan, true).starts_with(RECOVERABLE_INPUT_ERROR));
    }
}

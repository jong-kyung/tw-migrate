use std::{error::Error, fmt};

/// A migration failure classified by the operation that produced it.
///
/// Variants retain rendered diagnostics as owned strings so parser-specific
/// types do not cross capability-crate boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationError {
    InvalidRequest { message: String },
    Serialization { message: String },
    AuthoredStylesheetParse { message: String },
    EditedStylesheetParse { message: String },
    SourceParse { message: String },
    SourceAnalysis { message: String },
    UnsupportedSource { message: String },
    SourceMap { message: String },
    InvalidEdit { message: String },
    PlanCollision { message: String },
    OutputValidation { message: String },
    Invariant { message: String },
}

impl MigrationError {
    /// Whether a package migration may be skipped under `--force`.
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::AuthoredStylesheetParse { .. }
                | Self::SourceParse { .. }
                | Self::SourceAnalysis { .. }
                | Self::UnsupportedSource { .. }
        )
    }

    fn message(&self) -> &str {
        match self {
            Self::InvalidRequest { message }
            | Self::Serialization { message }
            | Self::AuthoredStylesheetParse { message }
            | Self::EditedStylesheetParse { message }
            | Self::SourceParse { message }
            | Self::SourceAnalysis { message }
            | Self::UnsupportedSource { message }
            | Self::SourceMap { message }
            | Self::InvalidEdit { message }
            | Self::PlanCollision { message }
            | Self::OutputValidation { message }
            | Self::Invariant { message } => message,
        }
    }
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl Error for MigrationError {}

pub type MigrationResult<T> = Result<T, MigrationError>;

#[cfg(test)]
mod tests {
    use super::MigrationError;

    #[test]
    fn display_preserves_the_owned_message() {
        let error = MigrationError::AuthoredStylesheetParse {
            message: "Failed to parse app.css: unexpected token".into(),
        };
        assert_eq!(
            error.to_string(),
            "Failed to parse app.css: unexpected token"
        );
    }

    #[test]
    fn recoverability_is_structural() {
        for error in [
            MigrationError::AuthoredStylesheetParse {
                message: "message without a parse prefix".into(),
            },
            MigrationError::SourceParse {
                message: String::new(),
            },
            MigrationError::SourceAnalysis {
                message: String::new(),
            },
            MigrationError::UnsupportedSource {
                message: String::new(),
            },
        ] {
            assert!(error.is_recoverable(), "{error:?}");
        }

        for error in [
            MigrationError::InvalidRequest {
                message: "Failed to parse request-shaped text".into(),
            },
            MigrationError::Serialization {
                message: String::new(),
            },
            MigrationError::EditedStylesheetParse {
                message: String::new(),
            },
            MigrationError::SourceMap {
                message: "Failed to parse source-map-shaped text".into(),
            },
            MigrationError::InvalidEdit {
                message: String::new(),
            },
            MigrationError::PlanCollision {
                message: String::new(),
            },
            MigrationError::OutputValidation {
                message: String::new(),
            },
            MigrationError::Invariant {
                message: String::new(),
            },
        ] {
            assert!(!error.is_recoverable(), "{error:?}");
        }
    }
}

use serde::Serialize;

/// Same codes as `backend/src/errors.ts`, plus `EngineMissing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    EConvertFailed,
    ETimeout,
    EEncrypted,
    EUnsupported,
    EUnsupportedTarget,
    EUnknownTarget,
    ETooLarge,
    ENoTables,
    ENoLayers,
    ENotTabular,
    EBadPageRange,
    ETooFewFiles,
    EWrongPassword,
    EInvalidField,
    EBusy,
    EBadRequest,
    EInternal,
    EEngineMissing,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EConvertFailed => "E_CONVERT_FAILED",
            Self::ETimeout => "E_TIMEOUT",
            Self::EEncrypted => "E_ENCRYPTED",
            Self::EUnsupported => "E_UNSUPPORTED",
            Self::EUnsupportedTarget => "E_UNSUPPORTED_TARGET",
            Self::EUnknownTarget => "E_UNKNOWN_TARGET",
            Self::ETooLarge => "E_TOO_LARGE",
            Self::ENoTables => "E_NO_TABLES",
            Self::ENoLayers => "E_NO_LAYERS",
            Self::ENotTabular => "E_NOT_TABULAR",
            Self::EBadPageRange => "E_BAD_PAGE_RANGE",
            Self::ETooFewFiles => "E_TOO_FEW_FILES",
            Self::EWrongPassword => "E_WRONG_PASSWORD",
            Self::EInvalidField => "E_INVALID_FIELD",
            Self::EBusy => "E_BUSY",
            Self::EBadRequest => "E_BAD_REQUEST",
            Self::EInternal => "E_INTERNAL",
            Self::EEngineMissing => "E_ENGINE_MISSING",
        }
    }
}

/// `message` is a complete sentence written for the user, as in the TS backend.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    /// For `EEngineMissing`: which engine to offer to install.
    pub engine: Option<String>,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), engine: None }
    }
    pub fn engine_missing(engine: &str) -> Self {
        Self {
            code: ErrorCode::EEngineMissing,
            message: format!("This conversion needs {engine}, which is not installed yet."),
            engine: Some(engine.to_string()),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::new(ErrorCode::EInternal, format!("File operation failed: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

//! Port of packages/ai/src/utils/models-error.ts

use std::fmt;

use pi_js::Error;
use serde::{Deserialize, Serialize};

use crate::utils::diagnostics::format_thrown_value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelsErrorCode {
    ModelSource,
    ModelValidation,
    Provider,
    Stream,
    Auth,
    #[serde(rename = "oauth")]
    OAuth,
}

impl ModelsErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelSource => "model_source",
            Self::ModelValidation => "model_validation",
            Self::Provider => "provider",
            Self::Stream => "stream",
            Self::Auth => "auth",
            Self::OAuth => "oauth",
        }
    }
}

impl fmt::Display for ModelsErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ModelsError {
    pub code: ModelsErrorCode,
    pub message: String,
    #[source]
    pub cause: Option<Error>,
}

impl ModelsError {
    pub fn new(code: ModelsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            cause: None,
        }
    }

    pub fn with_cause(code: ModelsErrorCode, message: impl Into<String>, cause: Error) -> Self {
        let message = with_cause_detail(message.into(), format_thrown_value(&cause));
        Self {
            code,
            message,
            cause: Some(cause),
        }
    }

    pub fn into_js_error(self) -> Error {
        Error::typed("ModelsError", self)
    }

    pub fn downcast(err: &Error) -> Option<&Self> {
        err.downcast_ref::<Self>()
    }
}

impl From<ModelsError> for Error {
    fn from(value: ModelsError) -> Self {
        value.into_js_error()
    }
}

fn with_cause_detail(message: String, detail: String) -> String {
    let trimmed = detail.trim();
    if trimmed.is_empty() || message.contains(trimmed) {
        return message;
    }
    format!("{message}: {trimmed}")
}

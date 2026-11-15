//! Port of packages/chord/src/services/errors.ts

#![allow(dead_code, unused_variables)]

use serde::{Deserialize, Serialize};

/// Stable wire and `instanceof` codes for [`RemoteServiceError`].
pub const REMOTE_SERVICE_ERROR_CODES: [&str; 8] = [
    "service_not_allowed",
    "service_not_found",
    "service_mode_mismatch",
    "service_member_not_found",
    "service_member_mismatch",
    "service_instance_not_found",
    "service_stale_instance",
    "service_invalid_value",
];

/// One code from [`REMOTE_SERVICE_ERROR_CODES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RemoteServiceErrorCode {
    #[serde(rename = "service_not_allowed")]
    ServiceNotAllowed,
    #[serde(rename = "service_not_found")]
    ServiceNotFound,
    #[serde(rename = "service_mode_mismatch")]
    ServiceModeMismatch,
    #[serde(rename = "service_member_not_found")]
    ServiceMemberNotFound,
    #[serde(rename = "service_member_mismatch")]
    ServiceMemberMismatch,
    #[serde(rename = "service_instance_not_found")]
    ServiceInstanceNotFound,
    #[serde(rename = "service_stale_instance")]
    ServiceStaleInstance,
    #[serde(rename = "service_invalid_value")]
    ServiceInvalidValue,
}

impl RemoteServiceErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ServiceNotAllowed => "service_not_allowed",
            Self::ServiceNotFound => "service_not_found",
            Self::ServiceModeMismatch => "service_mode_mismatch",
            Self::ServiceMemberNotFound => "service_member_not_found",
            Self::ServiceMemberMismatch => "service_member_mismatch",
            Self::ServiceInstanceNotFound => "service_instance_not_found",
            Self::ServiceStaleInstance => "service_stale_instance",
            Self::ServiceInvalidValue => "service_invalid_value",
        }
    }
}

pub fn is_remote_service_error_code(value: &serde_json::Value) -> bool {
    matches!(value.as_str(), Some(s) if REMOTE_SERVICE_ERROR_CODES.contains(&s))
}

/// Collection of errors thrown synchronously or during cleanup (`AggregateError`).
#[derive(Debug)]
pub struct AggregateError {
    pub message: String,
    pub errors: Vec<pi_js::Error>,
}

impl std::fmt::Display for AggregateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AggregateError {}

/// Throws one collected error or an `AggregateError` when multiple errors occurred.
pub(crate) fn throw_collected_errors(errors: &[pi_js::Error], message: &str) -> pi_js::Result<()> {
    if errors.is_empty() {
        return Ok(());
    }
    if errors.len() == 1 {
        return Err(clone_error(&errors[0]));
    }
    Err(pi_js::Error::typed(
        "AggregateError",
        AggregateError {
            message: message.to_string(),
            errors: errors.iter().map(clone_error).collect(),
        },
    ))
}

/// Clones a [`pi_js::Error`], preserving `RemoteServiceError`, `AggregateError`, `Abort`, `Node`, and `Js` payloads.
pub(crate) fn clone_error(error: &pi_js::Error) -> pi_js::Error {
    match error {
        pi_js::Error::Js(js) => pi_js::Error::Js(pi_js::JsError {
            name: js.name.clone(),
            message: js.message.clone(),
            code: js.code.clone(),
            cause: js.cause.as_deref().map(|cause| Box::new(clone_error(cause))),
        }),
        pi_js::Error::Abort(reason) => pi_js::Error::Abort(reason.clone()),
        pi_js::Error::Node(node) => pi_js::Error::Node(node.clone()),
        pi_js::Error::Exit(code) => pi_js::Error::Exit(*code),
        pi_js::Error::Typed(name, _) => {
            if let Some(remote) = error.downcast_ref::<RemoteServiceError>() {
                return pi_js::Error::typed("RemoteServiceError", remote.clone());
            }
            if let Some(agg) = error.downcast_ref::<AggregateError>() {
                return pi_js::Error::typed(
                    "AggregateError",
                    AggregateError {
                        message: agg.message.clone(),
                        errors: agg.errors.iter().map(clone_error).collect(),
                    },
                );
            }
            pi_js::Error::js(name, error.message())
        }
    }
}

/// Failure of a remote service allowlist, shape, or instance generation check.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RemoteServiceError {
    pub code: RemoteServiceErrorCode,
    pub message: String,
}

impl RemoteServiceError {
    pub fn new(code: RemoteServiceErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn into_js_error(self) -> pi_js::Error {
        pi_js::Error::typed("RemoteServiceError", self)
    }
}

impl From<RemoteServiceError> for pi_js::Error {
    fn from(value: RemoteServiceError) -> Self {
        value.into_js_error()
    }
}

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

pub fn is_remote_service_error_code(value: &serde_json::Value) -> bool {
    todo!("port: is_remote_service_error_code")
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
}

//! Port of packages/server/src/errors.ts

use std::fmt;
use std::sync::Arc;

use pi_chord::RemoteServiceErrorCode;
use serde::{Deserialize, Serialize};

pub const INTERNAL_SERVER_ERROR_MESSAGE: &str = "Internal server error";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerOperationErrorCode {
    ServiceNotAllowed,
    ServiceNotFound,
    ServiceModeMismatch,
    ServiceMemberNotFound,
    ServiceMemberMismatch,
    ServiceInstanceNotFound,
    ServiceStaleInstance,
    ServiceInvalidValue,
    WrongServer,
    SessionNotFound,
    SessionAmbiguous,
    SessionNotAttached,
    ServerDraining,
}

impl ServerOperationErrorCode {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ServiceNotAllowed => "service_not_allowed",
            Self::ServiceNotFound => "service_not_found",
            Self::ServiceModeMismatch => "service_mode_mismatch",
            Self::ServiceMemberNotFound => "service_member_not_found",
            Self::ServiceMemberMismatch => "service_member_mismatch",
            Self::ServiceInstanceNotFound => "service_instance_not_found",
            Self::ServiceStaleInstance => "service_stale_instance",
            Self::ServiceInvalidValue => "service_invalid_value",
            Self::WrongServer => "wrong_server",
            Self::SessionNotFound => "session_not_found",
            Self::SessionAmbiguous => "session_ambiguous",
            Self::SessionNotAttached => "session_not_attached",
            Self::ServerDraining => "server_draining",
        }
    }
}

impl fmt::Display for ServerOperationErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<RemoteServiceErrorCode> for ServerOperationErrorCode {
    fn from(code: RemoteServiceErrorCode) -> Self {
        match code {
            RemoteServiceErrorCode::ServiceNotAllowed => Self::ServiceNotAllowed,
            RemoteServiceErrorCode::ServiceNotFound => Self::ServiceNotFound,
            RemoteServiceErrorCode::ServiceModeMismatch => Self::ServiceModeMismatch,
            RemoteServiceErrorCode::ServiceMemberNotFound => Self::ServiceMemberNotFound,
            RemoteServiceErrorCode::ServiceMemberMismatch => Self::ServiceMemberMismatch,
            RemoteServiceErrorCode::ServiceInstanceNotFound => Self::ServiceInstanceNotFound,
            RemoteServiceErrorCode::ServiceStaleInstance => Self::ServiceStaleInstance,
            RemoteServiceErrorCode::ServiceInvalidValue => Self::ServiceInvalidValue,
        }
    }
}

/// A host or lifecycle error that can safely cross the protocol boundary.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ServerError {
    pub code: ServerOperationErrorCode,
    pub message: String,
}

impl ServerError {
    pub fn new(code: ServerOperationErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("ServerError", self)
    }

    /// Extracts `(code, message)` when `error` is [`ServerError`] or any of its subclasses.
    pub fn from_js_error(error: &pi_js::Error) -> Option<(ServerOperationErrorCode, &str)> {
        if let Some(e) = error.downcast_ref::<ServerError>() {
            return Some((e.code, &e.message));
        }
        if let Some(e) = error.downcast_ref::<WrongServerError>() {
            return Some((e.code, &e.message));
        }
        if let Some(e) = error.downcast_ref::<SessionNotFoundError>() {
            return Some((e.code, &e.message));
        }
        if let Some(e) = error.downcast_ref::<SessionAmbiguousError>() {
            return Some((e.code, &e.message));
        }
        if let Some(e) = error.downcast_ref::<SessionNotAttachedError>() {
            return Some((e.code, &e.message));
        }
        if let Some(e) = error.downcast_ref::<ServerDrainingError>() {
            return Some((e.code, &e.message));
        }
        None
    }
}

impl From<ServerError> for pi_js::Error {
    fn from(value: ServerError) -> Self {
        value.into_error()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WrongServerError {
    pub code: ServerOperationErrorCode,
    pub message: String,
}

impl WrongServerError {
    pub fn new() -> Self {
        Self {
            code: ServerOperationErrorCode::WrongServer,
            message: "Request was addressed to another server".to_string(),
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("WrongServerError", self)
    }
}

impl Default for WrongServerError {
    fn default() -> Self {
        Self::new()
    }
}

impl From<WrongServerError> for pi_js::Error {
    fn from(value: WrongServerError) -> Self {
        value.into_error()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SessionNotFoundError {
    pub code: ServerOperationErrorCode,
    pub message: String,
}

impl SessionNotFoundError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: ServerOperationErrorCode::SessionNotFound,
            message: message.into(),
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("SessionNotFoundError", self)
    }
}

impl Default for SessionNotFoundError {
    fn default() -> Self {
        Self::new("Session was not found")
    }
}

impl From<SessionNotFoundError> for pi_js::Error {
    fn from(value: SessionNotFoundError) -> Self {
        value.into_error()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SessionAmbiguousError {
    pub code: ServerOperationErrorCode,
    pub message: String,
}

impl SessionAmbiguousError {
    pub fn new() -> Self {
        Self {
            code: ServerOperationErrorCode::SessionAmbiguous,
            message: "Session ID matches more than one session".to_string(),
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("SessionAmbiguousError", self)
    }
}

impl Default for SessionAmbiguousError {
    fn default() -> Self {
        Self::new()
    }
}

impl From<SessionAmbiguousError> for pi_js::Error {
    fn from(value: SessionAmbiguousError) -> Self {
        value.into_error()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SessionNotAttachedError {
    pub code: ServerOperationErrorCode,
    pub message: String,
}

impl SessionNotAttachedError {
    pub fn new() -> Self {
        Self {
            code: ServerOperationErrorCode::SessionNotAttached,
            message: "Session is not attached to this client".to_string(),
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("SessionNotAttachedError", self)
    }
}

impl Default for SessionNotAttachedError {
    fn default() -> Self {
        Self::new()
    }
}

impl From<SessionNotAttachedError> for pi_js::Error {
    fn from(value: SessionNotAttachedError) -> Self {
        value.into_error()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ServerDrainingError {
    pub code: ServerOperationErrorCode,
    pub message: String,
}

impl ServerDrainingError {
    pub fn new() -> Self {
        Self {
            code: ServerOperationErrorCode::ServerDraining,
            message: "Server is draining".to_string(),
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("ServerDrainingError", self)
    }
}

impl Default for ServerDrainingError {
    fn default() -> Self {
        Self::new()
    }
}

impl From<ServerDrainingError> for pi_js::Error {
    fn from(value: ServerDrainingError) -> Self {
        value.into_error()
    }
}

/// Rust counterpart of JS `AggregateError`.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AggregateError {
    pub message: String,
    pub errors: Vec<pi_js::Error>,
}

impl AggregateError {
    pub fn new(errors: Vec<pi_js::Error>, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            errors,
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("AggregateError", self)
    }
}

impl From<AggregateError> for pi_js::Error {
    fn from(value: AggregateError) -> Self {
        value.into_error()
    }
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{inner}")]
struct ClonedTypedError {
    inner: Arc<pi_js::Error>,
}

/// Best-effort structural clone of [`pi_js::Error`] for broadcasting shutdown results across
/// `close()` and `closed()` observers.
pub(crate) fn clone_js_error(error: &pi_js::Error) -> pi_js::Error {
    match error {
        pi_js::Error::Js(j) => pi_js::Error::Js(pi_js::JsError {
            name: j.name.clone(),
            message: j.message.clone(),
            code: j.code.clone(),
            cause: j.cause.as_deref().map(|cause| Box::new(clone_js_error(cause))),
        }),
        pi_js::Error::Abort(reason) => pi_js::Error::Abort(reason.clone()),
        pi_js::Error::Node(node) => pi_js::Error::Node(node.clone()),
        pi_js::Error::Exit(code) => pi_js::Error::Exit(*code),
        pi_js::Error::Typed(name, _) => {
            if let Some(e) = error.downcast_ref::<ServerError>() {
                return e.clone().into();
            }
            if let Some(e) = error.downcast_ref::<WrongServerError>() {
                return e.clone().into();
            }
            if let Some(e) = error.downcast_ref::<SessionNotFoundError>() {
                return e.clone().into();
            }
            if let Some(e) = error.downcast_ref::<SessionAmbiguousError>() {
                return e.clone().into();
            }
            if let Some(e) = error.downcast_ref::<SessionNotAttachedError>() {
                return e.clone().into();
            }
            if let Some(e) = error.downcast_ref::<ServerDrainingError>() {
                return e.clone().into();
            }
            if let Some(e) = error.downcast_ref::<AggregateError>() {
                return AggregateError::new(e.errors.iter().map(clone_js_error).collect(), e.message.clone()).into();
            }
            if let Some(e) = error.downcast_ref::<pi_protocol::ProtocolValidationError>() {
                return pi_js::Error::typed("ProtocolValidationError", e.clone());
            }
            if let Some(e) = error.downcast_ref::<pi_chord::RemoteServiceError>() {
                return pi_js::Error::typed("RemoteServiceError", e.clone());
            }
            pi_js::Error::js(name, error.message())
        }
    }
}

pub(crate) fn shared_js_error(error: Arc<pi_js::Error>) -> pi_js::Error {
    match error.as_ref() {
        pi_js::Error::Typed(name, _) => {
            let cloned = clone_js_error(&error);
            if matches!(cloned, pi_js::Error::Js(_)) {
                pi_js::Error::typed(name, ClonedTypedError { inner: error })
            } else {
                cloned
            }
        }
        other => clone_js_error(other),
    }
}

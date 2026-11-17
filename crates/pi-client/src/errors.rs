//! Port of packages/client/src/errors.ts

use std::sync::Arc;

use pi_protocol::{ProtocolError, ProtocolErrorCode};

/// Typed server RPC error carrying a bounded protocol error code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ServerError {
    pub message: String,
    pub code: ProtocolErrorCode,
}

impl ServerError {
    pub fn new(error: ProtocolError) -> Self {
        Self {
            message: error.message,
            code: error.code,
        }
    }

    pub fn into_pi_error(self) -> pi_js::Error {
        pi_js::Error::typed("ServerError", self)
    }
}

impl From<ServerError> for pi_js::Error {
    fn from(error: ServerError) -> Self {
        error.into_pi_error()
    }
}

/// Error reported when the client transport is disconnected or fails during an operation.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct DisconnectedError {
    pub message: String,
    #[source]
    pub cause: Option<Arc<pi_js::Error>>,
}

impl Clone for DisconnectedError {
    fn clone(&self) -> Self {
        Self {
            message: self.message.clone(),
            cause: self.cause.as_ref().map(Arc::clone),
        }
    }
}

impl DisconnectedError {
    pub fn new(message: impl Into<String>, cause: Option<pi_js::Error>) -> Self {
        Self {
            message: message.into(),
            cause: cause.map(Arc::new),
        }
    }

    pub fn with_shared_cause(message: impl Into<String>, cause: Option<Arc<pi_js::Error>>) -> Self {
        Self {
            message: message.into(),
            cause,
        }
    }

    pub fn disconnected() -> Self {
        Self::new("Client is disconnected", None)
    }

    pub fn cause(&self) -> Option<&pi_js::Error> {
        self.cause.as_deref()
    }

    pub fn into_pi_error(self) -> pi_js::Error {
        pi_js::Error::typed("DisconnectedError", self)
    }
}

impl Default for DisconnectedError {
    fn default() -> Self {
        Self::disconnected()
    }
}

impl From<DisconnectedError> for pi_js::Error {
    fn from(error: DisconnectedError) -> Self {
        error.into_pi_error()
    }
}

/// Error reported when operations are attempted after `Client::dispose`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, thiserror::Error)]
#[error("Client is disposed")]
pub struct ClientDisposedError;

impl ClientDisposedError {
    pub fn into_pi_error(self) -> pi_js::Error {
        pi_js::Error::typed("ClientDisposedError", self)
    }
}

impl From<ClientDisposedError> for pi_js::Error {
    fn from(error: ClientDisposedError) -> Self {
        error.into_pi_error()
    }
}

/// Converts any `pi_js::Error` into a `DisconnectedError`, preserving an existing `DisconnectedError`.
pub fn to_disconnected_error(error: pi_js::Error) -> pi_js::Error {
    if error.downcast_ref::<DisconnectedError>().is_some() {
        return error;
    }
    let message = error.message();
    DisconnectedError::new(message, Some(error)).into_pi_error()
}

/// Clones a `pi_js::Error`, preserving typed error variants known to the client protocol stack.
pub(crate) fn clone_pi_error(error: &pi_js::Error) -> pi_js::Error {
    match error {
        pi_js::Error::Js(js) => pi_js::Error::Js(pi_js::JsError {
            name: js.name.clone(),
            message: js.message.clone(),
            code: js.code.clone(),
            cause: js.cause.as_deref().map(|inner| Box::new(clone_pi_error(inner))),
        }),
        pi_js::Error::Abort(reason) => pi_js::Error::Abort(reason.clone()),
        pi_js::Error::Node(node) => pi_js::Error::Node(node.clone()),
        pi_js::Error::Exit(code) => pi_js::Error::Exit(*code),
        pi_js::Error::Typed(name, inner) => {
            if let Some(err) = inner.downcast_ref::<ServerError>() {
                pi_js::Error::typed(name, err.clone())
            } else if let Some(err) = inner.downcast_ref::<DisconnectedError>() {
                pi_js::Error::typed(name, err.clone())
            } else if inner.downcast_ref::<ClientDisposedError>().is_some() {
                pi_js::Error::typed(name, ClientDisposedError)
            } else if let Some(err) = inner.downcast_ref::<pi_protocol::ProtocolValidationError>() {
                pi_js::Error::typed(name, err.clone())
            } else if let Some(err) = inner.downcast_ref::<pi_protocol::FrameError>() {
                pi_js::Error::typed(name, err.clone())
            } else if let Some(err) = inner.downcast_ref::<pi_protocol::CborError>() {
                pi_js::Error::typed(name, err.clone())
            } else if let Some(err) = inner.downcast_ref::<pi_chord::RemoteServiceError>() {
                pi_js::Error::typed(name, err.clone())
            } else {
                pi_js::Error::js(name, inner.to_string())
            }
        }
    }
}

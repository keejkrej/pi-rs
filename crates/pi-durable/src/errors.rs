//! Port of packages/durable/src/errors.ts

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

use crate::types::ConversationId;

/// A transaction read a table after its first table write. Read every required row before writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadAfterWrite {
    pub method: String,
    pub message: String,
}

impl ReadAfterWrite {
    pub fn new(method: impl Into<String>) -> Self {
        let method = method.into();
        let message = format!("Tx.{method}() cannot read tables after the first table write");
        Self { method, message }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("ReadAfterWrite", self)
    }
}

impl fmt::Display for ReadAfterWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for ReadAfterWrite {}

impl From<ReadAfterWrite> for pi_js::Error {
    fn from(err: ReadAfterWrite) -> Self {
        err.into_error()
    }
}

/// Storage rejected a batch before any durable effect; the owning Session may continue safely.
#[derive(Clone, Debug)]
pub struct StorageRejected {
    pub message: String,
    pub cause: Option<Arc<pi_js::Error>>,
}

impl StorageRejected {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cause: None,
        }
    }

    pub fn with_cause(message: impl Into<String>, cause: pi_js::Error) -> Self {
        Self {
            message: message.into(),
            cause: Some(Arc::new(cause)),
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("StorageRejected", self)
    }
}

impl fmt::Display for StorageRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for StorageRejected {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.cause.as_deref().map(|err| err as &(dyn StdError + 'static))
    }
}

impl From<StorageRejected> for pi_js::Error {
    fn from(err: StorageRejected) -> Self {
        err.into_error()
    }
}

/// A submission reached a busy conversation and was not admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationBusy {
    pub conversation_id: ConversationId,
    pub message: String,
}

impl ConversationBusy {
    pub fn new(conversation_id: ConversationId) -> Self {
        let message = format!("Conversation {conversation_id} is busy");
        Self {
            conversation_id,
            message,
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("ConversationBusy", self)
    }
}

impl fmt::Display for ConversationBusy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for ConversationBusy {}

impl From<ConversationBusy> for pi_js::Error {
    fn from(err: ConversationBusy) -> Self {
        err.into_error()
    }
}

/// Returns `true` if `err` represents a [`ReadAfterWrite`] error.
pub fn is_read_after_write(err: &pi_js::Error) -> bool {
    err.downcast_ref::<ReadAfterWrite>().is_some() || err.name() == "ReadAfterWrite"
}

/// Returns `true` if `err` represents a [`StorageRejected`] error.
pub fn is_storage_rejected(err: &pi_js::Error) -> bool {
    err.downcast_ref::<StorageRejected>().is_some() || err.name() == "StorageRejected"
}

/// Returns `true` if `err` represents a [`ConversationBusy`] error.
pub fn is_conversation_busy(err: &pi_js::Error) -> bool {
    err.downcast_ref::<ConversationBusy>().is_some() || err.name() == "ConversationBusy"
}

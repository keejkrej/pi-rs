//! Port of packages/mcp/src/transports/transport.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;

use pi_js::abort::AbortSignal;
use pi_js::{Error, Unsubscribe};

use crate::protocol::jsonrpc::JsonRpcMessage;

pub const DEFAULT_MAX_MESSAGE_BYTES: i64 = 16 * 1024 * 1024;

pub type McpTransportMessageListener = Arc<dyn Fn(&JsonRpcMessage) + Send + Sync>;
pub type McpTransportErrorListener = Arc<dyn Fn(&Error) + Send + Sync>;
pub type McpTransportCloseListener = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug, Default)]
pub struct McpTransportSendOptions {
    /// Aborted once the sender no longer waits for a reply (the request was cancelled, timed out, answered,
    /// or the client closed). A transport must not dispatch the message after that, for example when
    /// retrying it after an authorization refresh. It does not undo a message already dispatched.
    pub signal: Option<AbortSignal>,
}

#[async_trait]
pub trait McpTransport: Send + Sync {
    async fn start(&self) -> pi_js::Result<()>;
    async fn send(&self, message: JsonRpcMessage) -> pi_js::Result<()>;
    // PORT: Additive default method so existing downstream `McpTransport` impls keep compiling.
    async fn send_with_options(&self, message: JsonRpcMessage, _options: McpTransportSendOptions) -> pi_js::Result<()> {
        self.send(message).await
    }
    async fn close(&self) -> pi_js::Result<()>;
    fn on_message(&self, listener: McpTransportMessageListener) -> Unsubscribe;
    fn on_error(&self, listener: McpTransportErrorListener) -> Unsubscribe;
    fn on_close(&self, listener: McpTransportCloseListener) -> Unsubscribe;
    /// Optional `setProtocolVersion`. The default is a missing method: calling it is a no-op.
    fn set_protocol_version(&self, version: &str) {
        let _ = version;
    }
}

struct TransportEventsState {
    next_id: u64,
    message_listeners: IndexMap<u64, McpTransportMessageListener>,
    error_listeners: IndexMap<u64, McpTransportErrorListener>,
    close_listeners: IndexMap<u64, McpTransportCloseListener>,
    close_emitted: bool,
}

/// Listener bookkeeping shared by transports. `emitClose` fires at most once per transport.
#[derive(Clone)]
pub struct TransportEvents {
    inner: Arc<Mutex<TransportEventsState>>,
}

impl TransportEvents {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TransportEventsState {
                // PORT: listener identity is an id; a JS `Set` of functions has no Rust equivalent.
                next_id: 0,
                message_listeners: IndexMap::new(),
                error_listeners: IndexMap::new(),
                close_listeners: IndexMap::new(),
                close_emitted: false,
            })),
        }
    }

    pub fn on_message(&self, listener: McpTransportMessageListener) -> Unsubscribe {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = state.next_id;
        state.next_id += 1;
        state.message_listeners.insert(id, listener);
        let inner = Arc::clone(&self.inner);
        Unsubscribe::new(move || {
            inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .message_listeners
                .shift_remove(&id);
        })
    }

    pub fn on_error(&self, listener: McpTransportErrorListener) -> Unsubscribe {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = state.next_id;
        state.next_id += 1;
        state.error_listeners.insert(id, listener);
        let inner = Arc::clone(&self.inner);
        Unsubscribe::new(move || {
            inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .error_listeners
                .shift_remove(&id);
        })
    }

    pub fn on_close(&self, listener: McpTransportCloseListener) -> Unsubscribe {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = state.next_id;
        state.next_id += 1;
        state.close_listeners.insert(id, listener);
        let inner = Arc::clone(&self.inner);
        Unsubscribe::new(move || {
            inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .close_listeners
                .shift_remove(&id);
        })
    }

    pub(crate) fn emit_message(&self, message: &JsonRpcMessage) {
        let listeners: Vec<McpTransportMessageListener> = {
            let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            state.message_listeners.values().cloned().collect()
        };
        for listener in listeners {
            listener(message);
        }
    }

    pub(crate) fn emit_error(&self, error: &Error) {
        let listeners: Vec<McpTransportErrorListener> = {
            let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            state.error_listeners.values().cloned().collect()
        };
        for listener in listeners {
            listener(error);
        }
    }

    pub(crate) fn emit_close(&self) {
        let listeners: Vec<McpTransportCloseListener> = {
            let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if state.close_emitted {
                return;
            }
            state.close_emitted = true;
            state.close_listeners.values().cloned().collect()
        };
        for listener in listeners {
            listener();
        }
    }
}

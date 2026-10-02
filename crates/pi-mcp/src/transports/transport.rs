//! Port of packages/mcp/src/transports/transport.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;

use pi_js::{Error, Unsubscribe};

use crate::protocol::jsonrpc::JsonRpcMessage;

pub const DEFAULT_MAX_MESSAGE_BYTES: i64 = 16 * 1024 * 1024;

pub type McpTransportMessageListener = Arc<dyn Fn(&JsonRpcMessage) + Send + Sync>;
pub type McpTransportErrorListener = Arc<dyn Fn(&Error) + Send + Sync>;
pub type McpTransportCloseListener = Arc<dyn Fn() + Send + Sync>;

#[async_trait]
pub trait McpTransport: Send + Sync {
    async fn start(&self) -> pi_js::Result<()>;
    async fn send(&self, message: JsonRpcMessage) -> pi_js::Result<()>;
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
        todo!("port: TransportEvents::on_message")
    }

    pub fn on_error(&self, listener: McpTransportErrorListener) -> Unsubscribe {
        todo!("port: TransportEvents::on_error")
    }

    pub fn on_close(&self, listener: McpTransportCloseListener) -> Unsubscribe {
        todo!("port: TransportEvents::on_close")
    }

    pub(crate) fn emit_message(&self, message: &JsonRpcMessage) {
        todo!("port: TransportEvents::emit_message")
    }

    pub(crate) fn emit_error(&self, error: &Error) {
        todo!("port: TransportEvents::emit_error")
    }

    pub(crate) fn emit_close(&self) {
        todo!("port: TransportEvents::emit_close")
    }
}

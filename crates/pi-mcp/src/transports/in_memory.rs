//! Port of packages/mcp/src/transports/in-memory.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use pi_js::{Error, Unsubscribe};

use crate::protocol::jsonrpc::JsonRpcMessage;
use crate::transports::transport::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener, McpTransportMessageListener, TransportEvents,
};

struct InMemoryTransportState {
    peer: Option<InMemoryTransport>,
    started: bool,
    closed: bool,
}

struct InMemoryTransportInner {
    events: TransportEvents,
    state: Mutex<InMemoryTransportState>,
}

#[derive(Clone)]
pub struct InMemoryTransport {
    inner: Arc<InMemoryTransportInner>,
}

impl InMemoryTransport {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(InMemoryTransportInner {
                events: TransportEvents::new(),
                state: Mutex::new(InMemoryTransportState {
                    peer: None,
                    started: false,
                    closed: false,
                }),
            }),
        }
    }

    pub fn connect_peer(&self, peer: &InMemoryTransport) {
        todo!("port: InMemoryTransport::connect_peer")
    }

    /// Exposed so tests can simulate transport-level failures.
    pub fn emit_error(&self, error: &Error) {
        self.inner.events.emit_error(error);
    }

    fn deliver(&self, message: JsonRpcMessage) {
        todo!("port: InMemoryTransport::deliver")
    }
}

#[async_trait]
impl McpTransport for InMemoryTransport {
    async fn start(&self) -> pi_js::Result<()> {
        todo!("port: InMemoryTransport::start")
    }

    async fn send(&self, message: JsonRpcMessage) -> pi_js::Result<()> {
        todo!("port: InMemoryTransport::send")
    }

    async fn close(&self) -> pi_js::Result<()> {
        todo!("port: InMemoryTransport::close")
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Unsubscribe {
        self.inner.events.on_message(listener)
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Unsubscribe {
        self.inner.events.on_error(listener)
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Unsubscribe {
        self.inner.events.on_close(listener)
    }
}

#[derive(Clone)]
pub struct InMemoryTransportPair {
    pub client: InMemoryTransport,
    pub server: InMemoryTransport,
}

pub fn create_in_memory_transport_pair() -> InMemoryTransportPair {
    todo!("port: create_in_memory_transport_pair")
}

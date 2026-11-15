//! Port of packages/mcp/src/transports/in-memory.ts

#![allow(dead_code, unused_variables)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::oneshot;

use pi_js::{Error, Unsubscribe};

use crate::protocol::jsonrpc::{JsonRpcMessage, McpConnectionClosedError};
use crate::transports::transport::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener, McpTransportMessageListener, TransportEvents,
};

struct InMemoryTransportState {
    peer: Option<InMemoryTransport>,
    started: bool,
    closed: bool,
    pending_deliveries: VecDeque<(JsonRpcMessage, oneshot::Sender<()>)>,
    draining: bool,
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
                    pending_deliveries: VecDeque::new(),
                    draining: false,
                }),
            }),
        }
    }

    pub fn connect_peer(&self, peer: &InMemoryTransport) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        assert!(state.peer.is_none(), "In-memory MCP transport already has a peer");
        state.peer = Some(peer.clone());
    }

    /// Exposed so tests can simulate transport-level failures.
    pub fn emit_error(&self, error: &Error) {
        self.inner.events.emit_error(error);
    }

    fn deliver(&self, message: JsonRpcMessage) {
        let closed = self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).closed;
        if closed {
            return;
        }
        self.inner.events.emit_message(&message);
    }

    fn enqueue_delivery(&self, message: JsonRpcMessage) -> Option<oneshot::Receiver<()>> {
        let (tx, rx) = oneshot::channel();
        let should_spawn = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                return None;
            }
            state.pending_deliveries.push_back((message, tx));
            if state.draining {
                false
            } else {
                state.draining = true;
                true
            }
        };
        if should_spawn {
            let this = self.clone();
            tokio::spawn(async move {
                this.drain_deliveries();
            });
        }
        Some(rx)
    }

    fn drain_deliveries(&self) {
        loop {
            let (next, ack) = {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.closed {
                    state.pending_deliveries.clear();
                    state.draining = false;
                    return;
                }
                match state.pending_deliveries.pop_front() {
                    Some(item) => item,
                    None => {
                        state.draining = false;
                        return;
                    }
                }
            };
            self.deliver(next);
            let _ = ack.send(());
        }
    }
}

#[async_trait]
impl McpTransport for InMemoryTransport {
    async fn start(&self) -> pi_js::Result<()> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err(Error::typed(
                "McpConnectionClosedError",
                McpConnectionClosedError::new(None),
            ));
        }
        state.started = true;
        Ok(())
    }

    async fn send(&self, message: JsonRpcMessage) -> pi_js::Result<()> {
        let peer = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.started || state.closed {
                return Err(Error::typed(
                    "McpConnectionClosedError",
                    McpConnectionClosedError::new(None),
                ));
            }
            state.peer.clone()
        };
        let Some(peer) = peer else {
            return Err(Error::typed(
                "McpConnectionClosedError",
                McpConnectionClosedError::new(Some("In-memory MCP peer is not connected")),
            ));
        };
        {
            let peer_state = peer.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if !peer_state.started || peer_state.closed {
                return Err(Error::typed(
                    "McpConnectionClosedError",
                    McpConnectionClosedError::new(Some("In-memory MCP peer is not connected")),
                ));
            }
        }
        if let Some(rx) = peer.enqueue_delivery(message) {
            let _ = rx.await;
        }
        Ok(())
    }

    async fn close(&self) -> pi_js::Result<()> {
        let peer = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            state.pending_deliveries.clear();
            state.peer.clone()
        };
        self.inner.events.emit_close();
        if let Some(peer) = peer {
            Box::pin(peer.close()).await?;
        }
        Ok(())
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

impl Default for InMemoryTransport {
    fn default() -> Self {
        Self::new()
    }
}

pub struct InMemoryTransportPair {
    pub client: InMemoryTransport,
    pub server: InMemoryTransport,
}

pub fn create_in_memory_transport_pair() -> InMemoryTransportPair {
    let client = InMemoryTransport::new();
    let server = InMemoryTransport::new();
    client.connect_peer(&server);
    server.connect_peer(&client);
    InMemoryTransportPair { client, server }
}

//! Port of packages/server/src/connection.ts

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::ServiceStateEncoder;
use pi_js::Result;
use pi_js::abort::AbortController;
use pi_js::time::Timeout;
use pi_protocol::{ClientMessage, ClientMessageDecoder, RpcTarget};
use tokio::sync::Notify;

use crate::types::RoutedServerServiceAttachment;

/// An established, authorized ordered byte connection.
#[async_trait]
pub trait ByteConnection: Send + Sync {
    fn closed(&self) -> bool;
    async fn send(&self, chunk: &[u8]) -> Result<()>;
    async fn close(&self, final_chunk: Option<&[u8]>) -> Result<()>;
}

pub trait ByteConnectionHandler: Send + Sync {
    fn on_data(&self, chunk: &[u8]);
    fn on_close(&self);
    fn on_error(&self, error: pi_js::Error);
}

pub type ByteConnectionAcceptor = Arc<dyn Fn(Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStage {
    AwaitingHello,
    Handshaking,
    Ready,
    Closing,
    Closed,
}

pub(crate) struct ActiveRequest {
    pub controller: AbortController,
    pub target: RpcTarget,
}

pub struct ConnectionState {
    pub(crate) connection: Arc<dyn ByteConnection>,
    pub(crate) decoder: ClientMessageDecoder,
    pub(crate) inner: Mutex<ConnectionMutableState>,
    pub(crate) close_notify: Arc<Notify>,
}

pub(crate) struct ConnectionMutableState {
    pub service_state_encoders: IndexMap<String, ServiceStateEncoder>,
    pub stage: ConnectionStage,
    pub disconnected: bool,
    pub handshake_timeout: Option<Timeout>,
    pub handshake_waiters: Vec<ClientMessage>,
    pub server_services: Option<Arc<dyn RoutedServerServiceAttachment>>,
    pub active_requests: IndexMap<String, ActiveRequest>,
}

impl ConnectionState {
    pub(crate) fn new(
        connection: Arc<dyn ByteConnection>,
        decoder: ClientMessageDecoder,
        handshake_timeout: Timeout,
    ) -> Self {
        Self {
            connection,
            decoder,
            inner: Mutex::new(ConnectionMutableState {
                service_state_encoders: IndexMap::new(),
                stage: ConnectionStage::AwaitingHello,
                disconnected: false,
                handshake_timeout: Some(handshake_timeout),
                handshake_waiters: Vec::new(),
                server_services: None,
                active_requests: IndexMap::new(),
            }),
            close_notify: Arc::new(Notify::new()),
        }
    }

    pub fn stage(&self) -> ConnectionStage {
        self.inner.lock().unwrap().stage
    }

    pub fn disconnected(&self) -> bool {
        self.inner.lock().unwrap().disconnected
    }
}

pub fn is_terminal_connection(state: &ConnectionState) -> bool {
    let guard = state.inner.lock().unwrap();
    guard.disconnected || guard.stage == ConnectionStage::Closing || guard.stage == ConnectionStage::Closed
}

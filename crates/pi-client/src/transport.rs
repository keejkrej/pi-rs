//! Port of packages/client/src/transport.ts

use std::sync::Arc;

/// Reliable, ordered byte transport between client and server.
pub trait ByteTransport: Send + Sync {
    /// Sends one byte chunk. Calls must be delivered in invocation order.
    fn send(&self, chunk: Vec<u8>) -> pi_js::BoxFuture<pi_js::Result<()>>;
    /// Closes the transport. Implementations must make repeated calls harmless.
    fn close(&self);
}

/// Callback handlers provided by `Connection` to a `ByteTransportFactory`.
#[derive(Clone)]
pub struct ByteTransportHandlers {
    on_data: Arc<dyn Fn(&[u8]) + Send + Sync>,
    on_close: Arc<dyn Fn() + Send + Sync>,
    on_error: Arc<dyn Fn(pi_js::Error) + Send + Sync>,
}

impl ByteTransportHandlers {
    pub fn new(
        on_data: impl Fn(&[u8]) + Send + Sync + 'static,
        on_close: impl Fn() + Send + Sync + 'static,
        on_error: impl Fn(pi_js::Error) + Send + Sync + 'static,
    ) -> Self {
        Self {
            on_data: Arc::new(on_data),
            on_close: Arc::new(on_close),
            on_error: Arc::new(on_error),
        }
    }

    /// Delivers an arbitrary inbound byte chunk.
    pub fn on_data(&self, chunk: &[u8]) {
        (self.on_data)(chunk);
    }

    /// Reports an orderly terminal close.
    pub fn on_close(&self) {
        (self.on_close)();
    }

    /// Reports a terminal transport failure.
    pub fn on_error(&self, error: pi_js::Error) {
        (self.on_error)(error);
    }
}

/// Creates a fresh connected, authenticated transport. Exactly one terminal handler is expected.
pub type ByteTransportFactory =
    Arc<dyn Fn(ByteTransportHandlers) -> pi_js::BoxFuture<pi_js::Result<Arc<dyn ByteTransport>>> + Send + Sync>;

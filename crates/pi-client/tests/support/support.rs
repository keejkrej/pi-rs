//! Port of packages/client/test/support.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pi_client::{ByteTransport, ByteTransportHandlers};
use pi_protocol::{
    ClientMessage, ClientMessageDecoder, PROTOCOL_VERSION, ServerHello, ServerMessage, encode_server_message,
};
use tokio::sync::oneshot;

struct MessageWaiter {
    count: usize,
    tx: oneshot::Sender<()>,
}

struct MemoryByteServerState {
    messages: Vec<ClientMessage>,
    client_close_count: usize,
    handlers: Option<ByteTransportHandlers>,
    decoder: ClientMessageDecoder,
    waiters: Vec<MessageWaiter>,
    active_conn_id: u64,
}

#[derive(Clone)]
pub struct MemoryByteServer {
    server_id: String,
    state: Arc<Mutex<MemoryByteServerState>>,
}

impl MemoryByteServer {
    pub fn new(server_id: Option<&str>) -> Self {
        Self {
            server_id: server_id.unwrap_or("00000000-0000-4000-8000-000000000001").to_string(),
            state: Arc::new(Mutex::new(MemoryByteServerState {
                messages: Vec::new(),
                client_close_count: 0,
                handlers: None,
                decoder: ClientMessageDecoder::new(None),
                waiters: Vec::new(),
                active_conn_id: 0,
            })),
        }
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub fn messages(&self) -> Vec<ClientMessage> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .messages
            .clone()
    }

    pub fn client_close_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .client_close_count
    }

    pub fn connect(&self, handlers: ByteTransportHandlers) -> Arc<dyn ByteTransport> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.active_conn_id += 1;
        let conn_id = st.active_conn_id;
        st.handlers = Some(handlers);
        st.decoder = ClientMessageDecoder::new(None);
        drop(st);

        Arc::new(MemoryTransport {
            server: self.clone(),
            conn_id,
            closed: AtomicBool::new(false),
        })
    }

    pub async fn wait_for_messages(&self, count: usize) {
        let rx = {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.messages.len() >= count {
                return;
            }
            let (tx, rx) = oneshot::channel();
            st.waiters.push(MessageWaiter { count, tx });
            rx
        };
        let _ = rx.await;
    }

    pub fn send(&self, message: ServerMessage) -> pi_js::Result<()> {
        let handlers = {
            let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.handlers
                .clone()
                .ok_or_else(|| pi_js::Error::msg("No client connection"))?
        };
        let bytes = encode_server_message(&message, None)?;
        handlers.on_data(&bytes);
        Ok(())
    }

    pub fn send_raw(&self, chunk: &[u8]) -> pi_js::Result<()> {
        let handlers = {
            let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.handlers
                .clone()
                .ok_or_else(|| pi_js::Error::msg("No client connection"))?
        };
        handlers.on_data(chunk);
        Ok(())
    }

    pub fn disconnect(&self) {
        let handlers = {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.handlers.take()
        };
        if let Some(h) = handlers {
            h.on_close();
        }
    }

    pub fn error(&self, error: pi_js::Error) {
        let handlers = {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.handlers.take()
        };
        if let Some(h) = handlers {
            h.on_error(error);
        }
    }
}

struct MemoryTransport {
    server: MemoryByteServer,
    conn_id: u64,
    closed: AtomicBool,
}

impl ByteTransport for MemoryTransport {
    fn send(&self, chunk: Vec<u8>) -> pi_js::BoxFuture<pi_js::Result<()>> {
        let server = self.server.clone();
        Box::pin(async move {
            let decoder = {
                let st = server.state.lock().unwrap_or_else(PoisonError::into_inner);
                st.decoder.clone()
            };
            let decoded = decoder.push(&chunk)?;
            let mut send_hello = false;
            {
                let mut st = server.state.lock().unwrap_or_else(PoisonError::into_inner);
                for message in decoded {
                    if matches!(message, ClientMessage::Hello(_)) {
                        send_hello = true;
                    }
                    st.messages.push(message);
                }
                let count = st.messages.len();
                let mut remaining = Vec::with_capacity(st.waiters.len());
                for waiter in st.waiters.drain(..) {
                    if count >= waiter.count {
                        let _ = waiter.tx.send(());
                    } else {
                        remaining.push(waiter);
                    }
                }
                st.waiters = remaining;
            }
            if send_hello {
                let _ = server.send(ServerMessage::Hello(ServerHello {
                    version: PROTOCOL_VERSION,
                    server_id: server.server_id.clone(),
                }));
            }
            Ok(())
        })
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut st = self.server.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.client_close_count += 1;
        if st.active_conn_id == self.conn_id {
            st.handlers = None;
        }
    }
}

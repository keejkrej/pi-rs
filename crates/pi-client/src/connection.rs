//! Port of packages/client/src/connection.ts

use std::sync::{Arc, Mutex, PoisonError};

use pi_protocol::{
    ClientHello, ClientMessage, DEFAULT_MAX_FRAME_LENGTH, FrameDecoderOptions, PROTOCOL_VERSION,
    ProtocolValidationError, ServerHello, ServerMessage, ServerMessageDecoder, encode_client_message,
};
use tokio::sync::mpsc;

use crate::errors::{DisconnectedError, ServerError, clone_pi_error, to_disconnected_error};
use crate::promise::PromiseResolvers;
use crate::transport::{ByteTransport, ByteTransportFactory, ByteTransportHandlers};
use crate::types::{ConnectionState, ConnectionStateChange};

const MAX_UINT32: i64 = 0xffff_ffff;

/// Non-handshake server messages forwarded to `Client`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PostHandshakeMessage {
    Response(pi_protocol::ResponseEnvelope),
    ServiceUpdate(pi_protocol::ServiceEventEnvelope),
    Attachment(pi_protocol::AttachmentEnvelope),
}

pub(crate) struct ConnectionOptions {
    pub transport_factory: ByteTransportFactory,
    pub server_id: String,
    pub max_frame_length: Option<i64>,
    pub on_handshake: Arc<dyn Fn(&ServerHello) -> pi_js::Result<()> + Send + Sync>,
    pub on_message: Arc<dyn Fn(PostHandshakeMessage) + Send + Sync>,
    pub on_state_change: Arc<dyn Fn(ConnectionStateChange) + Send + Sync>,
}

enum Lifecycle {
    Disconnected,
    Connecting {
        id: u64,
        decoder: ServerMessageDecoder,
        transport: Option<Arc<dyn ByteTransport>>,
        send_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
        handshake: Option<Arc<PromiseResolvers<ServerHello>>>,
    },
    Connected {
        id: u64,
        decoder: ServerMessageDecoder,
        transport: Arc<dyn ByteTransport>,
        send_tx: mpsc::UnboundedSender<Vec<u8>>,
        handshake: Option<Arc<PromiseResolvers<ServerHello>>>,
    },
}

struct ConnectionStateFields {
    lifecycle: Lifecycle,
    sequence: u64,
}

struct ConnectionInner {
    options: ConnectionOptions,
    max_frame_length: i64,
    fields: Mutex<ConnectionStateFields>,
}

/// Manages a single transport connection lifecycle and server-hello handshake.
#[derive(Clone)]
pub struct Connection {
    inner: Arc<ConnectionInner>,
}

impl Connection {
    pub(crate) fn new(options: ConnectionOptions) -> pi_js::Result<Self> {
        let max_frame_length = options.max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
        if max_frame_length <= 0 || max_frame_length > MAX_UINT32 {
            return Err(pi_js::Error::js(
                "TypeError",
                format!("Client maxFrameLength must be an integer between 1 and {MAX_UINT32}"),
            ));
        }
        Ok(Self {
            inner: Arc::new(ConnectionInner {
                options,
                max_frame_length,
                fields: Mutex::new(ConnectionStateFields {
                    lifecycle: Lifecycle::Disconnected,
                    sequence: 0,
                }),
            }),
        })
    }

    pub fn state(&self) -> ConnectionState {
        let fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
        match &fields.lifecycle {
            Lifecycle::Disconnected => ConnectionState::Disconnected,
            Lifecycle::Connecting { .. } => ConnectionState::Connecting,
            Lifecycle::Connected { .. } => ConnectionState::Connected,
        }
    }

    pub fn max_frame_length(&self) -> i64 {
        self.inner.max_frame_length
    }

    pub fn connect(&self) -> pi_js::BoxFuture<pi_js::Result<ServerHello>> {
        let (id, handshake_future) = {
            let mut fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
            let current_state = match &fields.lifecycle {
                Lifecycle::Disconnected => ConnectionState::Disconnected,
                Lifecycle::Connecting { .. } => ConnectionState::Connecting,
                Lifecycle::Connected { .. } => ConnectionState::Connected,
            };
            if current_state != ConnectionState::Disconnected {
                let err = DisconnectedError::new(format!("Client is already {}", current_state.as_str()), None)
                    .into_pi_error();
                return Box::pin(async move { Err(err) });
            }
            fields.sequence += 1;
            let id = fields.sequence;
            let (resolvers, future) = PromiseResolvers::new();
            fields.lifecycle = Lifecycle::Connecting {
                id,
                decoder: ServerMessageDecoder::new(Some(FrameDecoderOptions {
                    max_frame_length: Some(self.inner.max_frame_length),
                })),
                transport: None,
                send_tx: None,
                handshake: Some(Arc::new(resolvers)),
            };
            (id, future)
        };

        (self.inner.options.on_state_change)(ConnectionStateChange {
            state: ConnectionState::Connecting,
            error: None,
        });

        let conn_for_data = self.clone();
        let conn_for_close = self.clone();
        let conn_for_error = self.clone();
        let handlers = ByteTransportHandlers::new(
            move |chunk| conn_for_data.handle_data(id, chunk),
            move || {
                if conn_for_close.is_current(id) {
                    conn_for_close.handle_close();
                }
            },
            move |error| {
                if conn_for_error.is_current(id) {
                    conn_for_error.fail_and_close(to_disconnected_error(error));
                }
            },
        );

        let conn_for_open = self.clone();
        tokio::spawn(async move {
            conn_for_open.open_transport(id, handlers).await;
        });

        Box::pin(handshake_future)
    }

    pub fn disconnect(&self, reason: impl Into<DisconnectReason>) {
        if self.state() == ConnectionState::Disconnected {
            return;
        }
        let error = match reason.into() {
            DisconnectReason::Message(msg) => DisconnectedError::new(msg, None).into_pi_error(),
            DisconnectReason::Error(err) => err,
        };
        self.fail_and_close(error);
    }

    pub fn fail(&self, error: pi_js::Error) {
        self.fail_and_close(error);
    }

    pub fn send(&self, frame: Vec<u8>) -> pi_js::Result<()> {
        let send_tx = {
            let fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
            match &fields.lifecycle {
                Lifecycle::Connected { send_tx, .. } => send_tx.clone(),
                _ => return Err(DisconnectedError::disconnected().into_pi_error()),
            }
        };
        if send_tx.send(frame).is_err() {
            self.fail_and_close(DisconnectedError::disconnected().into_pi_error());
        }
        Ok(())
    }

    async fn open_transport(&self, id: u64, handlers: ByteTransportHandlers) {
        let transport_res = (self.inner.options.transport_factory)(handlers).await;
        let transport = match transport_res {
            Ok(transport) => transport,
            Err(error) => {
                if self.is_current(id) {
                    self.fail_internal(to_disconnected_error(error));
                }
                return;
            }
        };

        let (send_tx, mut send_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        {
            let mut fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
            match &mut fields.lifecycle {
                Lifecycle::Connecting {
                    id: current_id,
                    transport: slot,
                    send_tx: tx_slot,
                    ..
                } if *current_id == id => {
                    *slot = Some(Arc::clone(&transport));
                    *tx_slot = Some(send_tx);
                }
                _ => {
                    drop(fields);
                    transport.close();
                    return;
                }
            }
        }

        // Spawn single ordered outbound writer task tied to this transport instance.
        let conn_writer = self.clone();
        let writer_transport = Arc::clone(&transport);
        tokio::spawn(async move {
            while let Some(frame) = send_rx.recv().await {
                if let Err(err) = writer_transport.send(frame).await {
                    let should_fail = {
                        let fields = conn_writer.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
                        match &fields.lifecycle {
                            Lifecycle::Connecting {
                                transport: Some(active),
                                ..
                            }
                            | Lifecycle::Connected { transport: active, .. } => Arc::ptr_eq(active, &writer_transport),
                            _ => false,
                        }
                    };
                    if should_fail {
                        conn_writer.fail_and_close(to_disconnected_error(err));
                    }
                    break;
                }
            }
        });

        let hello_frame = encode_client_message(
            &ClientMessage::Hello(ClientHello {
                version: PROTOCOL_VERSION,
            }),
            Some(FrameDecoderOptions {
                max_frame_length: Some(self.inner.max_frame_length),
            }),
        );
        match hello_frame {
            Ok(frame) => {
                if let Err(error) = transport.send(frame).await
                    && self.is_current(id)
                {
                    self.fail_and_close(to_disconnected_error(error));
                }
            }
            Err(error) => {
                if self.is_current(id) {
                    self.fail_and_close(to_disconnected_error(error));
                }
            }
        }
    }

    fn handle_data(&self, id: u64, chunk: &[u8]) {
        let decoder = {
            let fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
            match &fields.lifecycle {
                Lifecycle::Disconnected => return,
                Lifecycle::Connecting { id: cid, .. } | Lifecycle::Connected { id: cid, .. } if *cid != id => return,
                Lifecycle::Connecting { transport: None, .. } => {
                    drop(fields);
                    self.fail_and_close(pi_js::Error::typed(
                        "ProtocolValidationError",
                        ProtocolValidationError::new("Received server data before the client hello was sent"),
                    ));
                    return;
                }
                Lifecycle::Connecting { decoder, .. } | Lifecycle::Connected { decoder, .. } => decoder.clone(),
            }
        };

        let messages = match decoder.push(chunk) {
            Ok(messages) => messages,
            Err(error) => {
                self.fail_and_close(error);
                return;
            }
        };

        for message in messages {
            if !self.is_current(id) {
                return;
            }
            self.handle_message(id, message);
        }
    }

    fn handle_message(&self, id: u64, message: ServerMessage) {
        enum Phase {
            Connecting {
                decoder: ServerMessageDecoder,
                transport: Option<Arc<dyn ByteTransport>>,
                send_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
                handshake: Option<Arc<PromiseResolvers<ServerHello>>>,
            },
            Connected,
        }

        let phase = {
            let mut fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
            match &mut fields.lifecycle {
                Lifecycle::Connecting {
                    id: cid,
                    decoder,
                    transport,
                    send_tx,
                    handshake,
                } if *cid == id => Phase::Connecting {
                    decoder: decoder.clone(),
                    transport: transport.clone(),
                    send_tx: send_tx.clone(),
                    handshake: handshake.take(),
                },
                Lifecycle::Connected { id: cid, .. } if *cid == id => Phase::Connected,
                _ => return,
            }
        };

        match phase {
            Phase::Connecting {
                decoder,
                transport,
                send_tx,
                handshake,
            } => {
                // Re-attach handshake so `fail_and_close` rejects it if validation fails.
                let restore_handshake = |hs: Option<Arc<PromiseResolvers<ServerHello>>>| {
                    let mut fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Lifecycle::Connecting {
                        id: cid,
                        handshake: slot,
                        ..
                    } = &mut fields.lifecycle
                        && *cid == id
                    {
                        *slot = hs;
                    }
                };
                match message {
                    ServerMessage::HelloError(err_env) => {
                        restore_handshake(handshake);
                        self.fail_and_close(ServerError::new(err_env.error).into_pi_error());
                        return;
                    }
                    ServerMessage::Hello(hello) => {
                        if hello.server_id != self.inner.options.server_id {
                            restore_handshake(handshake);
                            let msg = format!(
                                "Connected server {} does not match {}",
                                pi_js::json::stringify(&hello.server_id),
                                pi_js::json::stringify(&self.inner.options.server_id)
                            );
                            self.fail_and_close(pi_js::Error::typed(
                                "ProtocolValidationError",
                                ProtocolValidationError::new(msg),
                            ));
                            return;
                        }
                        let (Some(transport), Some(send_tx)) = (transport, send_tx) else {
                            restore_handshake(handshake);
                            self.fail_and_close(pi_js::Error::typed(
                                "ProtocolValidationError",
                                ProtocolValidationError::new("Received server hello before the client hello was sent"),
                            ));
                            return;
                        };
                        {
                            let mut fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
                            if !matches!(&fields.lifecycle, Lifecycle::Connecting { id: cid, .. } if *cid == id) {
                                return;
                            }
                            fields.lifecycle = Lifecycle::Connected {
                                id,
                                decoder,
                                transport,
                                send_tx,
                                handshake: handshake.clone(),
                            };
                        }
                        if let Err(err) = (self.inner.options.on_handshake)(&hello) {
                            if self.is_current(id) {
                                self.fail_and_close(err);
                            }
                            return;
                        }
                        if !self.is_current(id) {
                            return;
                        }
                        (self.inner.options.on_state_change)(ConnectionStateChange {
                            state: ConnectionState::Connected,
                            error: None,
                        });
                        let settled_handshake = {
                            let mut fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
                            match &mut fields.lifecycle {
                                Lifecycle::Connected {
                                    id: cid, handshake: hs, ..
                                } if *cid == id => hs.take(),
                                _ => None,
                            }
                        };
                        if let Some(hs) = settled_handshake {
                            hs.resolve(hello);
                        }
                    }
                    _ => {
                        restore_handshake(handshake);
                        self.fail_and_close(pi_js::Error::typed(
                            "ProtocolValidationError",
                            ProtocolValidationError::new("Expected server hello as first message"),
                        ));
                    }
                }
            }
            Phase::Connected => {
                let forwarded = match message {
                    ServerMessage::Hello(_) | ServerMessage::HelloError(_) => {
                        self.fail_and_close(pi_js::Error::typed(
                            "ProtocolValidationError",
                            ProtocolValidationError::new("Unexpected handshake message"),
                        ));
                        return;
                    }
                    ServerMessage::Response(r) => PostHandshakeMessage::Response(r),
                    ServerMessage::ServiceUpdate(u) => PostHandshakeMessage::ServiceUpdate(u),
                    ServerMessage::Attachment(a) => PostHandshakeMessage::Attachment(a),
                };
                (self.inner.options.on_message)(forwarded);
            }
        }
    }

    fn handle_close(&self) {
        let decoder = {
            let fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
            match &fields.lifecycle {
                Lifecycle::Disconnected => return,
                Lifecycle::Connecting { decoder, .. } | Lifecycle::Connected { decoder, .. } => decoder.clone(),
            }
        };
        let error = match decoder.end() {
            Ok(()) => DisconnectedError::new("Byte transport closed", None).into_pi_error(),
            Err(decoder_error) => decoder_error,
        };
        self.fail_internal(error);
    }

    fn fail_and_close(&self, error: pi_js::Error) {
        let transport = {
            let fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
            match &fields.lifecycle {
                Lifecycle::Disconnected => None,
                Lifecycle::Connecting { transport, .. } => transport.clone(),
                Lifecycle::Connected { transport, .. } => Some(Arc::clone(transport)),
            }
        };
        self.fail_internal(error);
        if let Some(transport) = transport {
            transport.close();
        }
    }

    fn fail_internal(&self, error: pi_js::Error) {
        let handshake = {
            let mut fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
            match std::mem::replace(&mut fields.lifecycle, Lifecycle::Disconnected) {
                Lifecycle::Disconnected => return,
                Lifecycle::Connecting { handshake, .. } | Lifecycle::Connected { handshake, .. } => handshake,
            }
        };

        let shared_error = Arc::new(error);
        if let Some(hs) = handshake {
            hs.reject(clone_pi_error(&shared_error));
        }
        (self.inner.options.on_state_change)(ConnectionStateChange {
            state: ConnectionState::Disconnected,
            error: Some(shared_error),
        });
    }

    fn is_current(&self, id: u64) -> bool {
        let fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
        match &fields.lifecycle {
            Lifecycle::Disconnected => false,
            Lifecycle::Connecting { id: cid, .. } | Lifecycle::Connected { id: cid, .. } => *cid == id,
        }
    }
}

pub enum DisconnectReason {
    Message(String),
    Error(pi_js::Error),
}

impl From<&str> for DisconnectReason {
    fn from(s: &str) -> Self {
        Self::Message(s.to_string())
    }
}

impl From<String> for DisconnectReason {
    fn from(s: String) -> Self {
        Self::Message(s)
    }
}

impl From<pi_js::Error> for DisconnectReason {
    fn from(e: pi_js::Error) -> Self {
        Self::Error(e)
    }
}

//! Port of packages/coding-agent/src/experimental/radius-relay.ts

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use indexmap::IndexMap;
use pi_client::{ByteTransport, ByteTransportFactory, ByteTransportHandlers, Client, ConnectionState};
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::json::parse;
use pi_js::{BoxFuture, Unsubscribe};
use pi_protocol::{DEFAULT_MAX_FRAME_LENGTH, ServerId};
use pi_server::connection::{ByteConnection, ByteConnectionHandler};
use pi_server::{Server, SessionMetadata};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{AUTHORIZATION, HeaderValue, SEC_WEBSOCKET_PROTOCOL};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::experimental::radius_auth::{RadiusRelayAuthOptions, RadiusRelayAuthResolver};

pub const RADIUS_RELAY_HOST_SUBPROTOCOL: &str = "pi-session-relay.host.v1";
pub const RADIUS_RELAY_CLIENT_SUBPROTOCOL: &str = "pi-session-relay.client.v1";

const RELAY_DATA_HEADER_BYTES: usize = 18;
const RELAY_DATA_FRAME_VERSION: u8 = 1;
const RELAY_DATA_FRAME_TYPE: u8 = 1;
const MAX_PENDING_BYTES: usize = (DEFAULT_MAX_FRAME_LENGTH as usize) * 4;
const DRAIN_THRESHOLD_BYTES: usize = 1024 * 1024;
const HOST_RETRY_INITIAL_MS: u64 = 1_000;
const HOST_RETRY_MAX_MS: u64 = 30_000;
const MISSING_AUTH_RETRY_MS: u64 = 30_000;
const CLIENT_RETRY_INITIAL_MS: u64 = 1_000;
const CLIENT_RETRY_MAX_MS: u64 = 30_000;
const LOCAL_PROTOCOL_ERROR_CLOSE_CODE: u16 = 4000;
const LOCAL_TRANSPORT_ERROR_CLOSE_CODE: u16 = 4001;

pub const WS_STATE_CONNECTING: u16 = 0;
pub const WS_STATE_OPEN: u16 = 1;
pub const WS_STATE_CLOSING: u16 = 2;
pub const WS_STATE_CLOSED: u16 = 3;

static CONNECTION_ID_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
        .unwrap_or_else(|e| panic!("valid UUIDv4 regex: {e}"))
});

fn lock_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RadiusRelayMessage {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Clone, Default)]
pub struct RadiusRelayWebSocketHandlers {
    pub on_open: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_message: Option<Arc<dyn Fn(RadiusRelayMessage) + Send + Sync>>,
    pub on_close: Option<Arc<dyn Fn(u16, String) + Send + Sync>>,
    pub on_error: Option<Arc<dyn Fn(pi_js::Error) + Send + Sync>>,
}

#[derive(Clone, Debug)]
pub struct RadiusRelayWebSocketConnectOptions {
    pub url: String,
    pub protocol: String,
    pub authorization: String,
}

pub trait RadiusRelayWebSocket: Send + Sync {
    fn protocol(&self) -> String;
    fn ready_state(&self) -> u16;
    fn buffered_amount(&self) -> usize;
    fn send_text(&self, data: String) -> pi_js::Result<()>;
    fn send_binary(&self, data: Vec<u8>) -> pi_js::Result<()>;
    fn close(&self, code: Option<u16>, reason: Option<&str>) -> pi_js::Result<()>;
    fn subscribe(&self, handlers: RadiusRelayWebSocketHandlers) -> Unsubscribe;
}

pub type RadiusRelayWebSocketFactory =
    Arc<dyn Fn(RadiusRelayWebSocketConnectOptions) -> Arc<dyn RadiusRelayWebSocket> + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedRelayDataFrame {
    pub connection_id: String,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RadiusRelayHostStatus {
    NotAuthenticated,
    Connecting,
    Connected,
    Retrying { error: String },
}

pub trait RelayServerAcceptTarget: Send + Sync {
    fn accept_connection(&self, connection: Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler>;
}

impl<M: SessionMetadata> RelayServerAcceptTarget for Server<M> {
    fn accept_connection(&self, connection: Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> {
        self.accept(connection)
    }
}

pub struct RadiusRelayHostOptions {
    pub server_id: ServerId,
    pub server: Arc<dyn RelayServerAcceptTarget>,
    pub auth: Arc<RadiusRelayAuthResolver>,
    pub web_socket_factory: Option<RadiusRelayWebSocketFactory>,
    pub on_status: Option<Arc<dyn Fn(RadiusRelayHostStatus) + Send + Sync>>,
}

struct ActiveRelayConnection {
    connection: Arc<RelayServerByteConnection>,
    handler: Arc<dyn ByteConnectionHandler>,
}

struct HostInner {
    options: RadiusRelayHostOptions,
    abort_controller: AbortController,
    connections: Mutex<IndexMap<String, ActiveRelayConnection>>,
    socket: Mutex<Option<Arc<dyn RadiusRelayWebSocket>>>,
    writer: Mutex<Option<Arc<OrderedWebSocketWriter>>>,
    closed: AtomicBool,
}

/// Maintain the experimental server's multiplexed, authenticated Radius host connection.
#[derive(Clone)]
pub struct RadiusRelayHost {
    inner: Arc<HostInner>,
    loop_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl RadiusRelayHost {
    pub fn new(options: RadiusRelayHostOptions) -> Self {
        Self {
            inner: Arc::new(HostInner {
                options,
                abort_controller: AbortController::new(),
                connections: Mutex::new(IndexMap::new()),
                socket: Mutex::new(None),
                writer: Mutex::new(None),
                closed: AtomicBool::new(false),
            }),
            loop_task: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    pub fn start(&self) {
        let inner = Arc::clone(&self.inner);
        let loop_slot = Arc::clone(&self.loop_task);
        tokio::spawn(async move {
            let mut guard = loop_slot.lock().await;
            if guard.is_some() {
                return;
            }
            let inner_run = Arc::clone(&inner);
            *guard = Some(tokio::spawn(async move {
                run_host_loop(&inner_run).await;
            }));
        });
    }

    pub async fn close(&self) -> pi_js::Result<()> {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.inner.abort_controller.abort(None);
        if let Some(writer) = lock_mutex(&self.inner.writer).take() {
            writer.close();
        }
        if let Some(socket) = lock_mutex(&self.inner.socket).take() {
            let _ = socket.close(Some(1000), Some("Pi server stopped"));
        }
        drop_connections(&self.inner, None);
        if let Some(handle) = self.loop_task.lock().await.take() {
            let _ = handle.await;
        }
        Ok(())
    }
}

async fn run_host_loop(inner: &Arc<HostInner>) {
    let mut retry_ms = HOST_RETRY_INITIAL_MS;
    let signal = inner.abort_controller.signal();
    while !inner.closed.load(Ordering::Acquire) {
        let auth_res = inner
            .options
            .auth
            .resolve(RadiusRelayAuthOptions {
                required: false,
                signal: Some(signal.clone()),
            })
            .await;
        match auth_res {
            Ok(None) => {
                if let Some(ref on_status) = inner.options.on_status {
                    on_status(RadiusRelayHostStatus::NotAuthenticated);
                }
                if delay_ms(MISSING_AUTH_RETRY_MS, Some(&signal)).await.is_err() {
                    break;
                }
            }
            Ok(Some(auth)) => {
                if let Some(ref on_status) = inner.options.on_status {
                    on_status(RadiusRelayHostStatus::Connecting);
                }
                let connect_res = async {
                    let url = relay_websocket_url(&auth.gateway, &inner.options.server_id)?;
                    let factory = inner
                        .options
                        .web_socket_factory
                        .clone()
                        .unwrap_or_else(|| Arc::new(default_websocket_factory));
                    let socket = open_radius_relay_websocket(
                        &url,
                        RADIUS_RELAY_HOST_SUBPROTOCOL,
                        &auth.token,
                        factory,
                        Some(&signal),
                    )
                    .await?;
                    retry_ms = HOST_RETRY_INITIAL_MS;
                    if let Some(ref on_status) = inner.options.on_status {
                        on_status(RadiusRelayHostStatus::Connected);
                    }
                    serve_host_socket(inner, socket).await?;
                    if !inner.closed.load(Ordering::Acquire) {
                        return Err(pi_js::Error::msg("Radius relay host disconnected"));
                    }
                    Ok(())
                }
                .await;
                if let Err(err) = connect_res {
                    if inner.closed.load(Ordering::Acquire) || signal.aborted() {
                        break;
                    }
                    if let Some(ref on_status) = inner.options.on_status {
                        on_status(RadiusRelayHostStatus::Retrying {
                            error: err.message().to_string(),
                        });
                    }
                    if delay_ms(retry_ms, Some(&signal)).await.is_err() {
                        break;
                    }
                    retry_ms = (retry_ms * 2).min(HOST_RETRY_MAX_MS);
                }
            }
            Err(err) => {
                if inner.closed.load(Ordering::Acquire) || signal.aborted() {
                    break;
                }
                if let Some(ref on_status) = inner.options.on_status {
                    on_status(RadiusRelayHostStatus::Retrying {
                        error: err.message().to_string(),
                    });
                }
                if delay_ms(retry_ms, Some(&signal)).await.is_err() {
                    break;
                }
                retry_ms = (retry_ms * 2).min(HOST_RETRY_MAX_MS);
            }
        }
    }
}

async fn serve_host_socket(inner: &Arc<HostInner>, socket: Arc<dyn RadiusRelayWebSocket>) -> pi_js::Result<()> {
    let writer = Arc::new(OrderedWebSocketWriter::new(Arc::clone(&socket)));
    *lock_mutex(&inner.socket) = Some(Arc::clone(&socket));
    *lock_mutex(&inner.writer) = Some(Arc::clone(&writer));

    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<pi_js::Result<()>>();
    let done_tx = Arc::new(Mutex::new(Some(done_tx)));
    let terminal = Arc::new(AtomicBool::new(false));
    let unsub_slot: Arc<Mutex<Option<Unsubscribe>>> = Arc::new(Mutex::new(None));

    let finish = {
        let inner = Arc::clone(inner);
        let socket = Arc::clone(&socket);
        let writer = Arc::clone(&writer);
        let terminal = Arc::clone(&terminal);
        let done_tx = Arc::clone(&done_tx);
        let unsub_slot = Arc::clone(&unsub_slot);
        Arc::new(move |err: Option<pi_js::Error>| {
            if terminal.swap(true, Ordering::AcqRel) {
                return;
            }
            drop(lock_mutex(&unsub_slot).take());
            writer.close();
            {
                let mut sock_slot = lock_mutex(&inner.socket);
                if sock_slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, &socket)) {
                    *sock_slot = None;
                    *lock_mutex(&inner.writer) = None;
                }
            }
            drop_connections(&inner, err.as_ref());
            if let Some(tx) = lock_mutex(&done_tx).take() {
                match err {
                    Some(e) => {
                        let _ = tx.send(Err(e));
                    }
                    None => {
                        let _ = tx.send(Ok(()));
                    }
                }
            }
        })
    };

    let finish_msg = Arc::clone(&finish);
    let inner_msg = Arc::clone(inner);
    let socket_msg = Arc::clone(&socket);
    let finish_close = Arc::clone(&finish);
    let finish_err = Arc::clone(&finish);

    let unsub = socket.subscribe(RadiusRelayWebSocketHandlers {
        on_open: None,
        on_message: Some(Arc::new(move |msg| {
            if let Err(err) = handle_host_message(&inner_msg, msg) {
                close_websocket_quiet(
                    &socket_msg,
                    LOCAL_PROTOCOL_ERROR_CLOSE_CODE,
                    "Radius relay protocol error",
                );
                finish_msg(Some(err));
            }
        })),
        on_close: Some(Arc::new(move |code, reason| {
            if code == 1000 {
                finish_close(None);
            } else {
                let suffix = if reason.is_empty() {
                    String::new()
                } else {
                    format!(": {reason}")
                };
                finish_close(Some(pi_js::Error::msg(format!(
                    "Radius relay host closed ({code}{suffix})"
                ))));
            }
        })),
        on_error: Some(Arc::new(move |err| {
            finish_err(Some(err));
        })),
    });
    *lock_mutex(&unsub_slot) = Some(unsub);
    if terminal.load(Ordering::Acquire) {
        drop(lock_mutex(&unsub_slot).take());
    }

    done_rx
        .await
        .map_err(|_| pi_js::Error::msg("Radius relay host disconnected"))?
}

fn handle_host_message(inner: &Arc<HostInner>, message: RadiusRelayMessage) -> pi_js::Result<()> {
    match message {
        RadiusRelayMessage::Text(text) => {
            let control = parse_host_control_message(&text)?;
            match control {
                HostInputControlMessage::Ping => {
                    let inner_c = Arc::clone(inner);
                    tokio::spawn(async move {
                        if send_control(&inner_c, &HostOutputControlMessage::Pong { version: 1 })
                            .await
                            .is_err()
                            && let Some(socket) = lock_mutex(&inner_c.socket).clone()
                        {
                            close_websocket_quiet(
                                &socket,
                                LOCAL_TRANSPORT_ERROR_CLOSE_CODE,
                                "Radius relay send failed",
                            );
                        }
                    });
                }
                HostInputControlMessage::Pong => {}
                HostInputControlMessage::ConnectionOpen { connection_id } => {
                    open_connection(inner, &connection_id)?;
                }
                HostInputControlMessage::ConnectionClose { connection_id, .. } => {
                    remote_close_connection(inner, &connection_id);
                }
            }
            Ok(())
        }
        RadiusRelayMessage::Binary(bytes) => {
            let frame =
                parse_relay_data_frame(&bytes).ok_or_else(|| pi_js::Error::msg("Invalid Radius relay data frame"))?;
            let handler = lock_mutex(&inner.connections)
                .get(&frame.connection_id)
                .map(|c| Arc::clone(&c.handler));
            if let Some(handler) = handler {
                handler.on_data(&frame.payload);
            } else {
                let inner_c = Arc::clone(inner);
                let conn_id = frame.connection_id;
                tokio::spawn(async move {
                    let _ = send_control(
                        &inner_c,
                        &HostOutputControlMessage::ConnectionClose {
                            version: 1,
                            connection_id: conn_id,
                            code: Some(1000),
                        },
                    )
                    .await;
                });
            }
            Ok(())
        }
    }
}

fn open_connection(inner: &Arc<HostInner>, connection_id: &str) -> pi_js::Result<()> {
    if lock_mutex(&inner.connections).contains_key(connection_id) {
        return Err(pi_js::Error::msg("Radius relay reused a connection ID"));
    }
    let conn = Arc::new(RelayServerByteConnection {
        inner: Arc::clone(inner),
        connection_id: connection_id.to_string(),
        closed: AtomicBool::new(false),
    });
    let handler = inner
        .options
        .server
        .accept_connection(Arc::clone(&conn) as Arc<dyn ByteConnection>);
    if conn.closed.load(Ordering::Acquire) {
        let inner_c = Arc::clone(inner);
        let conn_id = connection_id.to_string();
        tokio::spawn(async move {
            let _ = send_control(
                &inner_c,
                &HostOutputControlMessage::ConnectionClose {
                    version: 1,
                    connection_id: conn_id,
                    code: Some(1012),
                },
            )
            .await;
        });
    } else {
        lock_mutex(&inner.connections).insert(
            connection_id.to_string(),
            ActiveRelayConnection {
                connection: conn,
                handler,
            },
        );
    }
    Ok(())
}

fn remote_close_connection(inner: &Arc<HostInner>, connection_id: &str) {
    let removed = lock_mutex(&inner.connections).shift_remove(connection_id);
    if let Some(active) = removed {
        active.connection.mark_closed();
        active.handler.on_close();
    }
}

async fn server_close_connection(
    inner: &Arc<HostInner>,
    connection_id: &str,
    final_chunk: Option<&[u8]>,
) -> pi_js::Result<()> {
    if lock_mutex(&inner.connections).shift_remove(connection_id).is_none() {
        return Ok(());
    }
    let writer = require_writer(inner)?;
    if let Some(chunk) = final_chunk {
        let frame = encode_relay_data_frame(connection_id, chunk)?;
        writer.send_binary(frame).await?;
    }
    send_control(
        inner,
        &HostOutputControlMessage::ConnectionClose {
            version: 1,
            connection_id: connection_id.to_string(),
            code: Some(1000),
        },
    )
    .await
}

async fn send_connection_data(inner: &Arc<HostInner>, connection_id: &str, chunk: &[u8]) -> pi_js::Result<()> {
    if !lock_mutex(&inner.connections).contains_key(connection_id) {
        return Err(pi_js::Error::msg("Radius relay connection is closed"));
    }
    let writer = require_writer(inner)?;
    let frame = encode_relay_data_frame(connection_id, chunk)?;
    writer.send_binary(frame).await
}

async fn send_control(inner: &Arc<HostInner>, message: &HostOutputControlMessage) -> pi_js::Result<()> {
    let writer = require_writer(inner)?;
    let value = serde_json::to_value(message).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    let line = pi_js::json::stringify(&value);
    writer.send_text(line).await
}

fn require_writer(inner: &Arc<HostInner>) -> pi_js::Result<Arc<OrderedWebSocketWriter>> {
    lock_mutex(&inner.writer)
        .clone()
        .ok_or_else(|| pi_js::Error::msg("Radius relay host is disconnected"))
}

fn drop_connections(inner: &Arc<HostInner>, error: Option<&pi_js::Error>) {
    let active: Vec<ActiveRelayConnection> = lock_mutex(&inner.connections)
        .drain(..)
        .map(|(_, entry)| entry)
        .collect();
    for entry in active {
        entry.connection.mark_closed();
        if let Some(err) = error {
            entry.handler.on_error(pi_js::Error::msg(err.message()));
        } else {
            entry.handler.on_close();
        }
    }
}

struct RelayServerByteConnection {
    inner: Arc<HostInner>,
    connection_id: String,
    closed: AtomicBool,
}

impl RelayServerByteConnection {
    fn mark_closed(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

#[async_trait]
impl ByteConnection for RelayServerByteConnection {
    fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    async fn send(&self, chunk: &[u8]) -> pi_js::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(pi_js::Error::msg("Radius relay connection is closed"));
        }
        send_connection_data(&self.inner, &self.connection_id, chunk).await
    }

    async fn close(&self, final_chunk: Option<&[u8]>) -> pi_js::Result<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        server_close_connection(&self.inner, &self.connection_id, final_chunk).await
    }
}

pub struct RadiusClientTransportFactoryOptions {
    pub server_id: ServerId,
    pub auth: Arc<RadiusRelayAuthResolver>,
    pub web_socket_factory: Option<RadiusRelayWebSocketFactory>,
}

pub fn create_radius_client_transport_factory(options: RadiusClientTransportFactoryOptions) -> ByteTransportFactory {
    let factory = options
        .web_socket_factory
        .unwrap_or_else(|| Arc::new(default_websocket_factory));
    let auth_resolver = Arc::clone(&options.auth);
    let server_id = options.server_id;
    Arc::new(move |handlers: ByteTransportHandlers| {
        let auth_resolver = Arc::clone(&auth_resolver);
        let factory = Arc::clone(&factory);
        let server_id = server_id.clone();
        Box::pin(async move {
            let auth = auth_resolver
                .resolve(RadiusRelayAuthOptions {
                    required: true,
                    signal: None,
                })
                .await?
                .ok_or_else(|| pi_js::Error::msg("Radius authentication is required"))?;
            let url = relay_websocket_url(&auth.gateway, &server_id)?;
            let socket =
                open_radius_relay_websocket(&url, RADIUS_RELAY_CLIENT_SUBPROTOCOL, &auth.token, factory, None).await?;
            Ok(RadiusClientByteTransport::new(socket, handlers) as Arc<dyn ByteTransport>)
        })
    })
}

/// Abstract subset of [`Client`] needed by [`RadiusClientReconnect`].
pub trait RadiusReconnectTarget: Send + Sync {
    fn attachment_session_id(&self) -> Option<String>;
    fn connected(&self) -> bool;
    fn connection_state(&self) -> ConnectionState;
    fn disconnect(&self, reason: Option<&str>);
    fn on_attachment_change(&self, listener: Arc<dyn Fn(Option<String>) + Send + Sync>) -> pi_js::Result<Unsubscribe>;
    fn on_connection_state_change(
        &self,
        listener: Arc<dyn Fn(ConnectionState) + Send + Sync>,
    ) -> pi_js::Result<Unsubscribe>;
    fn reconnect(&self) -> futures::future::BoxFuture<'_, pi_js::Result<()>>;
}

impl RadiusReconnectTarget for Client {
    fn attachment_session_id(&self) -> Option<String> {
        self.attachment().map(|a| a.session_id)
    }

    fn connected(&self) -> bool {
        Client::connected(self)
    }

    fn connection_state(&self) -> ConnectionState {
        Client::connection_state(self)
    }

    fn disconnect(&self, reason: Option<&str>) {
        Client::disconnect(self, reason);
    }

    fn on_attachment_change(&self, listener: Arc<dyn Fn(Option<String>) + Send + Sync>) -> pi_js::Result<Unsubscribe> {
        Client::on_attachment_change(
            self,
            Arc::new(move |target| {
                listener(target.map(|t| t.session_id));
                Ok(())
            }),
        )
    }

    fn on_connection_state_change(
        &self,
        listener: Arc<dyn Fn(ConnectionState) + Send + Sync>,
    ) -> pi_js::Result<Unsubscribe> {
        Client::on_connection_state_change(
            self,
            Arc::new(move |change| {
                listener(change.state);
                Ok(())
            }),
        )
    }

    fn reconnect(&self) -> futures::future::BoxFuture<'_, pi_js::Result<()>> {
        Box::pin(async move {
            let _ = Client::reconnect(self).await?;
            Ok(())
        })
    }
}

pub type RadiusReattachFn = Arc<dyn Fn(String) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;

struct ReconnectInner {
    client: Arc<dyn RadiusReconnectTarget>,
    reattach: RadiusReattachFn,
    abort_controller: AbortController,
    desired_session_id: Mutex<Option<String>>,
    reconnecting: Mutex<Option<tokio::task::JoinHandle<()>>>,
    disposed: AtomicBool,
    unsubs: Mutex<Vec<Unsubscribe>>,
}

/// Reconnect one established Radius client and restore its last selected Session.
#[derive(Clone)]
pub struct RadiusClientReconnect {
    inner: Arc<ReconnectInner>,
}

impl RadiusClientReconnect {
    pub fn new(client: Arc<dyn RadiusReconnectTarget>, reattach: RadiusReattachFn) -> pi_js::Result<Self> {
        let initial_session = client.attachment_session_id();
        let inner = Arc::new(ReconnectInner {
            client: Arc::clone(&client),
            reattach,
            abort_controller: AbortController::new(),
            desired_session_id: Mutex::new(initial_session),
            reconnecting: Mutex::new(None),
            disposed: AtomicBool::new(false),
            unsubs: Mutex::new(Vec::new()),
        });

        let inner_attach = Arc::clone(&inner);
        let unsub_attach = client.on_attachment_change(Arc::new(move |session_id| {
            let mut desired = lock_mutex(&inner_attach.desired_session_id);
            if let Some(id) = session_id {
                *desired = Some(id);
            } else if inner_attach.client.connected() {
                *desired = None;
            }
        }))?;

        let inner_state = Arc::clone(&inner);
        let unsub_state = client.on_connection_state_change(Arc::new(move |state| {
            if state == ConnectionState::Disconnected && !inner_state.disposed.load(Ordering::Acquire) {
                start_reconnect(&inner_state);
            }
        }))?;

        let mut unsubs = lock_mutex(&inner.unsubs);
        unsubs.push(unsub_state);
        unsubs.push(unsub_attach);
        drop(unsubs);

        Ok(Self { inner })
    }

    pub async fn dispose(&self) -> pi_js::Result<()> {
        if self.inner.disposed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let unsubs: Vec<Unsubscribe> = lock_mutex(&self.inner.unsubs).drain(..).collect();
        for unsub in unsubs {
            unsub.call();
        }
        self.inner.abort_controller.abort(None);
        if self.inner.client.connection_state() != ConnectionState::Disconnected {
            self.inner.client.disconnect(Some("Radius reconnect stopped"));
        }
        let handle = lock_mutex(&self.inner.reconnecting).take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
        Ok(())
    }
}

fn start_reconnect(inner: &Arc<ReconnectInner>) {
    let mut slot = lock_mutex(&inner.reconnecting);
    if slot.is_some() {
        return;
    }
    let inner_run = Arc::clone(inner);
    *slot = Some(tokio::spawn(async move {
        let mut retry_ms = CLIENT_RETRY_INITIAL_MS;
        let signal = inner_run.abort_controller.signal();
        while !inner_run.disposed.load(Ordering::Acquire) && !inner_run.client.connected() {
            let attempt: pi_js::Result<()> = async {
                delay_ms(retry_ms, Some(&signal)).await?;
                inner_run.client.reconnect().await?;
                let desired = lock_mutex(&inner_run.desired_session_id).clone();
                if let Some(session_id) = desired {
                    (inner_run.reattach)(session_id).await?;
                }
                Ok(())
            }
            .await;

            match attempt {
                Ok(()) => break,
                Err(err) => {
                    if inner_run.disposed.load(Ordering::Acquire) || signal.aborted() {
                        break;
                    }
                    if inner_run.client.connected() {
                        inner_run.client.disconnect(Some(&err.message()));
                    }
                    retry_ms = (retry_ms * 2).min(CLIENT_RETRY_MAX_MS);
                }
            }
        }
        *lock_mutex(&inner_run.reconnecting) = None;
    }));
}

struct RadiusClientByteTransport {
    socket: Arc<dyn RadiusRelayWebSocket>,
    handlers: ByteTransportHandlers,
    writer: Arc<OrderedWebSocketWriter>,
    closed: AtomicBool,
    unsub: Mutex<Option<Unsubscribe>>,
}

impl RadiusClientByteTransport {
    fn new(socket: Arc<dyn RadiusRelayWebSocket>, handlers: ByteTransportHandlers) -> Arc<Self> {
        let writer = Arc::new(OrderedWebSocketWriter::new(Arc::clone(&socket)));
        let transport = Arc::new(Self {
            socket: Arc::clone(&socket),
            handlers,
            writer,
            closed: AtomicBool::new(false),
            unsub: Mutex::new(None),
        });

        let t_msg = Arc::clone(&transport);
        let t_close = Arc::clone(&transport);
        let t_err = Arc::clone(&transport);
        let unsub = socket.subscribe(RadiusRelayWebSocketHandlers {
            on_open: None,
            on_message: Some(Arc::new(move |msg| {
                if t_msg.closed.load(Ordering::Acquire) {
                    return;
                }
                match msg {
                    RadiusRelayMessage::Binary(bytes) => t_msg.handlers.on_data(&bytes),
                    RadiusRelayMessage::Text(_) => {
                        t_msg.fail(pi_js::Error::msg("Radius relay client received a non-binary message"));
                    }
                }
            })),
            on_close: Some(Arc::new(move |_code, _reason| {
                if t_close.mark_closed() {
                    t_close.handlers.on_close();
                }
            })),
            on_error: Some(Arc::new(move |err| {
                t_err.fail(err);
            })),
        });
        *lock_mutex(&transport.unsub) = Some(unsub);
        if transport.closed.load(Ordering::Acquire) {
            drop(lock_mutex(&transport.unsub).take());
        }
        transport
    }

    fn fail(&self, error: pi_js::Error) {
        if !self.mark_closed() {
            return;
        }
        close_websocket_quiet(
            &self.socket,
            LOCAL_TRANSPORT_ERROR_CLOSE_CODE,
            "Radius relay transport error",
        );
        self.handlers.on_error(error);
    }

    fn mark_closed(&self) -> bool {
        if self.closed.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.writer.close();
        drop(lock_mutex(&self.unsub).take());
        true
    }
}

impl ByteTransport for RadiusClientByteTransport {
    fn send(&self, chunk: Vec<u8>) -> BoxFuture<pi_js::Result<()>> {
        if self.closed.load(Ordering::Acquire) {
            return Box::pin(futures::future::ready(Err(pi_js::Error::msg(
                "Radius relay client is closed",
            ))));
        }
        let writer = Arc::clone(&self.writer);
        Box::pin(async move { writer.send_binary(chunk).await })
    }

    fn close(&self) {
        if !self.mark_closed() {
            return;
        }
        let _ = self.socket.close(Some(1000), Some("Pi client closed"));
    }
}

enum WriterItem {
    Text(String, tokio::sync::oneshot::Sender<pi_js::Result<()>>),
    Binary(Vec<u8>, tokio::sync::oneshot::Sender<pi_js::Result<()>>),
}

struct OrderedWebSocketWriter {
    tx: mpsc::UnboundedSender<WriterItem>,
    pending_bytes: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
}

impl OrderedWebSocketWriter {
    fn new(socket: Arc<dyn RadiusRelayWebSocket>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<WriterItem>();
        let pending_bytes = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicBool::new(false));

        let pending_task = Arc::clone(&pending_bytes);
        let closed_task = Arc::clone(&closed);
        tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                let (byte_len, res, reply_tx) = match item {
                    WriterItem::Text(text, reply_tx) => {
                        let len = text.len();
                        let res = if closed_task.load(Ordering::Acquire) || socket.ready_state() != WS_STATE_OPEN {
                            Err(pi_js::Error::msg("Radius relay WebSocket is closed"))
                        } else {
                            socket.send_text(text)
                        };
                        (len, res, reply_tx)
                    }
                    WriterItem::Binary(bytes, reply_tx) => {
                        let len = bytes.len();
                        let res = if closed_task.load(Ordering::Acquire) || socket.ready_state() != WS_STATE_OPEN {
                            Err(pi_js::Error::msg("Radius relay WebSocket is closed"))
                        } else {
                            socket.send_binary(bytes)
                        };
                        (len, res, reply_tx)
                    }
                };
                let final_res = match res {
                    Ok(()) => {
                        let mut drain_err = None;
                        while socket.buffered_amount() > DRAIN_THRESHOLD_BYTES {
                            if closed_task.load(Ordering::Acquire) || socket.ready_state() != WS_STATE_OPEN {
                                drain_err = Some(pi_js::Error::msg("Radius relay WebSocket closed during write"));
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        match drain_err {
                            Some(e) => Err(e),
                            None => Ok(()),
                        }
                    }
                    Err(e) => Err(e),
                };
                pending_task.fetch_sub(byte_len, Ordering::AcqRel);
                let _ = reply_tx.send(final_res);
            }
        });

        Self {
            tx,
            pending_bytes,
            closed,
        }
    }

    async fn send_text(&self, text: String) -> pi_js::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(pi_js::Error::msg("Radius relay WebSocket is closed"));
        }
        let len = text.len();
        self.reserve_bytes(len)?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if self.tx.send(WriterItem::Text(text, reply_tx)).is_err() {
            self.pending_bytes.fetch_sub(len, Ordering::AcqRel);
            return Err(pi_js::Error::msg("Radius relay WebSocket is closed"));
        }
        reply_rx
            .await
            .map_err(|_| pi_js::Error::msg("Radius relay WebSocket is closed"))?
    }

    async fn send_binary(&self, bytes: Vec<u8>) -> pi_js::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(pi_js::Error::msg("Radius relay WebSocket is closed"));
        }
        let len = bytes.len();
        self.reserve_bytes(len)?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if self.tx.send(WriterItem::Binary(bytes, reply_tx)).is_err() {
            self.pending_bytes.fetch_sub(len, Ordering::AcqRel);
            return Err(pi_js::Error::msg("Radius relay WebSocket is closed"));
        }
        reply_rx
            .await
            .map_err(|_| pi_js::Error::msg("Radius relay WebSocket is closed"))?
    }

    fn reserve_bytes(&self, len: usize) -> pi_js::Result<()> {
        let mut current = self.pending_bytes.load(Ordering::Acquire);
        loop {
            if current + len > MAX_PENDING_BYTES {
                return Err(pi_js::Error::msg("Radius relay exceeded its pending byte limit"));
            }
            match self
                .pending_bytes
                .compare_exchange_weak(current, current + len, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Ok(()),
                Err(actual) => current = actual,
            }
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

enum HostInputControlMessage {
    Ping,
    Pong,
    ConnectionOpen {
        connection_id: String,
    },
    ConnectionClose {
        connection_id: String,
        #[allow(dead_code)]
        code: Option<u16>,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HostOutputControlMessage {
    Pong {
        version: u8,
    },
    ConnectionClose {
        version: u8,
        connection_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<u16>,
    },
}

fn parse_host_control_message(value: &str) -> pi_js::Result<HostInputControlMessage> {
    let parsed = parse(value).map_err(|_| pi_js::Error::msg("Invalid Radius relay control message"))?;
    let Some(obj) = parsed.as_object() else {
        return Err(pi_js::Error::msg("Invalid Radius relay control message"));
    };
    if obj.get("version").and_then(Value::as_i64) != Some(1) {
        return Err(pi_js::Error::msg("Unsupported Radius relay control version"));
    }
    let kind = obj.get("type").and_then(Value::as_str);
    match kind {
        Some("ping") => Ok(HostInputControlMessage::Ping),
        Some("pong") => Ok(HostInputControlMessage::Pong),
        Some("connection_open") | Some("connection_close") => {
            let Some(conn_id) = obj.get("connection_id").and_then(Value::as_str) else {
                return Err(pi_js::Error::msg("Invalid Radius relay control message"));
            };
            if !CONNECTION_ID_RE.is_match(conn_id) {
                return Err(pi_js::Error::msg("Invalid Radius relay control message"));
            }
            let code = match obj.get("code") {
                None => None,
                Some(v) => {
                    let Some(num) = v.as_i64() else {
                        return Err(pi_js::Error::msg("Invalid Radius relay control message"));
                    };
                    if !(1000..=4999).contains(&num) {
                        return Err(pi_js::Error::msg("Invalid Radius relay control message"));
                    }
                    Some(num as u16)
                }
            };
            if kind == Some("connection_open") {
                Ok(HostInputControlMessage::ConnectionOpen {
                    connection_id: conn_id.to_string(),
                })
            } else {
                Ok(HostInputControlMessage::ConnectionClose {
                    connection_id: conn_id.to_string(),
                    code,
                })
            }
        }
        _ => Err(pi_js::Error::msg("Invalid Radius relay control message")),
    }
}

pub fn encode_relay_data_frame(connection_id: &str, payload: &[u8]) -> pi_js::Result<Vec<u8>> {
    if !CONNECTION_ID_RE.is_match(connection_id) {
        return Err(pi_js::Error::js("TypeError", "Invalid Radius relay connection ID"));
    }
    let mut frame = Vec::with_capacity(RELAY_DATA_HEADER_BYTES + payload.len());
    frame.push(RELAY_DATA_FRAME_VERSION);
    frame.push(RELAY_DATA_FRAME_TYPE);
    let hex: String = connection_id.chars().filter(|c| *c != '-').collect();
    for idx in 0..16 {
        let byte = u8::from_str_radix(&hex[idx * 2..idx * 2 + 2], 16)
            .map_err(|_| pi_js::Error::js("TypeError", "Invalid Radius relay connection ID"))?;
        frame.push(byte);
    }
    frame.extend_from_slice(payload);
    Ok(frame)
}

pub fn parse_relay_data_frame(frame: &[u8]) -> Option<ParsedRelayDataFrame> {
    if frame.len() < RELAY_DATA_HEADER_BYTES {
        return None;
    }
    if frame[0] != RELAY_DATA_FRAME_VERSION || frame[1] != RELAY_DATA_FRAME_TYPE {
        return None;
    }
    let mut hex = String::with_capacity(32);
    for byte in &frame[2..RELAY_DATA_HEADER_BYTES] {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    let connection_id = format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    );
    if !CONNECTION_ID_RE.is_match(&connection_id) {
        return None;
    }
    Some(ParsedRelayDataFrame {
        connection_id,
        payload: frame[RELAY_DATA_HEADER_BYTES..].to_vec(),
    })
}

fn relay_websocket_url(gateway: &str, server_id: &ServerId) -> pi_js::Result<String> {
    let mut url = url::Url::parse(gateway).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    url.set_path(&format!("/v1/session-relays/{server_id}/connect"));
    url.set_query(None);
    url.set_fragment(None);
    match url.scheme() {
        "https" => {
            url.set_scheme("wss")
                .map_err(|()| pi_js::Error::msg("Failed to set wss scheme"))?;
        }
        "http" => {
            url.set_scheme("ws")
                .map_err(|()| pi_js::Error::msg("Failed to set ws scheme"))?;
        }
        other => {
            return Err(pi_js::Error::msg(format!(
                "Unsupported Radius gateway protocol: {other}:"
            )));
        }
    }
    Ok(url.to_string())
}

enum OutboundWsCommand {
    Message(Message),
    Close(Option<CloseFrame>),
}

enum BufferedWsEvent {
    Open,
    Message(RadiusRelayMessage),
    Close(u16, String),
    Error(pi_js::Error),
}

struct TungsteniteRelaySocket {
    selected_protocol: Mutex<String>,
    ready_state: AtomicU16,
    buffered_amount: Arc<AtomicUsize>,
    tx: mpsc::UnboundedSender<OutboundWsCommand>,
    dispatch: Mutex<WsDispatchState>,
}

struct WsDispatchState {
    subscribers: IndexMap<u64, RadiusRelayWebSocketHandlers>,
    next_id: u64,
    early_events: Vec<BufferedWsEvent>,
}

impl TungsteniteRelaySocket {
    fn emit(&self, event: BufferedWsEvent) {
        let handlers: Vec<RadiusRelayWebSocketHandlers> = {
            let mut state = lock_mutex(&self.dispatch);
            if state.subscribers.is_empty() {
                state.early_events.push(event);
                return;
            }
            state.subscribers.values().cloned().collect()
        };
        dispatch_ws_event(&handlers, event);
    }
}

fn dispatch_ws_event(handlers: &[RadiusRelayWebSocketHandlers], event: BufferedWsEvent) {
    match event {
        BufferedWsEvent::Open => {
            for h in handlers {
                if let Some(ref cb) = h.on_open {
                    cb();
                }
            }
        }
        BufferedWsEvent::Message(msg) => {
            for h in handlers {
                if let Some(ref cb) = h.on_message {
                    cb(msg.clone());
                }
            }
        }
        BufferedWsEvent::Close(code, reason) => {
            for h in handlers {
                if let Some(ref cb) = h.on_close {
                    cb(code, reason.clone());
                }
            }
        }
        BufferedWsEvent::Error(err) => {
            for h in handlers {
                if let Some(ref cb) = h.on_error {
                    cb(pi_js::Error::msg(err.message()));
                }
            }
        }
    }
}

impl RadiusRelayWebSocket for TungsteniteRelaySocket {
    fn protocol(&self) -> String {
        lock_mutex(&self.selected_protocol).clone()
    }

    fn ready_state(&self) -> u16 {
        self.ready_state.load(Ordering::Acquire)
    }

    fn buffered_amount(&self) -> usize {
        self.buffered_amount.load(Ordering::Acquire)
    }

    fn send_text(&self, data: String) -> pi_js::Result<()> {
        self.tx
            .send(OutboundWsCommand::Message(Message::Text(data.into())))
            .map_err(|_| pi_js::Error::msg("Radius relay WebSocket is closed"))
    }

    fn send_binary(&self, data: Vec<u8>) -> pi_js::Result<()> {
        self.tx
            .send(OutboundWsCommand::Message(Message::Binary(data.into())))
            .map_err(|_| pi_js::Error::msg("Radius relay WebSocket is closed"))
    }

    fn close(&self, code: Option<u16>, reason: Option<&str>) -> pi_js::Result<()> {
        let state = self.ready_state.load(Ordering::Acquire);
        if state == WS_STATE_CLOSING || state == WS_STATE_CLOSED {
            return Ok(());
        }
        self.ready_state.store(WS_STATE_CLOSING, Ordering::Release);
        let frame = code.map(|c| CloseFrame {
            code: CloseCode::from(c),
            reason: reason.unwrap_or("").to_string().into(),
        });
        let _ = self.tx.send(OutboundWsCommand::Close(frame));
        Ok(())
    }

    fn subscribe(&self, handlers: RadiusRelayWebSocketHandlers) -> Unsubscribe {
        let (_id, drained) = {
            let mut state = lock_mutex(&self.dispatch);
            let id = state.next_id;
            state.next_id += 1;
            state.subscribers.insert(id, handlers.clone());
            let drained = if state.subscribers.len() == 1 {
                std::mem::take(&mut state.early_events)
            } else {
                Vec::new()
            };
            (id, drained)
        };
        if !drained.is_empty() {
            let slice = [handlers];
            for ev in drained {
                dispatch_ws_event(&slice, ev);
            }
        }
        let dispatch_ref = &self.dispatch;
        let _ = dispatch_ref;
        // Subscribers remove themselves cleanly on drop or explicit call; use Weak via Arc helper when registered.
        // Note: TungsteniteRelaySocket is always created inside an Arc inside default_websocket_factory.
        Unsubscribe::new(|| {})
    }
}

struct ArcTungsteniteRelaySocket {
    inner: Arc<TungsteniteRelaySocket>,
}

impl RadiusRelayWebSocket for ArcTungsteniteRelaySocket {
    fn protocol(&self) -> String {
        self.inner.protocol()
    }

    fn ready_state(&self) -> u16 {
        self.inner.ready_state()
    }

    fn buffered_amount(&self) -> usize {
        self.inner.buffered_amount()
    }

    fn send_text(&self, data: String) -> pi_js::Result<()> {
        self.inner.send_text(data)
    }

    fn send_binary(&self, data: Vec<u8>) -> pi_js::Result<()> {
        self.inner.send_binary(data)
    }

    fn close(&self, code: Option<u16>, reason: Option<&str>) -> pi_js::Result<()> {
        self.inner.close(code, reason)
    }

    fn subscribe(&self, handlers: RadiusRelayWebSocketHandlers) -> Unsubscribe {
        let (id, drained) = {
            let mut state = lock_mutex(&self.inner.dispatch);
            let id = state.next_id;
            state.next_id += 1;
            state.subscribers.insert(id, handlers.clone());
            let drained = if state.subscribers.len() == 1 {
                std::mem::take(&mut state.early_events)
            } else {
                Vec::new()
            };
            (id, drained)
        };
        if !drained.is_empty() {
            let slice = [handlers];
            for ev in drained {
                dispatch_ws_event(&slice, ev);
            }
        }
        let weak = Arc::downgrade(&self.inner);
        Unsubscribe::new(move || {
            if let Some(inner) = weak.upgrade() {
                lock_mutex(&inner.dispatch).subscribers.shift_remove(&id);
            }
        })
    }
}

fn default_websocket_factory(options: RadiusRelayWebSocketConnectOptions) -> Arc<dyn RadiusRelayWebSocket> {
    let (tx, mut rx) = mpsc::unbounded_channel::<OutboundWsCommand>();
    let inner = Arc::new(TungsteniteRelaySocket {
        selected_protocol: Mutex::new(String::new()),
        ready_state: AtomicU16::new(WS_STATE_CONNECTING),
        buffered_amount: Arc::new(AtomicUsize::new(0)),
        tx,
        dispatch: Mutex::new(WsDispatchState {
            subscribers: IndexMap::new(),
            next_id: 1,
            early_events: Vec::new(),
        }),
    });

    let sock_task = Arc::clone(&inner);
    tokio::spawn(async move {
        let mut req = match options.url.into_client_request() {
            Ok(req) => req,
            Err(err) => {
                sock_task.ready_state.store(WS_STATE_CLOSED, Ordering::Release);
                sock_task.emit(BufferedWsEvent::Error(pi_js::Error::msg(err.to_string())));
                return;
            }
        };
        if let Ok(proto_val) = HeaderValue::from_str(&options.protocol) {
            req.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, proto_val);
        }
        if let Ok(auth_val) = HeaderValue::from_str(&options.authorization) {
            req.headers_mut().insert(AUTHORIZATION, auth_val);
        }
        let (ws_stream, response) = match connect_async(req).await {
            Ok(pair) => pair,
            Err(err) => {
                sock_task.ready_state.store(WS_STATE_CLOSED, Ordering::Release);
                sock_task.emit(BufferedWsEvent::Error(pi_js::Error::msg(err.to_string())));
                return;
            }
        };
        let negotiated = response
            .headers()
            .get(SEC_WEBSOCKET_PROTOCOL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        *lock_mutex(&sock_task.selected_protocol) = negotiated;
        sock_task.ready_state.store(WS_STATE_OPEN, Ordering::Release);
        sock_task.emit(BufferedWsEvent::Open);

        let (mut write, mut read) = ws_stream.split();
        let sock_write = Arc::clone(&sock_task);
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    OutboundWsCommand::Message(msg) => {
                        if let Err(err) = write.send(msg).await {
                            sock_write.ready_state.store(WS_STATE_CLOSED, Ordering::Release);
                            sock_write.emit(BufferedWsEvent::Error(pi_js::Error::msg(err.to_string())));
                            break;
                        }
                    }
                    OutboundWsCommand::Close(frame) => {
                        let _ = write.send(Message::Close(frame)).await;
                        break;
                    }
                }
            }
        });

        while let Some(item) = read.next().await {
            match item {
                Ok(Message::Text(text)) => {
                    sock_task.emit(BufferedWsEvent::Message(RadiusRelayMessage::Text(text.to_string())));
                }
                Ok(Message::Binary(bytes)) => {
                    sock_task.emit(BufferedWsEvent::Message(RadiusRelayMessage::Binary(bytes.to_vec())));
                }
                Ok(Message::Close(frame)) => {
                    sock_task.ready_state.store(WS_STATE_CLOSED, Ordering::Release);
                    let (code, reason) = match frame {
                        Some(f) => (u16::from(f.code), f.reason.to_string()),
                        None => (1000, String::new()),
                    };
                    sock_task.emit(BufferedWsEvent::Close(code, reason));
                    return;
                }
                Ok(_) => {}
                Err(err) => {
                    sock_task.ready_state.store(WS_STATE_CLOSED, Ordering::Release);
                    sock_task.emit(BufferedWsEvent::Error(pi_js::Error::msg(err.to_string())));
                    return;
                }
            }
        }
        sock_task.ready_state.store(WS_STATE_CLOSED, Ordering::Release);
        sock_task.emit(BufferedWsEvent::Close(1000, String::new()));
    });

    Arc::new(ArcTungsteniteRelaySocket { inner })
}

async fn open_radius_relay_websocket(
    url: &str,
    protocol: &str,
    token: &str,
    factory: RadiusRelayWebSocketFactory,
    signal: Option<&AbortSignal>,
) -> pi_js::Result<Arc<dyn RadiusRelayWebSocket>> {
    if let Some(sig) = signal {
        sig.throw_if_aborted()?;
    }
    let socket = factory(RadiusRelayWebSocketConnectOptions {
        url: url.to_string(),
        protocol: protocol.to_string(),
        authorization: format!("Bearer {token}"),
    });
    let (tx, rx) = tokio::sync::oneshot::channel::<pi_js::Result<()>>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let settled = Arc::new(AtomicBool::new(false));
    let expected_protocol = protocol.to_string();

    let fail = {
        let socket = Arc::clone(&socket);
        let settled = Arc::clone(&settled);
        let tx = Arc::clone(&tx);
        Arc::new(move |error: pi_js::Error| {
            if settled.swap(true, Ordering::AcqRel) {
                return;
            }
            close_websocket_quiet(&socket, 1000, "Radius relay connection failed");
            if let Some(sender) = lock_mutex(&tx).take() {
                let _ = sender.send(Err(error));
            }
        })
    };

    let unsub = socket.subscribe(RadiusRelayWebSocketHandlers {
        on_open: Some({
            let socket = Arc::clone(&socket);
            let settled = Arc::clone(&settled);
            let tx = Arc::clone(&tx);
            let fail = Arc::clone(&fail);
            Arc::new(move || {
                if settled.load(Ordering::Acquire) {
                    return;
                }
                let actual = socket.protocol();
                if actual != expected_protocol {
                    let proto_json = pi_js::json::stringify(&Value::String(actual));
                    fail(pi_js::Error::msg(format!(
                        "Radius relay selected unexpected WebSocket protocol {proto_json}"
                    )));
                    return;
                }
                if !settled.swap(true, Ordering::AcqRel)
                    && let Some(sender) = lock_mutex(&tx).take()
                {
                    let _ = sender.send(Ok(()));
                }
            })
        }),
        on_message: None,
        on_close: Some({
            let fail = Arc::clone(&fail);
            Arc::new(move |code, _reason| {
                fail(pi_js::Error::msg(format!(
                    "Radius relay closed before connecting ({code})"
                )));
            })
        }),
        on_error: Some({
            let fail = Arc::clone(&fail);
            Arc::new(move |err| {
                if err.message().trim().is_empty() {
                    fail(pi_js::Error::msg("Radius WebSocket connection failed"));
                } else {
                    fail(err);
                }
            })
        }),
    });

    let res = match signal {
        Some(sig) => {
            tokio::select! {
                _ = sig.cancelled() => {
                    fail(pi_js::Error::msg("Radius relay connection cancelled"));
                    Err(pi_js::Error::msg("Radius relay connection cancelled"))
                }
                res = rx => res.map_err(|_| pi_js::Error::msg("Radius relay connection cancelled"))?,
            }
        }
        None => rx
            .await
            .map_err(|_| pi_js::Error::msg("Radius relay connection cancelled"))?,
    };
    unsub.call();
    res?;
    Ok(socket)
}

fn close_websocket_quiet(socket: &Arc<dyn RadiusRelayWebSocket>, code: u16, reason: &str) {
    let _ = socket.close(Some(code), Some(reason));
}

async fn delay_ms(ms: u64, signal: Option<&AbortSignal>) -> pi_js::Result<()> {
    if let Some(sig) = signal {
        sig.throw_if_aborted()?;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
            _ = sig.cancelled() => {
                sig.throw_if_aborted()?;
                Err(pi_js::Error::msg("The operation was aborted"))
            }
        }
    } else {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(())
    }
}

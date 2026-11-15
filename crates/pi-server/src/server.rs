//! Port of packages/server/src/server.ts

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::context::{BACKGROUND_CONTEXT, TODO_CONTEXT, with_abort_signal};
use pi_chord::{
    Context, JsonValue, RemoteServiceError, ServiceCall, ServiceControlCall, ServiceProviderUpdate,
    ServiceUpdatePublisher, create_service_state_encoder, decode_service_control_call, parse_service_call,
    parse_service_subscription_snapshot,
};
use pi_js::abort::{AbortController, AbortReason, AbortSignal};
use pi_js::time::set_timeout;
use pi_js::{BoxFuture, Error, Result};
use pi_protocol::{
    AttachmentEnvelope, ClientHello, ClientMessage, ClientMessageDecoder, DEFAULT_MAX_FRAME_LENGTH,
    FrameDecoderOptions, PROTOCOL_VERSION, ProtocolError, ProtocolValidationError, RequestEnvelope, ResponseEnvelope,
    RpcTarget, ServerHello, ServerHelloError, ServerMessage, ServiceEventEnvelope, encode_server_message, is_server_id,
    is_supported_protocol_version,
};
use serde_json::Value;
use tokio::sync::watch;

use crate::connection::{
    ActiveRequest, ByteConnection, ByteConnectionHandler, ConnectionStage, ConnectionState, is_terminal_connection,
};
use crate::errors::{AggregateError, INTERNAL_SERVER_ERROR_MESSAGE, ServerError, WrongServerError, shared_js_error};
use crate::listener::ServerListener;
use crate::session_router::{SessionRouter, SessionRouterOptions};
use crate::types::{
    ConnectionCountCallback, DefaultSessionMetadata, RoutedServerPresentation, ServerErrorCallback, ServerHost,
    ServerOptions, SessionMetadata,
};

const DEFAULT_HANDSHAKE_TIMEOUT_MS: i64 = 5_000;
const MAX_UINT32: i64 = 0xffff_ffff;
const MAX_TIMER_DELAY_MS: i64 = 2_147_483_647;

struct ServerLifecycle {
    started: bool,
    starting: bool,
    closing: bool,
    start_done_rx: Option<watch::Receiver<bool>>,
    close_rx: Option<watch::Receiver<Option<std::result::Result<(), Arc<Error>>>>>,
    closed_settled: bool,
}

struct ServerInner<TMetadata: SessionMetadata> {
    server_id: String,
    host: Arc<dyn ServerHost<TMetadata>>,
    listeners: Vec<Arc<dyn ServerListener>>,
    max_frame_length: i64,
    handshake_timeout_ms: i64,
    on_connection_count_changed: Option<ConnectionCountCallback>,
    on_error: Option<ServerErrorCallback>,
    connections: Mutex<IndexMap<usize, Arc<ConnectionState>>>,
    sessions: SessionRouter<TMetadata>,
    lifecycle: Mutex<ServerLifecycle>,
    closed_tx: watch::Sender<Option<std::result::Result<(), Arc<Error>>>>,
    closed_rx: watch::Receiver<Option<std::result::Result<(), Arc<Error>>>>,
    next_request_token: AtomicU64,
}

/// Logical RPC server accepting byte connections across one or more [`ServerListener`]s.
pub struct Server<TMetadata: SessionMetadata = DefaultSessionMetadata> {
    inner: Arc<ServerInner<TMetadata>>,
}

impl<TMetadata: SessionMetadata> Clone for Server<TMetadata> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<TMetadata: SessionMetadata> std::fmt::Debug for Server<TMetadata> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("server_id", &self.inner.server_id)
            .finish()
    }
}

struct SubscriptionBufferInner {
    ready: bool,
    pending: Vec<ServiceProviderUpdate>,
}

struct ServerConnectionHandler<TMetadata: SessionMetadata> {
    server: Server<TMetadata>,
    state: Arc<ConnectionState>,
}

impl<TMetadata: SessionMetadata> ByteConnectionHandler for ServerConnectionHandler<TMetadata> {
    fn on_data(&self, chunk: &[u8]) {
        self.server.receive(&self.state, chunk);
    }

    fn on_close(&self) {
        self.server.transport_closed(&self.state);
    }

    fn on_error(&self, error: Error) {
        self.server.report_error(&error);
        let server = self.server.clone();
        let state = self.state.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                server.close_connection(state.connection.clone(), None).await;
                server.disconnect(&state);
            });
        } else {
            self.server.disconnect(&self.state);
        }
    }
}

struct NoopConnectionHandler<TMetadata: SessionMetadata> {
    server: Server<TMetadata>,
}

impl<TMetadata: SessionMetadata> ByteConnectionHandler for NoopConnectionHandler<TMetadata> {
    fn on_data(&self, _chunk: &[u8]) {}

    fn on_close(&self) {}

    fn on_error(&self, error: Error) {
        self.server.report_error(&error);
    }
}

struct RouterPresentation<TMetadata: SessionMetadata> {
    router: SessionRouter<TMetadata>,
    client: Arc<ConnectionState>,
}

#[async_trait]
impl<TMetadata: SessionMetadata> RoutedServerPresentation for RouterPresentation<TMetadata> {
    async fn attach_session(&self, session_id: &str, context: Context) -> Result<()> {
        self.router
            .attach_client(self.client.clone(), session_id.to_string(), context)
            .await
    }

    async fn detach_session(&self, context: Context) -> Result<()> {
        self.router.detach_client(self.client.clone(), context).await
    }

    async fn prepare_session_removal(&self, session_id: &str, context: Context) -> Result<()> {
        self.router.remove_session(session_id, context).await
    }
}

impl<TMetadata: SessionMetadata> Server<TMetadata> {
    pub fn new(host: Arc<dyn ServerHost<TMetadata>>, options: ServerOptions) -> Result<Self> {
        let (max_frame_length, handshake_timeout_ms) = resolve_options(&options)?;
        let (closed_tx, closed_rx) = watch::channel(None);
        let server_id = options.server_id.clone();

        let inner = Arc::new_cyclic(|weak: &std::sync::Weak<ServerInner<TMetadata>>| {
            let weak_publish = weak.clone();
            let weak_error = weak.clone();
            let weak_closing = weak.clone();
            let sessions = SessionRouter::new(SessionRouterOptions {
                host: host.clone(),
                server_id: server_id.clone(),
                is_closing: Arc::new(move || {
                    weak_closing
                        .upgrade()
                        .is_none_or(|s| s.lifecycle.lock().expect("lifecycle mutex poisoned").closing)
                }),
                publish_attachment: Arc::new(move |client, attachment, _context| {
                    let weak = weak_publish.clone();
                    Box::pin(async move {
                        let Some(inner) = weak.upgrade() else {
                            return Ok(());
                        };
                        let server = Server { inner };
                        let _ = server
                            .send_message(&client, &ServerMessage::Attachment(AttachmentEnvelope { attachment }))
                            .await;
                        Ok(())
                    })
                }),
                report_error: Arc::new(move |error| {
                    if let Some(inner) = weak_error.upgrade() {
                        Server { inner }.report_error(error);
                    }
                }),
            });
            ServerInner {
                server_id,
                host,
                listeners: options.listeners,
                max_frame_length,
                handshake_timeout_ms,
                on_connection_count_changed: options.on_connection_count_changed,
                on_error: options.on_error,
                connections: Mutex::new(IndexMap::new()),
                sessions,
                lifecycle: Mutex::new(ServerLifecycle {
                    started: false,
                    starting: false,
                    closing: false,
                    start_done_rx: None,
                    close_rx: None,
                    closed_settled: false,
                }),
                closed_tx,
                closed_rx,
                next_request_token: AtomicU64::new(1),
            }
        });

        Ok(Self { inner })
    }

    pub fn server_id(&self) -> &str {
        &self.inner.server_id
    }

    /// Resolves after shutdown, or fails when listener or routed-Session cleanup fails.
    pub fn closed(&self) -> BoxFuture<Result<()>> {
        let mut rx = self.inner.closed_rx.clone();
        Box::pin(async move {
            let _ = rx.wait_for(|val| val.is_some()).await;
            let outcome = rx.borrow().clone().expect("watch reported Some before borrow");
            match outcome {
                Ok(()) => Ok(()),
                Err(shared) => Err(shared_js_error(shared)),
            }
        })
    }

    pub fn start(&self) -> BoxFuture<Result<Self>> {
        let mut guard = self.inner.lifecycle.lock().expect("lifecycle mutex poisoned");
        if guard.started {
            return Box::pin(async { Err(Error::msg("Server is already started")) });
        }
        if guard.starting {
            return Box::pin(async { Err(Error::msg("Server is already starting")) });
        }
        if guard.closing {
            return Box::pin(async { Err(Error::msg("Server is closing or closed")) });
        }
        let (done_tx, done_rx) = watch::channel(false);
        guard.starting = true;
        guard.start_done_rx = Some(done_rx);
        drop(guard);

        let server = self.clone();
        let (res_tx, res_rx) = tokio::sync::oneshot::channel();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let res = server.start_internal(done_tx).await;
                let _ = res_tx.send(res);
            });
            Box::pin(async move {
                res_rx
                    .await
                    .unwrap_or_else(|_| Err(Error::msg("Server startup task aborted")))
            })
        } else {
            Box::pin(async move { server.start_internal(done_tx).await })
        }
    }

    async fn start_internal(&self, done_tx: watch::Sender<bool>) -> Result<Self> {
        struct StartCleanupGuard<TMetadata: SessionMetadata> {
            inner: Arc<ServerInner<TMetadata>>,
            done_tx: watch::Sender<bool>,
        }
        impl<TMetadata: SessionMetadata> Drop for StartCleanupGuard<TMetadata> {
            fn drop(&mut self) {
                let mut guard = self.inner.lifecycle.lock().expect("lifecycle mutex poisoned");
                guard.starting = false;
                guard.start_done_rx = None;
                self.done_tx.send_replace(true);
            }
        }
        let _start_guard = StartCleanupGuard {
            inner: self.inner.clone(),
            done_tx,
        };

        let mut started_listeners: Vec<Arc<dyn ServerListener>> = Vec::new();
        let mut startup_error: Option<Error> = None;

        for listener in &self.inner.listeners {
            let server = self.clone();
            let acceptor: crate::connection::ByteConnectionAcceptor =
                Arc::new(move |connection| server.accept(connection));
            match listener.start(acceptor).await {
                Ok(()) => started_listeners.push(listener.clone()),
                Err(err) => {
                    startup_error = Some(err);
                    break;
                }
            }
        }

        if let Some(error) = startup_error {
            {
                let mut guard = self.inner.lifecycle.lock().expect("lifecycle mutex poisoned");
                guard.closing = true;
            }
            let mut cleanup_errors = Vec::new();
            let close_futs = started_listeners.iter().map(|listener| listener.close());
            for res in futures::future::join_all(close_futs).await {
                if let Err(err) = res {
                    cleanup_errors.push(err);
                }
            }
            if let Err(cleanup_error) = self.close_server_state().await {
                cleanup_errors.push(cleanup_error);
            }
            if !cleanup_errors.is_empty() {
                let mut all_errors = Vec::with_capacity(1 + cleanup_errors.len());
                all_errors.push(error);
                all_errors.extend(cleanup_errors);
                let failure = AggregateError::new(all_errors, "Server startup and cleanup failed").into_error();
                let shared = Arc::new(failure);
                self.settle_closed(Some(shared.clone()));
                return Err(shared_js_error(shared));
            }
            self.settle_closed(None);
            return Err(error);
        }

        {
            let mut guard = self.inner.lifecycle.lock().expect("lifecycle mutex poisoned");
            guard.started = true;
        }
        Ok(self.clone())
    }

    pub fn accept(&self, connection: Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> {
        let is_closing = self.inner.lifecycle.lock().expect("lifecycle mutex poisoned").closing;
        if is_closing {
            let server = self.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    server.close_connection(connection, None).await;
                });
            }
            return Arc::new(NoopConnectionHandler { server: self.clone() });
        }

        let decoder = ClientMessageDecoder::new(Some(FrameDecoderOptions {
            max_frame_length: Some(self.inner.max_frame_length),
        }));

        let state = Arc::new_cyclic(|weak| {
            let weak_timeout = weak.clone();
            let server_clone = self.clone();
            let timeout = set_timeout(self.inner.handshake_timeout_ms as u64, move || {
                let Some(st) = weak_timeout.upgrade() else {
                    return;
                };
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        server_clone
                            .fail_protocol(
                                &st,
                                ProtocolError {
                                    code: "invalid_request".to_string(),
                                    message: "Handshake timeout".to_string(),
                                },
                            )
                            .await;
                    });
                }
            });
            timeout.unref();
            ConnectionState::new(connection, decoder, timeout)
        });

        {
            let mut conns = self.inner.connections.lock().expect("connections mutex poisoned");
            conns.insert(Arc::as_ptr(&state) as usize, state.clone());
        }
        self.notify_connection_count_changed();

        Arc::new(ServerConnectionHandler {
            server: self.clone(),
            state,
        })
    }

    pub fn close(&self) -> BoxFuture<Result<()>> {
        let mut guard = self.inner.lifecycle.lock().expect("lifecycle mutex poisoned");
        if let Some(rx) = guard.close_rx.clone() {
            drop(guard);
            return Box::pin(async move {
                let mut rx = rx;
                let _ = rx.wait_for(|val| val.is_some()).await;
                let outcome = rx.borrow().clone().expect("watch reported Some before borrow");
                match outcome {
                    Ok(()) => Ok(()),
                    Err(shared) => Err(shared_js_error(shared)),
                }
            });
        }
        guard.closing = true;
        let (tx, rx) = watch::channel(None);
        guard.close_rx = Some(rx.clone());
        let start_done_rx = guard.start_done_rx.clone();
        drop(guard);

        let server = self.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let res = server.close_internal(start_done_rx).await;
                match res {
                    Ok(()) => {
                        tx.send_replace(Some(Ok(())));
                    }
                    Err(err) => {
                        tx.send_replace(Some(Err(Arc::new(err))));
                    }
                }
            });
            Box::pin(async move {
                let mut rx = rx;
                let _ = rx.wait_for(|val| val.is_some()).await;
                let outcome = rx.borrow().clone().expect("watch reported Some before borrow");
                match outcome {
                    Ok(()) => Ok(()),
                    Err(shared) => Err(shared_js_error(shared)),
                }
            })
        } else {
            Box::pin(async move { server.close_internal(start_done_rx).await })
        }
    }

    async fn close_internal(&self, start_done_rx: Option<watch::Receiver<bool>>) -> Result<()> {
        if let Some(mut rx) = start_done_rx {
            let _ = rx.wait_for(|done| *done).await;
        }
        let mut errors: Vec<Error> = Vec::new();
        let listener_futs = self.inner.listeners.iter().map(|listener| listener.close());
        for res in futures::future::join_all(listener_futs).await {
            if let Err(err) = res {
                errors.push(err);
            }
        }
        if let Err(err) = self.close_server_state().await {
            errors.push(err);
        }
        {
            let mut guard = self.inner.lifecycle.lock().expect("lifecycle mutex poisoned");
            guard.started = false;
        }
        if !errors.is_empty() {
            let failure = if errors.len() == 1 {
                errors.remove(0)
            } else {
                AggregateError::new(errors, "Server shutdown failed").into_error()
            };
            let shared = Arc::new(failure);
            self.settle_closed(Some(shared.clone()));
            return Err(shared_js_error(shared));
        }
        self.settle_closed(None);
        Ok(())
    }

    fn receive(&self, state: &Arc<ConnectionState>, chunk: &[u8]) {
        if is_terminal_connection(state) {
            return;
        }
        let messages = match state.decoder.push(chunk) {
            Ok(msgs) => msgs,
            Err(err) => {
                let proto_err = self.to_protocol_error(&err);
                self.spawn_fail_protocol(state, proto_err);
                return;
            }
        };
        for message in messages {
            if is_terminal_connection(state) {
                return;
            }
            self.dispatch_message(state, message);
        }
    }

    fn dispatch_message(&self, state: &Arc<ConnectionState>, message: ClientMessage) {
        let mut guard = state.inner.lock().expect("connection mutex poisoned");
        if guard.disconnected || guard.stage == ConnectionStage::Closing || guard.stage == ConnectionStage::Closed {
            return;
        }
        match guard.stage {
            ConnectionStage::AwaitingHello => {
                let ClientMessage::Hello(hello) = message else {
                    drop(guard);
                    self.spawn_fail_protocol(
                        state,
                        ProtocolError {
                            code: "invalid_request".to_string(),
                            message: "The first client message must be hello".to_string(),
                        },
                    );
                    return;
                };
                guard.stage = ConnectionStage::Handshaking;
                drop(guard);

                let server = self.clone();
                let st = state.clone();
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        if let Err(err) = server.finish_handshake(&st, hello).await {
                            let proto_err = server.to_protocol_error(&err);
                            server.fail_protocol(&st, proto_err).await;
                        }
                    });
                }
            }
            ConnectionStage::Handshaking => {
                if matches!(message, ClientMessage::Hello(_)) {
                    drop(guard);
                    self.spawn_fail_protocol(
                        state,
                        ProtocolError {
                            code: "invalid_request".to_string(),
                            message: "hello may only be sent as the first message".to_string(),
                        },
                    );
                    return;
                }
                guard.handshake_waiters.push(message);
            }
            ConnectionStage::Ready => {
                if matches!(message, ClientMessage::Hello(_)) {
                    drop(guard);
                    self.spawn_fail_protocol(
                        state,
                        ProtocolError {
                            code: "invalid_request".to_string(),
                            message: "hello may only be sent as the first message".to_string(),
                        },
                    );
                    return;
                }
                drop(guard);
                self.dispatch_ready_message(state, message);
            }
            ConnectionStage::Closing | ConnectionStage::Closed => {}
        }
    }

    fn dispatch_ready_message(&self, state: &Arc<ConnectionState>, message: ClientMessage) {
        match message {
            ClientMessage::Cancel(cancel) => {
                self.handle_cancel(state, &cancel.id, &cancel.target);
            }
            ClientMessage::Request(request) => {
                self.start_request(state, request);
            }
            ClientMessage::Hello(_) => {
                self.spawn_fail_protocol(
                    state,
                    ProtocolError {
                        code: "invalid_request".to_string(),
                        message: "hello may only be sent as the first message".to_string(),
                    },
                );
            }
        }
    }

    async fn finish_handshake(&self, state: &Arc<ConnectionState>, hello: ClientHello) -> Result<()> {
        if !is_supported_protocol_version(hello.version as f64) {
            self.fail_protocol(
                state,
                ProtocolError {
                    code: "version".to_string(),
                    message: format!(
                        "Unsupported protocol version {}; expected {}",
                        hello.version, PROTOCOL_VERSION
                    ),
                },
            )
            .await;
            return Ok(());
        }

        if self.should_abort_handshake(state) {
            return Ok(());
        }

        let presentation = Arc::new(RouterPresentation {
            router: self.inner.sessions.clone(),
            client: state.clone(),
        });
        let services = self
            .inner
            .host
            .server_services()
            .attach_client(presentation, TODO_CONTEXT.clone())
            .await?;

        if self.should_abort_handshake(state) {
            services.release(TODO_CONTEXT.clone()).await?;
            return Ok(());
        }

        {
            let mut guard = state.inner.lock().expect("connection mutex poisoned");
            guard.server_services = Some(services);
        }

        let sent = self
            .send_message(
                state,
                &ServerMessage::Hello(ServerHello {
                    version: PROTOCOL_VERSION,
                    server_id: self.inner.server_id.clone(),
                }),
            )
            .await;

        if sent {
            let waiters = {
                let mut guard = state.inner.lock().expect("connection mutex poisoned");
                if !guard.disconnected && guard.stage == ConnectionStage::Handshaking {
                    guard.stage = ConnectionStage::Ready;
                    if let Some(timeout) = guard.handshake_timeout.take() {
                        timeout.clear();
                    }
                    std::mem::take(&mut guard.handshake_waiters)
                } else {
                    Vec::new()
                }
            };
            for msg in waiters {
                let ready = {
                    let guard = state.inner.lock().expect("connection mutex poisoned");
                    guard.stage == ConnectionStage::Ready && !guard.disconnected
                };
                if !ready {
                    break;
                }
                self.dispatch_ready_message(state, msg);
            }
        }

        Ok(())
    }

    fn should_abort_handshake(&self, state: &ConnectionState) -> bool {
        let closing = self.inner.lifecycle.lock().expect("lifecycle mutex poisoned").closing;
        let guard = state.inner.lock().expect("connection mutex poisoned");
        closing || guard.disconnected || guard.stage != ConnectionStage::Handshaking || state.connection.closed()
    }

    fn handle_cancel(&self, state: &ConnectionState, id: &str, target: &RpcTarget) {
        if target_server_id(target) != self.inner.server_id {
            return;
        }
        let guard = state.inner.lock().expect("connection mutex poisoned");
        if let Some(active) = guard.active_requests.get(id)
            && active.target == *target
        {
            active
                .controller
                .abort(Some(AbortReason::error("AbortError", "RPC request cancelled")));
        }
    }

    fn start_request(&self, state: &Arc<ConnectionState>, envelope: RequestEnvelope) {
        {
            let guard = state.inner.lock().expect("connection mutex poisoned");
            if guard.active_requests.contains_key(&envelope.id) {
                drop(guard);
                let server = self.clone();
                let st = state.clone();
                let id = envelope.id;
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        let _ = server
                            .send_message(
                                &st,
                                &ServerMessage::Response(ResponseEnvelope {
                                    id,
                                    ok: false,
                                    result: None,
                                    error: Some(ProtocolError {
                                        code: "invalid_request".to_string(),
                                        message: "Request ID is already active".to_string(),
                                    }),
                                }),
                            )
                            .await;
                    });
                }
                return;
            }
        }

        let call = match parse_service_call(&envelope.call) {
            Ok(call) => call,
            Err(_) => {
                let server = self.clone();
                let st = state.clone();
                let id = envelope.id;
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        let _ = server
                            .send_message(
                                &st,
                                &ServerMessage::Response(ResponseEnvelope {
                                    id,
                                    ok: false,
                                    result: None,
                                    error: Some(ProtocolError {
                                        code: "invalid_request".to_string(),
                                        message: "Invalid service call".to_string(),
                                    }),
                                }),
                            )
                            .await;
                    });
                }
                return;
            }
        };

        let controller = AbortController::new();
        let signal = controller.signal();
        let token = self.inner.next_request_token.fetch_add(1, Ordering::Relaxed);
        let active = ActiveRequest {
            controller,
            target: envelope.target.clone(),
        };
        {
            let mut guard = state.inner.lock().expect("connection mutex poisoned");
            guard.active_requests.insert(envelope.id.clone(), active);
        }

        let server = self.clone();
        let st = state.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = token;
                server.handle_request(&st, envelope, call, signal).await;
            });
        }
    }

    async fn handle_request(
        &self,
        state: &Arc<ConnectionState>,
        envelope: RequestEnvelope,
        call: ServiceCall,
        signal: AbortSignal,
    ) {
        struct ActiveRequestGuard {
            state: Arc<ConnectionState>,
            id: String,
        }
        impl Drop for ActiveRequestGuard {
            fn drop(&mut self) {
                let mut guard = self.state.inner.lock().expect("connection mutex poisoned");
                guard.active_requests.shift_remove(&self.id);
            }
        }
        let _active_guard = ActiveRequestGuard {
            state: state.clone(),
            id: envelope.id.clone(),
        };

        let context = with_abort_signal(signal.clone(), TODO_CONTEXT.clone());
        let control = decode_service_control_call(&call);
        let subscribing_id = match &control {
            Some(ServiceControlCall::Subscribe { subscription_id, .. }) => Some(subscription_id.clone()),
            _ => None,
        };
        let unsubscribe_id = match &control {
            Some(ServiceControlCall::Unsubscribe { subscription_id }) => Some(subscription_id.clone()),
            _ => None,
        };

        let sub_buf = Arc::new(tokio::sync::Mutex::new(SubscriptionBufferInner {
            ready: subscribing_id.is_none(),
            pending: Vec::new(),
        }));
        let mut installed_subscription_encoder = false;
        let mut responded = false;

        let server_pub = self.clone();
        let state_pub = state.clone();
        let sub_id_pub = subscribing_id.clone();
        let buf_pub = sub_buf.clone();
        let publish: ServiceUpdatePublisher = Arc::new(move |subscription_id, update, _ctx| {
            let server = server_pub.clone();
            let state = state_pub.clone();
            let sub_id_match = sub_id_pub.clone();
            let buf = buf_pub.clone();
            Box::pin(async move {
                if sub_id_match.as_deref() == Some(subscription_id.as_str()) {
                    let mut guard = buf.lock().await;
                    if !guard.ready {
                        guard.pending.push(update);
                        return Ok(());
                    }
                    drop(guard);
                }
                server.send_service_update(&state, &subscription_id, &update).await;
                Ok(())
            })
        });

        let outcome: Result<Option<JsonValue>> = async {
            if target_server_id(&envelope.target) != self.inner.server_id {
                return Err(WrongServerError::new().into());
            }
            if let Some(ref sub_id) = subscribing_id {
                let guard = state.inner.lock().expect("connection mutex poisoned");
                if guard.service_state_encoders.contains_key(sub_id) {
                    return Err(Error::typed(
                        "ProtocolValidationError",
                        ProtocolValidationError::new(format!("Duplicate service subscription {sub_id}")),
                    ));
                }
            }

            let mut result = match &envelope.target {
                RpcTarget::Session(_) => {
                    self.inner
                        .sessions
                        .execute_service_call(call.clone(), envelope.target.clone(), state.clone(), publish, context)
                        .await?
                }
                RpcTarget::Server(_) => {
                    let server_services = {
                        state
                            .inner
                            .lock()
                            .expect("connection mutex poisoned")
                            .server_services
                            .clone()
                    };
                    let Some(services) = server_services else {
                        return Err(Error::typed(
                            "ProtocolValidationError",
                            ProtocolValidationError::new(format!(
                                "Unknown service member {}.{}",
                                call.service_id, call.member
                            )),
                        ));
                    };
                    services.invoke_service(&call, publish, context).await?
                }
            };

            if let Some(ref sub_id) = subscribing_id {
                let Some(snapshot_value) = result else {
                    return Err(Error::typed(
                        "ProtocolValidationError",
                        ProtocolValidationError::new("Service subscription did not return a snapshot"),
                    ));
                };
                let state_encoder = create_service_state_encoder();
                let snapshot = parse_service_subscription_snapshot(&snapshot_value)?;
                let encoded = state_encoder.encode_snapshot(&snapshot)?;
                let encoded_json = serde_json::to_value(encoded).map_err(|e| Error::msg(e.to_string()))?;
                {
                    let mut guard = state.inner.lock().expect("connection mutex poisoned");
                    guard.service_state_encoders.insert(sub_id.clone(), state_encoder);
                }
                installed_subscription_encoder = true;
                result = Some(encoded_json);
            } else if let Some(ref unsub_id) = unsubscribe_id {
                let mut guard = state.inner.lock().expect("connection mutex poisoned");
                guard.service_state_encoders.shift_remove(unsub_id);
            }

            self.send_message(
                state,
                &ServerMessage::Response(ResponseEnvelope {
                    id: envelope.id.clone(),
                    ok: true,
                    result,
                    error: None,
                }),
            )
            .await;
            responded = true;

            if let Some(ref sub_id) = subscribing_id {
                let mut buf = sub_buf.lock().await;
                let pending = std::mem::take(&mut buf.pending);
                for update in pending {
                    self.send_service_update(state, sub_id, &update).await;
                }
                buf.ready = true;
            }

            Ok(None)
        }
        .await;

        if let Err(error) = outcome {
            if let Some(ref sub_id) = subscribing_id
                && installed_subscription_encoder
                && !responded
            {
                let mut guard = state.inner.lock().expect("connection mutex poisoned");
                guard.service_state_encoders.shift_remove(sub_id);
            }
            if responded {
                self.report_error(&error);
                self.close_connection(state.connection.clone(), None).await;
                self.disconnect(state);
            } else {
                let proto_err = if signal.aborted() {
                    ProtocolError {
                        code: "cancelled".to_string(),
                        message: "RPC request cancelled".to_string(),
                    }
                } else {
                    self.to_protocol_error(&error)
                };
                self.send_message(
                    state,
                    &ServerMessage::Response(ResponseEnvelope {
                        id: envelope.id.clone(),
                        ok: false,
                        result: None,
                        error: Some(proto_err),
                    }),
                )
                .await;
            }
        }
    }

    fn transport_closed(&self, state: &Arc<ConnectionState>) {
        let should_end_decoder = {
            let guard = state.inner.lock().expect("connection mutex poisoned");
            !guard.disconnected && guard.stage != ConnectionStage::Closing
        };
        if should_end_decoder && let Err(err) = state.decoder.end() {
            self.report_error(&err);
        }
        self.disconnect(state);
    }

    fn disconnect(&self, state: &Arc<ConnectionState>) {
        let (server_services, controllers) = {
            let mut guard = state.inner.lock().expect("connection mutex poisoned");
            if guard.disconnected {
                return;
            }
            guard.disconnected = true;
            guard.stage = ConnectionStage::Closed;
            if let Some(timeout) = guard.handshake_timeout.take() {
                timeout.clear();
            }
            guard.handshake_waiters.clear();
            let controllers: Vec<AbortController> = guard
                .active_requests
                .drain(..)
                .map(|(_, active)| active.controller)
                .collect();
            guard.service_state_encoders.clear();
            (guard.server_services.take(), controllers)
        };

        for controller in controllers {
            controller.abort(Some(AbortReason::error("Error", "Client disconnected")));
        }
        state.close_notify.notify_waiters();

        let removed = {
            let mut conns = self.inner.connections.lock().expect("connections mutex poisoned");
            conns.shift_remove(&(Arc::as_ptr(state) as usize)).is_some()
        };
        if removed {
            self.notify_connection_count_changed();
        }

        let server = self.clone();
        let st = state.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let disconnect_fut = server.inner.sessions.disconnect(st, TODO_CONTEXT.clone());
                let release_fut = async move {
                    if let Some(services) = server_services {
                        services.release(TODO_CONTEXT.clone()).await
                    } else {
                        Ok(())
                    }
                };
                let (d_res, r_res) = tokio::join!(disconnect_fut, release_fut);
                if let Err(err) = d_res {
                    server.report_error(&err);
                }
                if let Err(err) = r_res {
                    server.report_error(&err);
                }
            });
        }
    }

    async fn send_service_update(
        &self,
        state: &Arc<ConnectionState>,
        subscription_id: &str,
        update: &ServiceProviderUpdate,
    ) {
        let encoded_json = {
            let guard = state.inner.lock().expect("connection mutex poisoned");
            let Some(encoder) = guard.service_state_encoders.get(subscription_id) else {
                return;
            };
            match encoder.encode_update(update) {
                Ok(wire) => match serde_json::to_value(wire) {
                    Ok(v) => v,
                    Err(e) => {
                        drop(guard);
                        self.report_error(&Error::msg(e.to_string()));
                        return;
                    }
                },
                Err(e) => {
                    drop(guard);
                    self.report_error(&e);
                    return;
                }
            }
        };
        self.send_message(
            state,
            &ServerMessage::ServiceUpdate(ServiceEventEnvelope {
                subscription_id: subscription_id.to_string(),
                update: encoded_json,
            }),
        )
        .await;
    }

    async fn send_message(&self, state: &Arc<ConnectionState>, message: &ServerMessage) -> bool {
        if state.disconnected() || state.connection.closed() {
            return false;
        }
        let frame = match encode_server_message(
            message,
            Some(FrameDecoderOptions {
                max_frame_length: Some(self.inner.max_frame_length),
            }),
        ) {
            Ok(bytes) => bytes,
            Err(err) => {
                self.report_error(&err);
                self.close_connection(state.connection.clone(), None).await;
                self.disconnect(state);
                return false;
            }
        };
        match state.connection.send(&frame).await {
            Ok(()) => true,
            Err(err) => {
                self.report_error(&err);
                self.close_connection(state.connection.clone(), None).await;
                self.disconnect(state);
                false
            }
        }
    }

    fn spawn_fail_protocol(&self, state: &Arc<ConnectionState>, error: ProtocolError) {
        if !self.begin_fail_protocol(state) {
            return;
        }
        let server = self.clone();
        let st = state.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                server.finish_fail_protocol(&st, error).await;
            });
        } else {
            self.disconnect(state);
        }
    }

    fn begin_fail_protocol(&self, state: &ConnectionState) -> bool {
        let mut guard = state.inner.lock().expect("connection mutex poisoned");
        if guard.disconnected || guard.stage == ConnectionStage::Closing || guard.stage == ConnectionStage::Closed {
            return false;
        }
        guard.stage = ConnectionStage::Closing;
        if let Some(timeout) = guard.handshake_timeout.take() {
            timeout.clear();
        }
        true
    }

    async fn fail_protocol(&self, state: &Arc<ConnectionState>, error: ProtocolError) {
        if !self.begin_fail_protocol(state) {
            return;
        }
        self.finish_fail_protocol(state, error).await;
    }

    async fn finish_fail_protocol(&self, state: &Arc<ConnectionState>, error: ProtocolError) {
        let message = ServerMessage::HelloError(ServerHelloError { error });
        let final_frame = match encode_server_message(
            &message,
            Some(FrameDecoderOptions {
                max_frame_length: Some(self.inner.max_frame_length),
            }),
        ) {
            Ok(bytes) => Some(bytes),
            Err(encode_error) => {
                self.report_error(&encode_error);
                None
            }
        };
        self.close_connection(state.connection.clone(), final_frame.as_deref())
            .await;
        self.disconnect(state);
    }

    async fn close_server_state(&self) -> Result<()> {
        let connections: Vec<Arc<ConnectionState>> = {
            let guard = self.inner.connections.lock().expect("connections mutex poisoned");
            guard.values().cloned().collect()
        };
        for conn in &connections {
            let mut guard = conn.inner.lock().expect("connection mutex poisoned");
            guard.stage = ConnectionStage::Closing;
            if let Some(timeout) = guard.handshake_timeout.take() {
                timeout.clear();
            }
        }
        let close_conn_futs = connections
            .iter()
            .map(|conn| self.close_connection(conn.connection.clone(), None));
        futures::future::join_all(close_conn_futs).await;
        for conn in &connections {
            self.disconnect(conn);
        }

        let session_close_res = self.inner.sessions.close(BACKGROUND_CONTEXT.clone()).await;
        {
            let mut guard = self.inner.connections.lock().expect("connections mutex poisoned");
            guard.clear();
        }
        session_close_res
    }

    async fn close_connection(&self, connection: Arc<dyn ByteConnection>, final_chunk: Option<&[u8]>) {
        if let Err(err) = connection.close(final_chunk).await {
            self.report_error(&err);
        }
    }

    fn to_protocol_error(&self, error: &Error) -> ProtocolError {
        if let Some((code, message)) = ServerError::from_js_error(error) {
            return ProtocolError {
                code: code.as_str().to_string(),
                message: message.to_string(),
            };
        }
        if let Some(remote) = error.downcast_ref::<RemoteServiceError>() {
            let code = crate::errors::ServerOperationErrorCode::from(remote.code);
            return ProtocolError {
                code: code.as_str().to_string(),
                message: remote.message.clone(),
            };
        }
        if let Some(proto) = error.downcast_ref::<ProtocolValidationError>() {
            return ProtocolError {
                code: "invalid_request".to_string(),
                message: proto.message.clone(),
            };
        }
        self.report_error(error);
        ProtocolError {
            code: "internal_error".to_string(),
            message: INTERNAL_SERVER_ERROR_MESSAGE.to_string(),
        }
    }

    fn notify_connection_count_changed(&self) {
        if let Some(ref cb) = self.inner.on_connection_count_changed {
            let count = self.inner.connections.lock().expect("connections mutex poisoned").len();
            cb(count);
        }
    }

    fn report_error(&self, error: &Error) {
        if let Some(ref cb) = self.inner.on_error {
            cb(error);
        }
    }

    fn settle_closed(&self, error: Option<Arc<Error>>) {
        let mut guard = self.inner.lifecycle.lock().expect("lifecycle mutex poisoned");
        if guard.closed_settled {
            return;
        }
        guard.closed_settled = true;
        drop(guard);
        match error {
            None => {
                self.inner.closed_tx.send_replace(Some(Ok(())));
            }
            Some(shared) => {
                self.inner.closed_tx.send_replace(Some(Err(shared)));
            }
        }
    }
}

fn target_server_id(target: &RpcTarget) -> &str {
    match target {
        RpcTarget::Session(session) => &session.server_id,
        RpcTarget::Server(server) => &server.server_id,
    }
}

fn resolve_options(options: &ServerOptions) -> Result<(i64, i64)> {
    if !is_server_id(&Value::String(options.server_id.clone())) {
        return Err(Error::js("TypeError", "serverId must be a canonical lowercase UUIDv4"));
    }
    let max_frame_length = options.max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    if max_frame_length <= 0 || max_frame_length > MAX_UINT32 {
        return Err(Error::js(
            "TypeError",
            format!("Server maxFrameLength must be an integer between 1 and {MAX_UINT32}"),
        ));
    }
    let handshake_timeout_ms = options.handshake_timeout_ms.unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT_MS);
    if handshake_timeout_ms <= 0 || handshake_timeout_ms > MAX_TIMER_DELAY_MS {
        return Err(Error::js(
            "TypeError",
            format!("Server handshakeTimeoutMs must be an integer between 1 and {MAX_TIMER_DELAY_MS}"),
        ));
    }
    Ok((max_frame_length, handshake_timeout_ms))
}

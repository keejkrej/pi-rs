//! Port of packages/client/src/client.ts

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use indexmap::IndexMap;
use pi_chord::{
    JsonValue, RemoteServiceTransport, ServiceCall, ServiceCatalogueEntry, ServiceMode, ServiceProviderUpdate,
    ServiceStateDecoder, ServiceSubscription as ChordServiceSubscription, create_service_catalogue_call,
    create_service_state_decoder, create_service_subscribe_call, create_service_unsubscribe_call, parse_service_call,
    parse_service_catalogue, parse_wire_service_provider_update, parse_wire_service_subscription_snapshot,
};
use pi_js::abort::{AbortReason, AbortSignal, ListenerGuard};
use pi_protocol::{
    AttachmentEnvelope, CancelEnvelope, ClientMessage, FrameDecoderOptions, ProtocolValidationError, RequestEnvelope,
    ResponseEnvelope, RpcTarget, ServerHello, ServiceEventEnvelope, SessionTarget, encode_client_message, is_server_id,
};
use tokio::sync::mpsc;

use crate::connection::{Connection, ConnectionOptions, PostHandshakeMessage};
use crate::errors::{ClientDisposedError, DisconnectedError, ServerError, clone_pi_error};
use crate::promise::PromiseResolvers;
use crate::types::{
    AttachmentChangeListener, ClientOptions, ConnectionState, ConnectionStateChange, ConnectionStateListener,
    ListenerErrorHandler, ServiceSubscription, ServiceUpdateListener, Unsubscribe,
};

pub type ServiceResult = Option<JsonValue>;

type RequestResolveFn = Box<dyn FnOnce(ServiceResult) -> pi_js::Result<()> + Send>;
type RequestRejectFn = Box<dyn FnOnce(pi_js::Error) + Send>;

struct PendingRequest {
    resolve: Option<RequestResolveFn>,
    reject: Option<RequestRejectFn>,
    abort_guard: Option<ListenerGuard>,
}

enum DeliveryEvent {
    Update(ServiceProviderUpdate),
    Drain(tokio::sync::oneshot::Sender<()>),
}

struct ActiveServiceListenerInner {
    decoder: ServiceStateDecoder,
    delivery_tx: mpsc::UnboundedSender<DeliveryEvent>,
    state: Mutex<ActiveServiceListenerState>,
}

struct ActiveServiceListenerState {
    queued_wire_updates: Vec<JsonValue>,
    queued: Vec<ServiceProviderUpdate>,
    hydrated: bool,
    ready: bool,
}

#[derive(Clone)]
struct ActiveServiceListener {
    inner: Arc<ActiveServiceListenerInner>,
}

impl ActiveServiceListener {
    fn new(listener: ServiceUpdateListener, on_listener_error: Option<ListenerErrorHandler>) -> Self {
        let (delivery_tx, mut delivery_rx) = mpsc::unbounded_channel::<DeliveryEvent>();
        let listener_clone = Arc::clone(&listener);
        let error_handler = on_listener_error.clone();
        tokio::spawn(async move {
            while let Some(event) = delivery_rx.recv().await {
                match event {
                    DeliveryEvent::Update(update) => {
                        if let Err(err) = (listener_clone)(update).await {
                            report_error(error_handler.as_ref(), err);
                        }
                    }
                    DeliveryEvent::Drain(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Self {
            inner: Arc::new(ActiveServiceListenerInner {
                decoder: create_service_state_decoder(),
                delivery_tx,
                state: Mutex::new(ActiveServiceListenerState {
                    queued_wire_updates: Vec::new(),
                    queued: Vec::new(),
                    hydrated: false,
                    ready: false,
                }),
            }),
        }
    }

    fn deliver(&self, update: ServiceProviderUpdate) {
        let _ = self.inner.delivery_tx.send(DeliveryEvent::Update(update));
    }

    async fn flush_deliveries(&self) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self.inner.delivery_tx.send(DeliveryEvent::Drain(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

struct ClientState {
    pending_requests: IndexMap<String, PendingRequest>,
    connection_state_listeners: IndexMap<u64, ConnectionStateListener>,
    attachment_listeners: IndexMap<u64, AttachmentChangeListener>,
    service_listeners: IndexMap<String, ActiveServiceListener>,
    request_sequence: u64,
    service_subscription_sequence: u64,
    listener_sequence: u64,
    hello: Option<ServerHello>,
    attachment: Option<SessionTarget>,
    disposed: bool,
}

struct ClientInner {
    options: ClientOptions,
    connection: OnceLock<Connection>,
    state: Mutex<ClientState>,
}

/// RPC client over framed CBOR messages and a `ByteTransportFactory`.
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

impl Client {
    pub fn new(options: ClientOptions) -> pi_js::Result<Self> {
        if !is_server_id(&serde_json::Value::String(options.server_id.clone())) {
            return Err(pi_js::Error::js(
                "TypeError",
                "serverId must be a canonical lowercase UUIDv4",
            ));
        }

        let inner = Arc::new(ClientInner {
            options: options.clone(),
            connection: OnceLock::new(),
            state: Mutex::new(ClientState {
                pending_requests: IndexMap::new(),
                connection_state_listeners: IndexMap::new(),
                attachment_listeners: IndexMap::new(),
                service_listeners: IndexMap::new(),
                request_sequence: 0,
                service_subscription_sequence: 0,
                listener_sequence: 0,
                hello: None,
                attachment: None,
                disposed: false,
            }),
        });

        let client = Self { inner };
        let client_handshake = client.clone();
        let client_message = client.clone();
        let client_state = client.clone();

        let connection = Connection::new(ConnectionOptions {
            transport_factory: options.transport_factory,
            server_id: options.server_id,
            max_frame_length: options.max_frame_length,
            on_handshake: Arc::new(move |hello| {
                let mut state = client_handshake
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                state.hello = Some(hello.clone());
                Ok(())
            }),
            on_message: Arc::new(move |msg| client_message.handle_message(msg)),
            on_state_change: Arc::new(move |change| client_state.handle_connection_state_change(change)),
        })?;

        let _ = client.inner.connection.set(connection);
        Ok(client)
    }

    fn connection(&self) -> &Connection {
        self.inner
            .connection
            .get()
            .expect("Connection initialized in Client::new")
    }

    pub fn disposed(&self) -> bool {
        self.inner.state.lock().unwrap_or_else(PoisonError::into_inner).disposed
    }

    pub fn connection_state(&self) -> ConnectionState {
        self.connection().state()
    }

    pub fn connected(&self) -> bool {
        self.connection().state() == ConnectionState::Connected
    }

    pub fn server_id(&self) -> &str {
        &self.inner.options.server_id
    }

    pub fn hello(&self) -> Option<ServerHello> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .hello
            .clone()
    }

    pub fn attachment(&self) -> Option<SessionTarget> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attachment
            .clone()
    }

    /// Creates a client, connects cleanly, or disposes before returning any handshake error.
    pub async fn connect_options(options: ClientOptions) -> pi_js::Result<Self> {
        let client = Self::new(options)?;
        match client.connect().await {
            Ok(_) => Ok(client),
            Err(error) => {
                let _ = client.dispose().await;
                Err(error)
            }
        }
    }

    pub fn connect(&self) -> pi_js::BoxFuture<pi_js::Result<ServerHello>> {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.disposed {
                return Box::pin(async { Err(ClientDisposedError.into_pi_error()) });
            }
            state.hello = None;
        }
        self.connection().connect()
    }

    pub fn reconnect(&self) -> pi_js::BoxFuture<pi_js::Result<ServerHello>> {
        self.connect()
    }

    pub fn disconnect(&self, reason: Option<&str>) {
        self.connection().disconnect(reason.unwrap_or("Client disconnected"));
    }

    pub fn on_connection_state_change(&self, listener: ConnectionStateListener) -> pi_js::Result<Unsubscribe> {
        let id = {
            let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.disposed {
                return Err(ClientDisposedError.into_pi_error());
            }
            state.listener_sequence += 1;
            let id = state.listener_sequence;
            state.connection_state_listeners.insert(id, listener);
            id
        };
        let client = self.clone();
        Ok(Unsubscribe::new(move || {
            let mut state = client.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.connection_state_listeners.shift_remove(&id);
        }))
    }

    pub fn on_attachment_change(&self, listener: AttachmentChangeListener) -> pi_js::Result<Unsubscribe> {
        let id = {
            let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.disposed {
                return Err(ClientDisposedError.into_pi_error());
            }
            state.listener_sequence += 1;
            let id = state.listener_sequence;
            state.attachment_listeners.insert(id, listener);
            id
        };
        let client = self.clone();
        Ok(Unsubscribe::new(move || {
            let mut state = client.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.attachment_listeners.shift_remove(&id);
        }))
    }

    /// Invoke one low-level protocol call against an explicit routed target.
    pub fn request(
        &self,
        target: RpcTarget,
        call: ServiceCall,
        signal: Option<AbortSignal>,
    ) -> pi_js::BoxFuture<pi_js::Result<ServiceResult>> {
        Box::pin(self.request_with_transform(target, call, signal, |res| Ok(res)))
    }

    pub async fn service_catalogue(
        &self,
        target: RpcTarget,
        signal: Option<AbortSignal>,
    ) -> pi_js::Result<Vec<ServiceCatalogueEntry>> {
        let result = self.request(target, create_service_catalogue_call(), signal).await?;
        let val = result.unwrap_or(JsonValue::Null);
        match parse_service_catalogue(&val) {
            Ok(catalogue) => Ok(catalogue),
            Err(err) => {
                let validation_err =
                    pi_js::Error::typed("ProtocolValidationError", ProtocolValidationError::new(err.message()));
                self.connection().fail(clone_pi_error(&validation_err));
                Err(validation_err)
            }
        }
    }

    pub async fn subscribe_service(
        &self,
        target: RpcTarget,
        service_id: &str,
        mode: ServiceMode,
        listener: ServiceUpdateListener,
        signal: Option<AbortSignal>,
    ) -> pi_js::Result<ServiceSubscription> {
        let (subscription_id, active) = {
            let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.service_subscription_sequence += 1;
            let subscription_id = format!("service-{}", state.service_subscription_sequence);
            let active = ActiveServiceListener::new(listener, self.inner.options.on_listener_error.clone());
            state.service_listeners.insert(subscription_id.clone(), active.clone());
            (subscription_id, active)
        };

        let active_for_transform = active.clone();
        let subscribe_result = self
            .request_with_transform(
                target.clone(),
                create_service_subscribe_call(&subscription_id, service_id, mode),
                signal,
                move |result| {
                    let wire_val = result.unwrap_or(JsonValue::Null);
                    let wire_snapshot = parse_wire_service_subscription_snapshot(&wire_val)?;
                    let decoded = active_for_transform.inner.decoder.decode_snapshot(&wire_snapshot)?;
                    let mut st = active_for_transform
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    st.hydrated = true;
                    let queued_wire: Vec<JsonValue> = st.queued_wire_updates.drain(..).collect();
                    drop(st);
                    let mut decoded_queued = Vec::with_capacity(queued_wire.len());
                    for wire_update_val in queued_wire {
                        let wire_update = parse_wire_service_provider_update(&wire_update_val)?;
                        let update = active_for_transform.inner.decoder.decode_update(&wire_update)?;
                        decoded_queued.push(update);
                    }
                    let mut st = active_for_transform
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    st.queued.extend(decoded_queued);
                    Ok(decoded)
                },
            )
            .await;

        let snapshot = match subscribe_result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                if state
                    .service_listeners
                    .get(&subscription_id)
                    .is_some_and(|existing| Arc::ptr_eq(&existing.inner, &active.inner))
                {
                    state.service_listeners.shift_remove(&subscription_id);
                }
                return Err(error);
            }
        };

        let is_still_registered = {
            let state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            state
                .service_listeners
                .get(&subscription_id)
                .is_some_and(|existing| Arc::ptr_eq(&existing.inner, &active.inner))
        };
        if !is_still_registered {
            return Err(DisconnectedError::disconnected().into_pi_error());
        }

        let disposed = Arc::new(AtomicBool::new(false));

        let disposed_for_start = Arc::clone(&disposed);
        let active_for_start = active.clone();
        let start_fn: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if disposed_for_start.load(Ordering::Acquire) {
                return;
            }
            let queued = {
                let mut st = active_for_start
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if st.ready {
                    return;
                }
                st.ready = true;
                st.queued.drain(..).collect::<Vec<_>>()
            };
            for update in queued {
                active_for_start.deliver(update);
            }
        });

        let client_for_dispose = self.clone();
        let subscription_id_for_dispose = subscription_id.clone();
        let target_for_dispose = target.clone();
        let active_for_dispose = active;
        let disposed_for_dispose = Arc::clone(&disposed);
        let dispose_fn: Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync> = Arc::new(move || {
            let client = client_for_dispose.clone();
            let sub_id = subscription_id_for_dispose.clone();
            let target = target_for_dispose.clone();
            let active = active_for_dispose.clone();
            let disposed = Arc::clone(&disposed_for_dispose);
            Box::pin(async move {
                if disposed.swap(true, Ordering::AcqRel) {
                    return Ok(());
                }
                {
                    let mut state = client.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if state
                        .service_listeners
                        .get(&sub_id)
                        .is_some_and(|existing| Arc::ptr_eq(&existing.inner, &active.inner))
                    {
                        state.service_listeners.shift_remove(&sub_id);
                    }
                }
                let result = if client.connected() && client.target_is_current(&target) {
                    client
                        .request(target, create_service_unsubscribe_call(&sub_id), None)
                        .await
                        .map(|_| ())
                } else {
                    Ok(())
                };
                active.flush_deliveries().await;
                let mut st = active.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                st.queued_wire_updates.clear();
                st.queued.clear();
                result
            })
        });

        Ok(ServiceSubscription::new(
            subscription_id,
            target,
            snapshot,
            start_fn,
            dispose_fn,
        ))
    }

    fn request_with_transform<T, F>(
        &self,
        target: RpcTarget,
        call: ServiceCall,
        signal: Option<AbortSignal>,
        transform: F,
    ) -> impl Future<Output = pi_js::Result<T>> + Send + 'static
    where
        T: Send + 'static,
        F: FnOnce(ServiceResult) -> pi_js::Result<T> + Send + 'static,
    {
        let disposed = self.disposed();
        if disposed {
            return Box::pin(async { Err(ClientDisposedError.into_pi_error()) }) as pi_js::BoxFuture<pi_js::Result<T>>;
        }
        if !self.connected() {
            return Box::pin(async { Err(DisconnectedError::disconnected().into_pi_error()) });
        }
        if let Some(ref sig) = signal
            && let Err(err) = sig.throw_if_aborted()
        {
            return Box::pin(async move { Err(err) });
        }

        let (resolvers, promise) = PromiseResolvers::<T>::new();
        let resolvers = Arc::new(resolvers);

        let id = {
            let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.request_sequence += 1;
            format!("request-{}", state.request_sequence)
        };

        struct CancelState {
            sent: bool,
            aborted: bool,
        }
        let cancel_state = Arc::new(Mutex::new(CancelState {
            sent: false,
            aborted: false,
        }));

        let conn_for_cancel = self.connection().clone();
        let id_for_cancel = id.clone();
        let target_for_cancel = target.clone();
        let send_cancel = Arc::new(move || {
            if conn_for_cancel.state() != ConnectionState::Connected {
                return;
            }
            let frame_res = encode_client_message(
                &ClientMessage::Cancel(CancelEnvelope {
                    id: id_for_cancel.clone(),
                    target: target_for_cancel.clone(),
                }),
                Some(FrameDecoderOptions {
                    max_frame_length: Some(conn_for_cancel.max_frame_length()),
                }),
            );
            match frame_res {
                Ok(frame) => {
                    let _ = conn_for_cancel.send(frame);
                }
                Err(error) => conn_for_cancel.fail(error),
            }
        });

        let abort_guard = if let Some(ref sig) = signal {
            let cancel_state_clone = Arc::clone(&cancel_state);
            let resolvers_clone = Arc::clone(&resolvers);
            let send_cancel_clone = Arc::clone(&send_cancel);
            Some(sig.on_abort(move |reason: &AbortReason| {
                let should_send = {
                    let mut st = cancel_state_clone.lock().unwrap_or_else(PoisonError::into_inner);
                    if st.aborted {
                        return;
                    }
                    st.aborted = true;
                    st.sent
                };
                resolvers_clone.reject(pi_js::Error::Abort(reason.clone()));
                if should_send {
                    send_cancel_clone();
                }
            }))
        } else {
            None
        };

        let resolvers_for_resolve = Arc::clone(&resolvers);
        let resolvers_for_reject = Arc::clone(&resolvers);
        let conn_for_transform_err = self.connection().clone();
        let pending = PendingRequest {
            resolve: Some(Box::new(move |result| match transform(result) {
                Ok(value) => {
                    resolvers_for_resolve.resolve(value);
                    Ok(())
                }
                Err(err) => {
                    let validation_err =
                        pi_js::Error::typed("ProtocolValidationError", ProtocolValidationError::new(err.message()));
                    conn_for_transform_err.fail(clone_pi_error(&validation_err));
                    resolvers_for_resolve.reject(clone_pi_error(&validation_err));
                    Err(validation_err)
                }
            })),
            reject: Some(Box::new(move |error| {
                resolvers_for_reject.reject(error);
            })),
            abort_guard,
        };

        {
            let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.pending_requests.insert(id.clone(), pending);
        }

        let validated_call = serde_json::to_value(&call)
            .map_err(|e| pi_js::Error::msg(e.to_string()))
            .and_then(|val| parse_service_call(&val))
            .and_then(|c| serde_json::to_value(&c).map_err(|e| pi_js::Error::msg(e.to_string())));

        let frame_res = validated_call.and_then(|call_value| {
            encode_client_message(
                &ClientMessage::Request(RequestEnvelope {
                    id: id.clone(),
                    target,
                    call: call_value,
                }),
                Some(FrameDecoderOptions {
                    max_frame_length: Some(self.connection().max_frame_length()),
                }),
            )
        });

        let frame = match frame_res {
            Ok(frame) => frame,
            Err(error) => {
                if let Some(mut pending) = self.take_pending_request(&id) {
                    if let Some(reject) = pending.reject.take() {
                        reject(error);
                    }
                    drop(pending.abort_guard.take());
                }
                return Box::pin(promise);
            }
        };

        if let Err(err) = self.connection().send(frame) {
            if let Some(mut pending) = self.take_pending_request(&id) {
                if let Some(reject) = pending.reject.take() {
                    reject(err);
                }
                drop(pending.abort_guard.take());
            }
            return Box::pin(promise);
        }

        let already_aborted = {
            let mut st = cancel_state.lock().unwrap_or_else(PoisonError::into_inner);
            st.sent = true;
            st.aborted
        };
        if already_aborted {
            send_cancel();
        }

        Box::pin(promise)
    }

    fn handle_message(&self, message: PostHandshakeMessage) {
        match message {
            PostHandshakeMessage::Attachment(AttachmentEnvelope { attachment }) => {
                if let Some(ref target) = attachment
                    && target.server_id != self.inner.options.server_id
                {
                    self.connection().fail(pi_js::Error::typed(
                        "ProtocolValidationError",
                        ProtocolValidationError::new("Attachment update belongs to another server"),
                    ));
                    return;
                }
                self.set_attachment(attachment);
            }
            PostHandshakeMessage::ServiceUpdate(ServiceEventEnvelope {
                subscription_id,
                update,
            }) => {
                let active = {
                    let state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                    state.service_listeners.get(&subscription_id).cloned()
                };
                let Some(active) = active else {
                    return;
                };
                let hydrated = {
                    let mut st = active.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if !st.hydrated {
                        st.queued_wire_updates.push(update);
                        return;
                    }
                    true
                };
                if hydrated {
                    let decoded = parse_wire_service_provider_update(&update)
                        .and_then(|wire| active.inner.decoder.decode_update(&wire));
                    match decoded {
                        Ok(decoded_update) => {
                            let mut st = active.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                            if st.ready {
                                drop(st);
                                active.deliver(decoded_update);
                            } else {
                                st.queued.push(decoded_update);
                            }
                        }
                        Err(err) => {
                            self.connection().fail(pi_js::Error::typed(
                                "ProtocolValidationError",
                                ProtocolValidationError::new(err.message()),
                            ));
                        }
                    }
                }
            }
            PostHandshakeMessage::Response(ResponseEnvelope { id, ok, result, error }) => {
                let Some(mut pending) = self.take_pending_request(&id) else {
                    self.connection().fail(pi_js::Error::typed(
                        "ProtocolValidationError",
                        ProtocolValidationError::new("Response has no matching request"),
                    ));
                    return;
                };
                // Drop abort listener guard outside lock before resolving/rejecting.
                drop(pending.abort_guard.take());
                if !ok {
                    let server_err = error.map_or_else(
                        || pi_js::Error::msg("Unknown server error"),
                        |e| ServerError::new(e).into_pi_error(),
                    );
                    if let Some(reject) = pending.reject.take() {
                        reject(server_err);
                    }
                    return;
                }
                if let Some(resolve) = pending.resolve.take() {
                    let _ = resolve(result);
                }
            }
        }
    }

    fn handle_connection_state_change(&self, change: ConnectionStateChange) {
        let listeners = if change.state == ConnectionState::Disconnected {
            let (pending, listeners, old_attachment, attach_listeners) = {
                let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.hello = None;
                let attachment_changed = state.attachment.is_some();
                let old_attachment = if attachment_changed {
                    state.attachment.take()
                } else {
                    None
                };
                let attach_listeners = if attachment_changed {
                    state.attachment_listeners.values().cloned().collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let pending = state.pending_requests.drain(..).map(|(_, v)| v).collect::<Vec<_>>();
                state.service_listeners.clear();
                let listeners = state.connection_state_listeners.values().cloned().collect::<Vec<_>>();
                (pending, listeners, old_attachment, attach_listeners)
            };

            for mut req in pending {
                drop(req.abort_guard.take());
                if let Some(reject) = req.reject.take() {
                    let err = match &change.error {
                        Some(shared) => clone_pi_error(shared),
                        None => DisconnectedError::disconnected().into_pi_error(),
                    };
                    reject(err);
                }
            }

            if old_attachment.is_some() {
                for listener in attach_listeners {
                    if let Err(err) = listener(None) {
                        report_error(self.inner.options.on_listener_error.as_ref(), err);
                    }
                }
            }
            listeners
        } else {
            let state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.connection_state_listeners.values().cloned().collect::<Vec<_>>()
        };

        for listener in listeners {
            if let Err(err) = listener(change.clone()) {
                report_error(self.inner.options.on_listener_error.as_ref(), err);
            }
        }
    }

    fn take_pending_request(&self, id: &str) -> Option<PendingRequest> {
        let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.pending_requests.shift_remove(id)
    }

    pub async fn dispose(&self) -> pi_js::Result<()> {
        let (pending, attachment_changed, attach_listeners) = {
            let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.disposed {
                return Ok(());
            }
            state.disposed = true;
            let pending = state.pending_requests.drain(..).map(|(_, v)| v).collect::<Vec<_>>();
            state.hello = None;
            let attachment_changed = state.attachment.take().is_some();
            let attach_listeners = if attachment_changed {
                state.attachment_listeners.values().cloned().collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            state.connection_state_listeners.clear();
            state.attachment_listeners.clear();
            state.service_listeners.clear();
            (pending, attachment_changed, attach_listeners)
        };

        for mut req in pending {
            drop(req.abort_guard.take());
            if let Some(reject) = req.reject.take() {
                reject(ClientDisposedError.into_pi_error());
            }
        }

        self.connection().disconnect(ClientDisposedError.into_pi_error());

        if attachment_changed {
            for listener in attach_listeners {
                if let Err(err) = listener(None) {
                    report_error(self.inner.options.on_listener_error.as_ref(), err);
                }
            }
        }

        Ok(())
    }

    fn set_attachment(&self, attachment: Option<SessionTarget>) {
        let listeners = {
            let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.attachment == attachment {
                return;
            }
            state.attachment = attachment.clone();
            state.attachment_listeners.values().cloned().collect::<Vec<_>>()
        };

        for listener in listeners {
            if let Err(err) = listener(attachment.clone()) {
                report_error(self.inner.options.on_listener_error.as_ref(), err);
            }
        }
    }

    fn target_is_current(&self, target: &RpcTarget) -> bool {
        let state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        match target {
            RpcTarget::Server(server) => state.hello.as_ref().is_some_and(|h| h.server_id == server.server_id),
            RpcTarget::Session(session) => state.attachment.as_ref().is_some_and(|a| a == session),
        }
    }
}

fn report_error(handler: Option<&ListenerErrorHandler>, error: pi_js::Error) {
    if let Some(cb) = handler {
        cb(error);
    }
}

/// Adapts a lazily resolved routed client target to a Chord service transport.
pub fn create_client_service_transport<F>(client: &Client, get_target: F) -> RemoteServiceTransport
where
    F: Fn() -> Option<RpcTarget> + Send + Sync + 'static,
{
    let target_getter = Arc::new(get_target);
    let resolve_target = Arc::new(move || -> pi_js::Result<RpcTarget> {
        target_getter().ok_or_else(|| pi_js::Error::msg("Remote service target is unavailable"))
    });

    let client_for_invoke = client.clone();
    let resolve_for_invoke = Arc::clone(&resolve_target);
    let invoke = Arc::new(
        move |call: &ServiceCall, context: &pi_chord::Context| -> pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>> {
            let client = client_for_invoke.clone();
            let resolve = Arc::clone(&resolve_for_invoke);
            let call = call.clone();
            let signal = context.abort_signal();
            Box::pin(async move {
                let target = resolve()?;
                client.request(target, call, signal).await
            })
        },
    );

    let client_for_subscribe = client.clone();
    let resolve_for_subscribe = resolve_target;
    let subscribe = Arc::new(
        move |service_id: &str,
              mode: ServiceMode,
              listener: Arc<dyn Fn(ServiceProviderUpdate, pi_chord::Context) + Send + Sync>,
              context: &pi_chord::Context|
              -> pi_js::BoxFuture<pi_js::Result<ChordServiceSubscription>> {
            let client = client_for_subscribe.clone();
            let resolve = Arc::clone(&resolve_for_subscribe);
            let service_id = service_id.to_string();
            let signal = context.abort_signal();
            Box::pin(async move {
                let target = resolve()?;
                let wrapped_listener: ServiceUpdateListener = Arc::new(move |update| {
                    let listener = Arc::clone(&listener);
                    Box::pin(async move {
                        listener(update, pi_chord::context::BACKGROUND_CONTEXT.clone());
                        Ok(())
                    })
                });
                let sub = client
                    .subscribe_service(target, &service_id, mode, wrapped_listener, signal)
                    .await?;
                let snapshot = sub.snapshot().clone();
                let sub_for_activate = sub.clone();
                let sub_for_close = sub;
                Ok(ChordServiceSubscription::new(
                    snapshot,
                    Arc::new(move || {
                        sub_for_activate.start();
                        Ok(())
                    }),
                    Arc::new(move |_ctx| {
                        let sub = sub_for_close.clone();
                        Box::pin(async move { sub.dispose().await })
                    }),
                ))
            })
        },
    );

    RemoteServiceTransport::new(invoke, subscribe)
}

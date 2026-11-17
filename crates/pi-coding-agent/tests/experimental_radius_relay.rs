//! Port of packages/coding-agent/test/experimental-radius-relay.test.ts

use std::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_client::{ByteTransportHandlers, ConnectionState};
use pi_coding_agent::cli::experimental::command_options::AuthInput;
use pi_coding_agent::experimental::radius_auth::RadiusRelayAuthResolver;
use pi_coding_agent::experimental::radius_relay::{
    RadiusClientReconnect, RadiusClientTransportFactoryOptions, RadiusReconnectTarget, RadiusRelayHost,
    RadiusRelayHostOptions, RadiusRelayHostStatus, RadiusRelayMessage, RadiusRelayWebSocket,
    RadiusRelayWebSocketConnectOptions, RadiusRelayWebSocketFactory, RadiusRelayWebSocketHandlers,
    RelayServerAcceptTarget, WS_STATE_CLOSED, WS_STATE_CONNECTING, WS_STATE_OPEN,
    create_radius_client_transport_factory, encode_relay_data_frame, parse_relay_data_frame,
};
use pi_js::Unsubscribe;
use pi_server::{ByteConnection, ByteConnectionHandler};
use serde_json::json;

const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";
const CONNECTION_ID: &str = "00000000-0000-4000-8000-000000000002";

#[derive(Clone)]
struct FakeWebSocket {
    inner: Arc<FakeWebSocketInner>,
}

struct FakeWebSocketInner {
    sent_text: Mutex<Vec<String>>,
    sent_binary: Mutex<Vec<Vec<u8>>>,
    listeners: Mutex<Vec<(u64, RadiusRelayWebSocketHandlers)>>,
    next_id: AtomicU64,
    protocol: Mutex<String>,
    ready_state: AtomicU16,
}

impl FakeWebSocket {
    fn new() -> Self {
        Self {
            inner: Arc::new(FakeWebSocketInner {
                sent_text: Mutex::new(Vec::new()),
                sent_binary: Mutex::new(Vec::new()),
                listeners: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
                protocol: Mutex::new(String::new()),
                ready_state: AtomicU16::new(WS_STATE_CONNECTING),
            }),
        }
    }

    fn snapshot_listeners(&self) -> Vec<RadiusRelayWebSocketHandlers> {
        self.inner
            .listeners
            .lock()
            .expect("lock")
            .iter()
            .map(|(_, h)| h.clone())
            .collect()
    }

    fn open(&self, protocol: &str) {
        *self.inner.protocol.lock().expect("lock") = protocol.to_string();
        self.inner.ready_state.store(WS_STATE_OPEN, Ordering::Release);
        for h in self.snapshot_listeners() {
            if let Some(ref on_open) = h.on_open {
                on_open();
            }
        }
    }

    fn message_text(&self, text: String) {
        for h in self.snapshot_listeners() {
            if let Some(ref on_message) = h.on_message {
                on_message(RadiusRelayMessage::Text(text.clone()));
            }
        }
    }

    fn message_binary(&self, bytes: Vec<u8>) {
        for h in self.snapshot_listeners() {
            if let Some(ref on_message) = h.on_message {
                on_message(RadiusRelayMessage::Binary(bytes.clone()));
            }
        }
    }

    fn remote_close(&self, code: u16, reason: &str) {
        self.inner.ready_state.store(WS_STATE_CLOSED, Ordering::Release);
        for h in self.snapshot_listeners() {
            if let Some(ref on_close) = h.on_close {
                on_close(code, reason.to_string());
            }
        }
    }

    fn fail(&self, error: pi_js::Error) {
        for h in self.snapshot_listeners() {
            if let Some(ref on_error) = h.on_error {
                on_error(pi_js::Error::msg(error.message()));
            }
        }
    }

    fn abnormal_close(&self, error: pi_js::Error) {
        self.inner.ready_state.store(WS_STATE_CLOSED, Ordering::Release);
        let msg = error.message().to_string();
        for h in self.snapshot_listeners() {
            if let Some(ref on_error) = h.on_error {
                on_error(pi_js::Error::msg(&msg));
            }
        }
        for h in self.snapshot_listeners() {
            if let Some(ref on_close) = h.on_close {
                on_close(1006, String::new());
            }
        }
    }

    fn sent_binary(&self) -> Vec<Vec<u8>> {
        self.inner.sent_binary.lock().expect("lock").clone()
    }
}

impl RadiusRelayWebSocket for FakeWebSocket {
    fn protocol(&self) -> String {
        self.inner.protocol.lock().expect("lock").clone()
    }

    fn ready_state(&self) -> u16 {
        self.inner.ready_state.load(Ordering::Acquire)
    }

    fn buffered_amount(&self) -> usize {
        0
    }

    fn send_text(&self, data: String) -> pi_js::Result<()> {
        if self.ready_state() != WS_STATE_OPEN {
            return Err(pi_js::Error::msg("socket is not open"));
        }
        self.inner.sent_text.lock().expect("lock").push(data);
        Ok(())
    }

    fn send_binary(&self, data: Vec<u8>) -> pi_js::Result<()> {
        if self.ready_state() != WS_STATE_OPEN {
            return Err(pi_js::Error::msg("socket is not open"));
        }
        self.inner.sent_binary.lock().expect("lock").push(data);
        Ok(())
    }

    fn close(&self, code: Option<u16>, reason: Option<&str>) -> pi_js::Result<()> {
        let code = code.unwrap_or(1000);
        if code != 1000 && !(3000..=4999).contains(&code) {
            return Err(pi_js::Error::msg("Invalid close code"));
        }
        if self.inner.ready_state.swap(WS_STATE_CLOSED, Ordering::AcqRel) == WS_STATE_CLOSED {
            return Ok(());
        }
        let reason_str = reason.unwrap_or("").to_string();
        for h in self.snapshot_listeners() {
            if let Some(ref on_close) = h.on_close {
                on_close(code, reason_str.clone());
            }
        }
        Ok(())
    }

    fn subscribe(&self, handlers: RadiusRelayWebSocketHandlers) -> Unsubscribe {
        let id = self.inner.next_id.fetch_add(1, Ordering::AcqRel);
        self.inner.listeners.lock().expect("lock").push((id, handlers));
        let inner = Arc::clone(&self.inner);
        Unsubscribe::new(move || {
            inner
                .listeners
                .lock()
                .expect("lock")
                .retain(|(entry_id, _)| *entry_id != id);
        })
    }
}

struct RecordedSocket {
    socket: FakeWebSocket,
    options: RadiusRelayWebSocketConnectOptions,
}

fn socket_factory() -> (Arc<Mutex<Vec<RecordedSocket>>>, RadiusRelayWebSocketFactory) {
    let sockets = Arc::new(Mutex::new(Vec::new()));
    let sockets_ref = Arc::clone(&sockets);
    let factory: RadiusRelayWebSocketFactory = Arc::new(move |options| {
        let socket = FakeWebSocket::new();
        sockets_ref.lock().expect("lock").push(RecordedSocket {
            socket: socket.clone(),
            options,
        });
        Arc::new(socket)
    });
    (sockets, factory)
}

async fn yield_tasks() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

async fn wait_for<F>(mut pred: F)
where
    F: FnMut() -> bool,
{
    for _ in 0..200 {
        if pred() {
            return;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(pred(), "timed out waiting for condition");
}

struct FakeByteHandler {
    received: Arc<Mutex<Vec<Vec<u8>>>>,
    closed: Arc<AtomicUsize>,
}

impl ByteConnectionHandler for FakeByteHandler {
    fn on_data(&self, chunk: &[u8]) {
        self.received.lock().expect("lock").push(chunk.to_vec());
    }

    fn on_close(&self) {
        self.closed.fetch_add(1, Ordering::AcqRel);
    }

    fn on_error(&self, _error: pi_js::Error) {}
}

struct FakeRelayServer {
    accepted: Mutex<Option<Arc<dyn ByteConnection>>>,
    received: Arc<Mutex<Vec<Vec<u8>>>>,
    closed: Arc<AtomicUsize>,
}

impl FakeRelayServer {
    fn new() -> Self {
        Self {
            accepted: Mutex::new(None),
            received: Arc::new(Mutex::new(Vec::new())),
            closed: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl RelayServerAcceptTarget for FakeRelayServer {
    fn accept_connection(&self, connection: Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> {
        *self.accepted.lock().expect("lock") = Some(connection);
        Arc::new(FakeByteHandler {
            received: Arc::clone(&self.received),
            closed: Arc::clone(&self.closed),
        })
    }
}

struct FakeReconnectTarget {
    connected: Mutex<bool>,
    state: Mutex<ConnectionState>,
    attachment: Mutex<Option<String>>,
    connection_listeners: Mutex<Vec<(u64, Arc<dyn Fn(ConnectionState) + Send + Sync>)>>,
    attachment_listeners: Mutex<Vec<(u64, Arc<dyn Fn(Option<String>) + Send + Sync>)>>,
    next_id: AtomicU64,
    attempts: AtomicUsize,
}

impl FakeReconnectTarget {
    fn new(session_id: Option<&str>) -> Self {
        Self {
            connected: Mutex::new(true),
            state: Mutex::new(ConnectionState::Connected),
            attachment: Mutex::new(session_id.map(ToString::to_string)),
            connection_listeners: Mutex::new(Vec::new()),
            attachment_listeners: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            attempts: AtomicUsize::new(0),
        }
    }
}

impl RadiusReconnectTarget for FakeReconnectTarget {
    fn attachment_session_id(&self) -> Option<String> {
        self.attachment.lock().expect("lock").clone()
    }

    fn connected(&self) -> bool {
        *self.connected.lock().expect("lock")
    }

    fn connection_state(&self) -> ConnectionState {
        *self.state.lock().expect("lock")
    }

    fn disconnect(&self, _reason: Option<&str>) {
        *self.connected.lock().expect("lock") = false;
        *self.state.lock().expect("lock") = ConnectionState::Disconnected;
        let listeners: Vec<_> = self
            .connection_listeners
            .lock()
            .expect("lock")
            .iter()
            .map(|(_, l)| Arc::clone(l))
            .collect();
        for l in listeners {
            l(ConnectionState::Disconnected);
        }
    }

    fn on_attachment_change(&self, listener: Arc<dyn Fn(Option<String>) + Send + Sync>) -> pi_js::Result<Unsubscribe> {
        let id = self.next_id.fetch_add(1, Ordering::AcqRel);
        self.attachment_listeners.lock().expect("lock").push((id, listener));
        Ok(Unsubscribe::new(|| {}))
    }

    fn on_connection_state_change(
        &self,
        listener: Arc<dyn Fn(ConnectionState) + Send + Sync>,
    ) -> pi_js::Result<Unsubscribe> {
        let id = self.next_id.fetch_add(1, Ordering::AcqRel);
        self.connection_listeners.lock().expect("lock").push((id, listener));
        Ok(Unsubscribe::new(|| {}))
    }

    fn reconnect(&self) -> futures::future::BoxFuture<'_, pi_js::Result<()>> {
        let attempt = self.attempts.fetch_add(1, Ordering::AcqRel) + 1;
        Box::pin(async move {
            if attempt == 1 {
                return Err(pi_js::Error::msg("temporary failure"));
            }
            *self.connected.lock().expect("lock") = true;
            *self.state.lock().expect("lock") = ConnectionState::Connected;
            let listeners: Vec<_> = self
                .connection_listeners
                .lock()
                .expect("lock")
                .iter()
                .map(|(_, l)| Arc::clone(l))
                .collect();
            for l in listeners {
                l(ConnectionState::Connected);
            }
            Ok(())
        })
    }
}

#[test]
fn matches_the_radius_multiplexing_envelope() {
    let payload = [0u8, 1, 2, 255];
    let frame = encode_relay_data_frame(CONNECTION_ID, &payload).expect("encode");
    let parsed = parse_relay_data_frame(&frame).expect("parse");

    assert_eq!(parsed.connection_id, CONNECTION_ID);
    assert_eq!(parsed.payload, payload);
}

#[tokio::test]
async fn bridges_multiplexed_host_clients_into_independent_server_connections() {
    let (sockets, factory) = socket_factory();
    let server = Arc::new(FakeRelayServer::new());
    let statuses = Arc::new(Mutex::new(Vec::<RadiusRelayHostStatus>::new()));
    let statuses_ref = Arc::clone(&statuses);
    let host = RadiusRelayHost::new(RadiusRelayHostOptions {
        server_id: SERVER_ID.to_string(),
        server: Arc::clone(&server) as Arc<dyn RelayServerAcceptTarget>,
        auth: Arc::new(RadiusRelayAuthResolver::new(
            Some(AuthInput::Token {
                token: "secret".to_string(),
            }),
            None,
        )),
        web_socket_factory: Some(factory),
        on_status: Some(Arc::new(move |status| {
            statuses_ref.lock().expect("lock").push(status);
        })),
    });

    host.start();
    wait_for(|| sockets.lock().expect("lock").len() == 1).await;
    let (socket, authorization, protocol) = {
        let guard = sockets.lock().expect("lock");
        (
            guard[0].socket.clone(),
            guard[0].options.authorization.clone(),
            guard[0].options.protocol.clone(),
        )
    };
    assert_eq!(authorization, "Bearer secret");
    socket.open(&protocol);
    wait_for(|| {
        statuses
            .lock()
            .expect("lock")
            .contains(&RadiusRelayHostStatus::Connected)
    })
    .await;

    let open_msg = pi_js::json::stringify(&json!({
        "version": 1,
        "type": "connection_open",
        "connection_id": CONNECTION_ID,
    }));
    socket.message_text(open_msg);

    let accepted = server
        .accepted
        .lock()
        .expect("lock")
        .clone()
        .expect("server accepted connection");
    let data_frame = encode_relay_data_frame(CONNECTION_ID, &[1, 2, 3]).expect("encode frame");
    socket.message_binary(data_frame);
    assert_eq!(*server.received.lock().expect("lock"), vec![vec![1u8, 2, 3]]);

    accepted.send(&[4, 5, 6]).await.expect("send outbound");
    wait_for(|| !socket.sent_binary().is_empty()).await;
    let last_binary = socket.sent_binary().last().cloned().expect("outbound frame");
    let outbound = parse_relay_data_frame(&last_binary).expect("parse outbound");
    assert_eq!(outbound.connection_id, CONNECTION_ID);
    assert_eq!(outbound.payload, vec![4u8, 5, 6]);

    let close_msg = pi_js::json::stringify(&json!({
        "version": 1,
        "type": "connection_close",
        "connection_id": CONNECTION_ID,
    }));
    socket.message_text(close_msg);
    assert_eq!(server.closed.load(Ordering::Acquire), 1);
    host.close().await.expect("close host");
}

#[tokio::test(start_paused = true)]
async fn reconnects_the_server_host_after_the_relay_connection_drops() {
    let (sockets, factory) = socket_factory();
    let server = Arc::new(FakeRelayServer::new());
    let statuses = Arc::new(Mutex::new(Vec::<RadiusRelayHostStatus>::new()));
    let statuses_ref = Arc::clone(&statuses);
    let host = RadiusRelayHost::new(RadiusRelayHostOptions {
        server_id: SERVER_ID.to_string(),
        server,
        auth: Arc::new(RadiusRelayAuthResolver::new(
            Some(AuthInput::Token {
                token: "secret".to_string(),
            }),
            None,
        )),
        web_socket_factory: Some(factory),
        on_status: Some(Arc::new(move |status| {
            statuses_ref.lock().expect("lock").push(status);
        })),
    });

    host.start();
    yield_tasks().await;
    assert_eq!(sockets.lock().expect("lock").len(), 1);
    let (socket0, protocol0) = {
        let guard = sockets.lock().expect("lock");
        (guard[0].socket.clone(), guard[0].options.protocol.clone())
    };
    socket0.open(&protocol0);
    yield_tasks().await;
    assert!(
        statuses
            .lock()
            .expect("lock")
            .contains(&RadiusRelayHostStatus::Connected)
    );

    socket0.remote_close(1006, "lost");
    yield_tasks().await;
    tokio::time::advance(Duration::from_millis(1_000)).await;
    yield_tasks().await;
    assert_eq!(sockets.lock().expect("lock").len(), 2);
    host.close().await.expect("close host");
}

#[tokio::test]
async fn uses_a_raw_authenticated_websocket_as_the_client_byte_transport() {
    let (sockets, factory) = socket_factory();
    let received = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let close_count = Arc::new(AtomicUsize::new(0));
    let error_count = Arc::new(AtomicUsize::new(0));

    let factory_fn = create_radius_client_transport_factory(RadiusClientTransportFactoryOptions {
        server_id: SERVER_ID.to_string(),
        auth: Arc::new(RadiusRelayAuthResolver::new(
            Some(AuthInput::Token {
                token: "secret".to_string(),
            }),
            None,
        )),
        web_socket_factory: Some(factory),
    });

    let handlers = ByteTransportHandlers::new(
        {
            let received = Arc::clone(&received);
            move |chunk| received.lock().expect("lock").push(chunk.to_vec())
        },
        {
            let close_count = Arc::clone(&close_count);
            move || {
                close_count.fetch_add(1, Ordering::AcqRel);
            }
        },
        {
            let error_count = Arc::clone(&error_count);
            move |_| {
                error_count.fetch_add(1, Ordering::AcqRel);
            }
        },
    );

    let opening = tokio::spawn(async move { factory_fn(handlers).await });
    wait_for(|| sockets.lock().expect("lock").len() == 1).await;
    let (socket, authorization, protocol) = {
        let guard = sockets.lock().expect("lock");
        (
            guard[0].socket.clone(),
            guard[0].options.authorization.clone(),
            guard[0].options.protocol.clone(),
        )
    };
    assert_eq!(authorization, "Bearer secret");
    socket.open(&protocol);
    let transport = opening.await.expect("join").expect("transport opened");

    transport.send(vec![1, 2, 3]).await.expect("send bytes");
    wait_for(|| !socket.sent_binary().is_empty()).await;
    assert_eq!(socket.sent_binary(), vec![vec![1u8, 2, 3]]);

    socket.message_binary(vec![4, 5, 6]);
    assert_eq!(*received.lock().expect("lock"), vec![vec![4u8, 5, 6]]);

    socket.remote_close(1006, "lost");
    assert_eq!(close_count.load(Ordering::Acquire), 1);
    assert_eq!(error_count.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn reports_established_abnormal_closures_so_the_client_can_reconnect() {
    let (sockets, factory) = socket_factory();
    let close_count = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));

    let factory_fn = create_radius_client_transport_factory(RadiusClientTransportFactoryOptions {
        server_id: SERVER_ID.to_string(),
        auth: Arc::new(RadiusRelayAuthResolver::new(
            Some(AuthInput::Token {
                token: "secret".to_string(),
            }),
            None,
        )),
        web_socket_factory: Some(factory),
    });

    let handlers = ByteTransportHandlers::new(
        |_| {},
        {
            let close_count = Arc::clone(&close_count);
            move || {
                close_count.fetch_add(1, Ordering::AcqRel);
            }
        },
        {
            let errors = Arc::clone(&errors);
            move |err| errors.lock().expect("lock").push(err.message().to_string())
        },
    );

    let opening = tokio::spawn(async move { factory_fn(handlers).await });
    wait_for(|| sockets.lock().expect("lock").len() == 1).await;
    let (socket, protocol) = {
        let guard = sockets.lock().expect("lock");
        (guard[0].socket.clone(), guard[0].options.protocol.clone())
    };
    socket.open(&protocol);
    let _transport = opening.await.expect("join").expect("transport opened");

    socket.abnormal_close(pi_js::Error::msg("network lost"));
    assert_eq!(*errors.lock().expect("lock"), vec!["network lost".to_string()]);
    assert_eq!(close_count.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn reports_a_useful_error_when_undici_omits_websocket_failure_details() {
    let (sockets, factory) = socket_factory();
    let factory_fn = create_radius_client_transport_factory(RadiusClientTransportFactoryOptions {
        server_id: SERVER_ID.to_string(),
        auth: Arc::new(RadiusRelayAuthResolver::new(
            Some(AuthInput::Token {
                token: "secret".to_string(),
            }),
            None,
        )),
        web_socket_factory: Some(factory),
    });

    let opening = tokio::spawn(async move { factory_fn(ByteTransportHandlers::new(|_| {}, || {}, |_| {})).await });
    wait_for(|| sockets.lock().expect("lock").len() == 1).await;
    let socket = sockets.lock().expect("lock")[0].socket.clone();
    socket.fail(pi_js::Error::msg(""));

    let err = opening
        .await
        .expect("join")
        .expect_err("must report connection failure");
    assert!(
        err.message().contains("Radius WebSocket connection failed"),
        "unexpected error: {}",
        err.message()
    );
}

#[tokio::test(start_paused = true)]
async fn reconnects_an_established_client_and_restores_its_selected_session() {
    let target = Arc::new(FakeReconnectTarget::new(Some("demo-1")));
    let reattached = Arc::new(Mutex::new(Vec::<String>::new()));
    let reattached_ref = Arc::clone(&reattached);
    let reconnect = RadiusClientReconnect::new(
        Arc::clone(&target) as Arc<dyn RadiusReconnectTarget>,
        Arc::new(move |session_id| {
            let reattached = Arc::clone(&reattached_ref);
            Box::pin(async move {
                reattached.lock().expect("lock").push(session_id);
                Ok(())
            })
        }),
    )
    .expect("create reconnector");

    target.disconnect(None);
    yield_tasks().await;

    tokio::time::advance(Duration::from_millis(1_000)).await;
    yield_tasks().await;
    assert_eq!(target.attempts.load(Ordering::Acquire), 1);

    tokio::time::advance(Duration::from_millis(2_000)).await;
    yield_tasks().await;
    assert_eq!(target.attempts.load(Ordering::Acquire), 2);
    assert_eq!(*reattached.lock().expect("lock"), vec!["demo-1".to_string()]);
    reconnect.dispose().await.expect("dispose reconnect");
}

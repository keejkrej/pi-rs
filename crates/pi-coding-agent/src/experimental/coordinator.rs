//! Port of packages/coding-agent/src/experimental/coordinator.ts

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use indexmap::{IndexMap, IndexSet};
use pi_js::Unsubscribe;
use pi_js::crypto::random_uuid;
use pi_js::fs::promises::{chmod, lstat, unlink};
use pi_js::json::parse;
use pi_js::time::now_ms;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, mpsc, oneshot, watch};

use crate::experimental::process::{
    InternalProcessRole, InternalProcessSpawnOptions, MAX_CONTROL_LINE_BYTES, encode_control_line,
    spawn_internal_process,
};

pub const COORDINATOR_PROTOCOL_VERSION: u32 = 3;
const COORDINATOR_START_TIMEOUT_MS: i64 = 10_000;
const COORDINATOR_RETRY_MS: u64 = 10;
const EMPTY_STARTUP_GRACE_MS: u64 = 30_000;
const EMPTY_SHUTDOWN_GRACE_MS: u64 = 250;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoordinatorConnectionEvent {
    #[serde(rename_all = "camelCase")]
    PeerConnected {
        peer_id: String,
    },
    #[serde(rename_all = "camelCase")]
    PeerDisconnected {
        peer_id: String,
    },
    Message {
        from: String,
        payload: Value,
    },
}

#[derive(Clone, Debug)]
pub struct CoordinatorConnectionOptions {
    pub control_path: String,
    pub endpoint: String,
    pub server_connection_id: Option<String>,
}

enum CoordinatorMessage {
    ServerRegistered {
        server_connection_id: String,
        peers: Vec<String>,
    },
    ServerReplaced,
    PeerConnected {
        peer_id: String,
    },
    PeerDisconnected {
        peer_id: String,
    },
    Message {
        from: String,
        payload: Value,
    },
}

fn parse_coordinator_message(value: &Value) -> Option<CoordinatorMessage> {
    let obj = value.as_object()?;
    match obj.get("type").and_then(Value::as_str)? {
        "server_registered" => {
            let server_connection_id = obj.get("serverConnectionId").and_then(Value::as_str)?.to_string();
            let peers_val = obj.get("peers").and_then(Value::as_array)?;
            let mut peers = Vec::with_capacity(peers_val.len());
            for item in peers_val {
                peers.push(item.as_str()?.to_string());
            }
            Some(CoordinatorMessage::ServerRegistered {
                server_connection_id,
                peers,
            })
        }
        "server_replaced" => Some(CoordinatorMessage::ServerReplaced),
        "peer_connected" => Some(CoordinatorMessage::PeerConnected {
            peer_id: obj.get("peerId").and_then(Value::as_str)?.to_string(),
        }),
        "peer_disconnected" => Some(CoordinatorMessage::PeerDisconnected {
            peer_id: obj.get("peerId").and_then(Value::as_str)?.to_string(),
        }),
        "message" => Some(CoordinatorMessage::Message {
            from: obj.get("from").and_then(Value::as_str)?.to_string(),
            payload: obj.get("payload")?.clone(),
        }),
        _ => None,
    }
}

pub type CoordinatorListener = Arc<dyn Fn(CoordinatorConnectionEvent) + Send + Sync>;

struct ConnectionState {
    peer_ids: IndexSet<String>,
    listeners: IndexMap<u64, CoordinatorListener>,
    next_listener_id: u64,
    connected: bool,
    registered: bool,
    closed: bool,
    replaced: bool,
    writer_tx: Option<mpsc::UnboundedSender<String>>,
    pending_registered: Option<oneshot::Sender<pi_js::Result<()>>>,
}

/// The server-side endpoint of the coordinator's intentionally opaque message router.
#[derive(Clone)]
pub struct CoordinatorConnection {
    inner: Arc<CoordinatorConnectionInner>,
}

struct CoordinatorConnectionInner {
    server_connection_id: String,
    control_path: String,
    endpoint: String,
    state: Mutex<ConnectionState>,
    replaced_tx: watch::Sender<bool>,
}

impl CoordinatorConnection {
    pub fn new(options: CoordinatorConnectionOptions) -> Self {
        let (replaced_tx, _replaced_rx) = watch::channel(false);
        Self {
            inner: Arc::new(CoordinatorConnectionInner {
                server_connection_id: options.server_connection_id.unwrap_or_else(random_uuid),
                control_path: options.control_path,
                endpoint: options.endpoint,
                state: Mutex::new(ConnectionState {
                    peer_ids: IndexSet::new(),
                    listeners: IndexMap::new(),
                    next_listener_id: 1,
                    connected: false,
                    registered: false,
                    closed: false,
                    replaced: false,
                    writer_tx: None,
                    pending_registered: None,
                }),
                replaced_tx,
            }),
        }
    }

    pub fn server_connection_id(&self) -> &str {
        &self.inner.server_connection_id
    }

    pub fn control_path(&self) -> &str {
        &self.inner.control_path
    }

    pub fn was_replaced(&self) -> bool {
        lock(&self.inner.state).replaced
    }

    pub fn peer_ids(&self) -> IndexSet<String> {
        lock(&self.inner.state).peer_ids.clone()
    }

    pub async fn replaced(&self) {
        let mut rx = self.inner.replaced_tx.subscribe();
        if *rx.borrow() {
            return;
        }
        while rx.changed().await.is_ok() {
            if *rx.borrow() {
                return;
            }
        }
    }

    pub fn on_event(&self, listener: CoordinatorListener) -> Unsubscribe {
        let id = {
            let mut state = lock(&self.inner.state);
            let id = state.next_listener_id;
            state.next_listener_id += 1;
            state.listeners.insert(id, listener);
            id
        };
        let inner = Arc::clone(&self.inner);
        Unsubscribe::new(move || {
            lock(&inner.state).listeners.shift_remove(&id);
        })
    }

    pub async fn connect(&self) -> pi_js::Result<()> {
        {
            let state = lock(&self.inner.state);
            if state.connected {
                return Err(pi_js::Error::msg("Coordinator server is already connected"));
            }
        }
        let stream = UnixStream::connect(&self.inner.control_path)
            .await
            .map_err(|e| pi_js::Error::from_io(e, "connectCoordinator", Some(&self.inner.control_path), None))?;
        let (read_half, mut write_half) = stream.into_split();
        let (writer_tx, mut writer_rx) = mpsc::unbounded_channel::<String>();
        let (reg_tx, reg_rx) = oneshot::channel();

        {
            let mut state = lock(&self.inner.state);
            if state.closed {
                return Err(pi_js::Error::msg("Coordinator server closed"));
            }
            state.connected = true;
            state.writer_tx = Some(writer_tx.clone());
            state.pending_registered = Some(reg_tx);
        }

        let mut register_msg = Map::new();
        register_msg.insert("type".to_string(), Value::String("register_server".to_string()));
        register_msg.insert("protocol".to_string(), Value::from(COORDINATOR_PROTOCOL_VERSION));
        register_msg.insert(
            "serverConnectionId".to_string(),
            Value::String(self.inner.server_connection_id.clone()),
        );
        register_msg.insert("endpoint".to_string(), Value::String(self.inner.endpoint.clone()));
        let register_line = encode_control_line(&Value::Object(register_msg))?;
        let _ = writer_tx.send(register_line);

        let inner_for_writer = Arc::clone(&self.inner);
        tokio::spawn(async move {
            while let Some(line) = writer_rx.recv().await {
                if write_half.write_all(line.as_bytes()).await.is_err() {
                    disconnected(
                        &inner_for_writer,
                        pi_js::Error::msg("Coordinator connection write failed"),
                    );
                    break;
                }
            }
        });

        let inner_for_reader = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => {
                        disconnected(&inner_for_reader, pi_js::Error::msg("Coordinator connection closed"));
                        break;
                    }
                    Ok(_) => {
                        if buf.len() > MAX_CONTROL_LINE_BYTES {
                            disconnected(&inner_for_reader, pi_js::Error::msg("Coordinator message is too large"));
                            break;
                        }
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                        }
                        if buf.last() == Some(&b'\r') {
                            buf.pop();
                        }
                        let text = match std::str::from_utf8(&buf) {
                            Ok(s) => s,
                            Err(_) => {
                                disconnected(&inner_for_reader, pi_js::Error::msg("Coordinator sent invalid JSON"));
                                break;
                            }
                        };
                        let value = match parse(text) {
                            Ok(v) => v,
                            Err(_) => {
                                disconnected(&inner_for_reader, pi_js::Error::msg("Coordinator sent invalid JSON"));
                                break;
                            }
                        };
                        handle_message(&inner_for_reader, &value);
                    }
                    Err(err) => {
                        disconnected(
                            &inner_for_reader,
                            pi_js::Error::from_io(err, "coordinatorRead", Some(&inner_for_reader.control_path), None),
                        );
                        break;
                    }
                }
            }
        });

        reg_rx
            .await
            .map_err(|_| pi_js::Error::msg("Coordinator connection closed"))?
    }

    pub async fn send(&self, peer_id: &str, payload: &Value) -> pi_js::Result<()> {
        let mut msg = Map::new();
        msg.insert("type".to_string(), Value::String("send".to_string()));
        msg.insert("to".to_string(), Value::String(peer_id.to_string()));
        msg.insert("payload".to_string(), payload.clone());
        self.write_message(&Value::Object(msg))
    }

    pub async fn broadcast(&self, payload: &Value) -> pi_js::Result<()> {
        let mut msg = Map::new();
        msg.insert("type".to_string(), Value::String("broadcast".to_string()));
        msg.insert("payload".to_string(), payload.clone());
        self.write_message(&Value::Object(msg))
    }

    pub fn close(&self) {
        let mut state = lock(&self.inner.state);
        if state.closed {
            return;
        }
        state.closed = true;
        state.writer_tx = None;
        state.peer_ids.clear();
        if let Some(tx) = state.pending_registered.take() {
            let _ = tx.send(Err(pi_js::Error::msg("Coordinator server closed")));
        }
    }

    fn write_message(&self, message: &Value) -> pi_js::Result<()> {
        let tx = {
            let state = lock(&self.inner.state);
            if !state.registered || state.closed {
                return Err(pi_js::Error::msg("Coordinator server is not connected"));
            }
            state
                .writer_tx
                .clone()
                .ok_or_else(|| pi_js::Error::msg("Coordinator server is not connected"))?
        };
        let line = encode_control_line(message)?;
        tx.send(line)
            .map_err(|_| pi_js::Error::msg("Coordinator server is not connected"))
    }
}

fn handle_message(inner: &Arc<CoordinatorConnectionInner>, value: &Value) {
    let Some(message) = parse_coordinator_message(value) else {
        disconnected(inner, pi_js::Error::msg("Coordinator sent an invalid message"));
        return;
    };
    match message {
        CoordinatorMessage::ServerRegistered {
            server_connection_id,
            peers,
        } => {
            if server_connection_id != inner.server_connection_id {
                disconnected(
                    inner,
                    pi_js::Error::msg("Coordinator returned an invalid server registration"),
                );
                return;
            }
            let pending = {
                let mut state = lock(&inner.state);
                for peer_id in peers {
                    state.peer_ids.insert(peer_id);
                }
                state.registered = true;
                state.pending_registered.take()
            };
            if let Some(tx) = pending {
                let _ = tx.send(Ok(()));
            }
        }
        CoordinatorMessage::ServerReplaced => {
            mark_replaced(inner);
        }
        CoordinatorMessage::PeerConnected { peer_id } => {
            let listeners: Vec<CoordinatorListener> = {
                let mut state = lock(&inner.state);
                state.peer_ids.insert(peer_id.clone());
                state.listeners.values().cloned().collect()
            };
            let event = CoordinatorConnectionEvent::PeerConnected { peer_id };
            for listener in listeners {
                listener(event.clone());
            }
        }
        CoordinatorMessage::PeerDisconnected { peer_id } => {
            let listeners: Vec<CoordinatorListener> = {
                let mut state = lock(&inner.state);
                state.peer_ids.shift_remove(&peer_id);
                state.listeners.values().cloned().collect()
            };
            let event = CoordinatorConnectionEvent::PeerDisconnected { peer_id };
            for listener in listeners {
                listener(event.clone());
            }
        }
        CoordinatorMessage::Message { from, payload } => {
            let listeners: Vec<CoordinatorListener> = lock(&inner.state).listeners.values().cloned().collect();
            let event = CoordinatorConnectionEvent::Message { from, payload };
            for listener in listeners {
                listener(event.clone());
            }
        }
    }
}

fn disconnected(inner: &Arc<CoordinatorConnectionInner>, error: pi_js::Error) {
    let (closed, pending) = {
        let mut state = lock(&inner.state);
        state.writer_tx = None;
        (state.closed, state.pending_registered.take())
    };
    if closed {
        return;
    }
    if let Some(tx) = pending {
        let _ = tx.send(Err(error));
    }
    mark_replaced(inner);
}

fn mark_replaced(inner: &Arc<CoordinatorConnectionInner>) {
    {
        let mut state = lock(&inner.state);
        if state.replaced {
            return;
        }
        state.replaced = true;
    }
    let _ = inner.replaced_tx.send(true);
}

pub struct CoordinatorStartupLease {
    stream: Option<UnixStream>,
}

impl CoordinatorStartupLease {
    pub fn close(&mut self) {
        self.stream = None;
    }
}

async fn try_connect(path: &str) -> pi_js::Result<Option<UnixStream>> {
    match UnixStream::connect(path).await {
        Ok(stream) => Ok(Some(stream)),
        Err(err) => match err.raw_os_error() {
            Some(libc::ENOENT | libc::ECONNREFUSED) => Ok(None),
            _ if err.kind() == std::io::ErrorKind::NotFound || err.kind() == std::io::ErrorKind::ConnectionRefused => {
                Ok(None)
            }
            _ => Err(pi_js::Error::from_io(err, "connectCoordinator", Some(path), None)),
        },
    }
}

pub async fn ensure_coordinator(public_path: &str, control_path: &str) -> pi_js::Result<CoordinatorStartupLease> {
    if let Some(existing) = try_connect(control_path).await? {
        return Ok(CoordinatorStartupLease { stream: Some(existing) });
    }
    let child = spawn_internal_process(
        InternalProcessRole::Coordinator,
        &[public_path.to_string(), control_path.to_string()],
        &InternalProcessSpawnOptions::default(),
    )?;
    let deadline = now_ms() + COORDINATOR_START_TIMEOUT_MS;
    loop {
        if let Some(socket) = try_connect(control_path).await? {
            return Ok(CoordinatorStartupLease { stream: Some(socket) });
        }
        if child.has_exited() {
            return Err(pi_js::Error::msg("Coordinator exited during startup"));
        }
        if now_ms() >= deadline {
            return Err(pi_js::Error::msg("Timed out waiting for coordinator startup"));
        }
        pi_js::time::sleep(COORDINATOR_RETRY_MS, None).await?;
    }
}

// ============================================================================
// Coordinator daemon implementation (mirroring lines 270..620 of coordinator.ts)
// ============================================================================

struct ServerPeer {
    server_connection_id: String,
    endpoint: String,
    tx: mpsc::UnboundedSender<String>,
}

struct RoutedPeer {
    peer_id: String,
    tx: mpsc::UnboundedSender<String>,
}

struct DaemonState {
    next_conn_id: u64,
    peers: IndexMap<String, RoutedPeer>,
    control_connections: IndexMap<u64, mpsc::UnboundedSender<String>>,
    public_connections: IndexMap<u64, Arc<Notify>>,
    current_server: Option<ServerPeer>,
    shutting_down: bool,
    empty_generation: u64,
}

struct DaemonHost {
    public_path: String,
    control_path: String,
    state: Mutex<DaemonState>,
    shutdown_notify: Arc<Notify>,
}

pub async fn run_coordinator_process(args: &[String]) -> pi_js::Result<()> {
    let Some(public_path) = args.first().cloned().filter(|s| !s.is_empty()) else {
        return Err(pi_js::Error::msg(
            "Coordinator requires public and control socket paths",
        ));
    };
    let Some(control_path) = args.get(1).cloned().filter(|s| !s.is_empty()) else {
        return Err(pi_js::Error::msg(
            "Coordinator requires public and control socket paths",
        ));
    };

    remove_stale_socket(&control_path).await?;
    remove_stale_socket(&public_path).await?;

    let control_listener = match UnixListener::bind(&control_path) {
        Ok(l) => l,
        Err(err) => {
            let _ = cleanup_socket(&control_path).await;
            let _ = cleanup_socket(&public_path).await;
            return Err(pi_js::Error::from_io(
                err,
                "listenControlSocket",
                Some(&control_path),
                None,
            ));
        }
    };
    if let Err(err) = restrict_socket(&control_path).await {
        let _ = cleanup_socket(&control_path).await;
        let _ = cleanup_socket(&public_path).await;
        return Err(err);
    }

    let public_listener = match UnixListener::bind(&public_path) {
        Ok(l) => l,
        Err(err) => {
            let _ = cleanup_socket(&control_path).await;
            let _ = cleanup_socket(&public_path).await;
            return Err(pi_js::Error::from_io(
                err,
                "listenPublicSocket",
                Some(&public_path),
                None,
            ));
        }
    };
    if let Err(err) = restrict_socket(&public_path).await {
        let _ = cleanup_socket(&control_path).await;
        let _ = cleanup_socket(&public_path).await;
        return Err(err);
    }

    let host = Arc::new(DaemonHost {
        public_path: public_path.clone(),
        control_path: control_path.clone(),
        state: Mutex::new(DaemonState {
            next_conn_id: 1,
            peers: IndexMap::new(),
            control_connections: IndexMap::new(),
            public_connections: IndexMap::new(),
            current_server: None,
            shutting_down: false,
            empty_generation: 0,
        }),
        shutdown_notify: Arc::new(Notify::new()),
    });

    schedule_empty_shutdown(&host, EMPTY_STARTUP_GRACE_MS);

    let host_ctrl = Arc::clone(&host);
    let ctrl_task = tokio::spawn(async move {
        loop {
            match control_listener.accept().await {
                Ok((stream, _)) => accept_control_connection(Arc::clone(&host_ctrl), stream),
                Err(_) => break,
            }
        }
    });

    let host_pub = Arc::clone(&host);
    let pub_task = tokio::spawn(async move {
        loop {
            match public_listener.accept().await {
                Ok((stream, _)) => accept_public_connection(Arc::clone(&host_pub), stream),
                Err(_) => break,
            }
        }
    });

    #[cfg(unix)]
    {
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .map_err(|e| pi_js::Error::from_io(e, "signal", None, None))?;
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|e| pi_js::Error::from_io(e, "signal", None, None))?;
        tokio::select! {
            _ = host.shutdown_notify.notified() => {}
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = host.shutdown_notify.notified() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }

    shutdown_coordinator(&host).await;
    ctrl_task.abort();
    pub_task.abort();
    Ok(())
}

fn schedule_empty_shutdown(host: &Arc<DaemonHost>, delay_ms: u64) {
    let generation = {
        let mut state = lock(&host.state);
        if state.shutting_down {
            return;
        }
        state.empty_generation += 1;
        state.empty_generation
    };
    let host_weak = Arc::downgrade(host);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        let Some(host) = host_weak.upgrade() else {
            return;
        };
        let should_shutdown = {
            let state = lock(&host.state);
            state.empty_generation == generation
                && !state.shutting_down
                && state.current_server.is_none()
                && state.peers.is_empty()
                && state.public_connections.is_empty()
                && state.control_connections.is_empty()
        };
        if should_shutdown {
            shutdown_coordinator(&host).await;
        }
    });
}

fn cancel_empty_shutdown(state: &mut DaemonState) {
    state.empty_generation += 1;
}

fn check_empty(host: &Arc<DaemonHost>) {
    let should_schedule = {
        let mut state = lock(&host.state);
        if state.shutting_down
            || state.current_server.is_some()
            || !state.peers.is_empty()
            || !state.public_connections.is_empty()
            || !state.control_connections.is_empty()
        {
            cancel_empty_shutdown(&mut state);
            false
        } else {
            true
        }
    };
    if should_schedule {
        schedule_empty_shutdown(host, EMPTY_SHUTDOWN_GRACE_MS);
    }
}

async fn shutdown_coordinator(host: &Arc<DaemonHost>) {
    let public_kills = {
        let mut state = lock(&host.state);
        if state.shutting_down {
            return;
        }
        state.shutting_down = true;
        cancel_empty_shutdown(&mut state);
        state.control_connections.clear();
        state.peers.clear();
        state.current_server = None;
        state.public_connections.drain(..).map(|(_, n)| n).collect::<Vec<_>>()
    };
    for notify in public_kills {
        notify.notify_waiters();
    }
    let _ = cleanup_socket(&host.public_path).await;
    let _ = cleanup_socket(&host.control_path).await;
    host.shutdown_notify.notify_waiters();
}

fn write_routed_line(tx: &mpsc::UnboundedSender<String>, message: &Value) {
    if let Ok(line) = encode_control_line(message) {
        let _ = tx.send(line);
    }
}

fn accept_control_connection(host: Arc<DaemonHost>, stream: UnixStream) {
    let (conn_id, tx, mut rx) = {
        let mut state = lock(&host.state);
        if state.shutting_down {
            return;
        }
        let id = state.next_conn_id;
        state.next_conn_id += 1;
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        state.control_connections.insert(id, tx.clone());
        (id, tx, rx)
    };

    let (read_half, mut write_half) = stream.into_split();
    let writer_handle = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if write_half.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });

    tokio::spawn(async move {
        let mut reader = BufReader::new(read_half);
        let mut buf = Vec::new();
        let mut server_id: Option<String> = None;
        let mut peer_id: Option<String> = None;

        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if buf.len() > MAX_CONTROL_LINE_BYTES {
                        break;
                    }
                    if buf.last() == Some(&b'\n') {
                        buf.pop();
                    }
                    if buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                    let Ok(text) = std::str::from_utf8(&buf) else {
                        break;
                    };
                    let Ok(val) = parse(text) else {
                        break;
                    };
                    let Some(obj) = val.as_object() else {
                        break;
                    };
                    let Some(msg_type) = obj.get("type").and_then(Value::as_str) else {
                        break;
                    };

                    if server_id.is_none() && peer_id.is_none() {
                        if msg_type == "register_server" {
                            if register_daemon_server(&host, conn_id, &tx, obj).is_err() {
                                break;
                            }
                            server_id = obj
                                .get("serverConnectionId")
                                .and_then(Value::as_str)
                                .map(ToString::to_string);
                            continue;
                        }
                        if msg_type == "register_peer" {
                            if register_daemon_peer(&host, &tx, obj).is_err() {
                                break;
                            }
                            peer_id = obj.get("peerId").and_then(Value::as_str).map(ToString::to_string);
                            continue;
                        }
                        break;
                    }

                    if let Some(ref sid) = server_id {
                        let is_current = lock(&host.state)
                            .current_server
                            .as_ref()
                            .is_some_and(|s| s.server_connection_id == *sid);
                        if is_current && handle_routed_message(&host, "server", obj).is_err() {
                            break;
                        }
                    } else if let Some(ref pid) = peer_id
                        && handle_routed_message(&host, pid, obj).is_err()
                    {
                        break;
                    }
                }
            }
        }

        let notifies_to_fire = {
            let mut state = lock(&host.state);
            state.control_connections.shift_remove(&conn_id);
            let mut extra: Vec<Arc<Notify>> = Vec::new();
            if let Some(ref sid) = server_id
                && state
                    .current_server
                    .as_ref()
                    .is_some_and(|s| s.server_connection_id == *sid)
            {
                state.current_server = None;
                let mut msg = Map::new();
                msg.insert("type".to_string(), Value::String("server_disconnected".to_string()));
                msg.insert("serverConnectionId".to_string(), Value::String(sid.clone()));
                let val = Value::Object(msg);
                for p in state.peers.values() {
                    write_routed_line(&p.tx, &val);
                }
            }
            if let Some(ref pid) = peer_id
                && state.peers.contains_key(pid)
            {
                state.peers.shift_remove(pid);
                if let Some(ref srv) = state.current_server {
                    let mut msg = Map::new();
                    msg.insert("type".to_string(), Value::String("peer_disconnected".to_string()));
                    msg.insert("peerId".to_string(), Value::String(pid.clone()));
                    write_routed_line(&srv.tx, &Value::Object(msg));
                }
            }
            let _ = &mut extra;
            extra
        };
        drop(notifies_to_fire);
        writer_handle.abort();
        check_empty(&host);
    });
}

fn register_daemon_server(
    host: &Arc<DaemonHost>,
    _conn_id: u64,
    tx: &mpsc::UnboundedSender<String>,
    obj: &Map<String, Value>,
) -> pi_js::Result<()> {
    let protocol = obj.get("protocol").and_then(Value::as_u64);
    if protocol != Some(u64::from(COORDINATOR_PROTOCOL_VERSION)) {
        return Err(pi_js::Error::msg("Unsupported coordinator protocol"));
    }
    let Some(server_connection_id) = obj
        .get("serverConnectionId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Err(pi_js::Error::msg("Coordinator serverConnectionId must be a string"));
    };
    let Some(endpoint) = obj.get("endpoint").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return Err(pi_js::Error::msg("Coordinator endpoint must be a string"));
    };

    let (previous, public_closes) = {
        let mut state = lock(&host.state);
        cancel_empty_shutdown(&mut state);
        let peer_keys: Vec<Value> = state.peers.keys().cloned().map(Value::String).collect();
        let previous = state.current_server.replace(ServerPeer {
            server_connection_id: server_connection_id.to_string(),
            endpoint: endpoint.to_string(),
            tx: tx.clone(),
        });
        let mut reg_msg = Map::new();
        reg_msg.insert("type".to_string(), Value::String("server_registered".to_string()));
        reg_msg.insert(
            "serverConnectionId".to_string(),
            Value::String(server_connection_id.to_string()),
        );
        reg_msg.insert("peers".to_string(), Value::Array(peer_keys));
        write_routed_line(tx, &Value::Object(reg_msg));

        let public_closes = if previous.is_some() {
            state.public_connections.drain(..).map(|(_, n)| n).collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        if let Some(ref prev) = previous {
            let mut disc_msg = Map::new();
            disc_msg.insert("type".to_string(), Value::String("server_disconnected".to_string()));
            disc_msg.insert(
                "serverConnectionId".to_string(),
                Value::String(prev.server_connection_id.clone()),
            );
            let disc_val = Value::Object(disc_msg);
            for p in state.peers.values() {
                write_routed_line(&p.tx, &disc_val);
            }
            let mut rep_msg = Map::new();
            rep_msg.insert("type".to_string(), Value::String("server_replaced".to_string()));
            write_routed_line(&prev.tx, &Value::Object(rep_msg));
        }

        let mut conn_msg = Map::new();
        conn_msg.insert("type".to_string(), Value::String("server_connected".to_string()));
        conn_msg.insert(
            "serverConnectionId".to_string(),
            Value::String(server_connection_id.to_string()),
        );
        let conn_val = Value::Object(conn_msg);
        for p in state.peers.values() {
            write_routed_line(&p.tx, &conn_val);
        }
        (previous, public_closes)
    };
    drop(previous);
    for notify in public_closes {
        notify.notify_waiters();
    }
    Ok(())
}

fn register_daemon_peer(
    host: &Arc<DaemonHost>,
    tx: &mpsc::UnboundedSender<String>,
    obj: &Map<String, Value>,
) -> pi_js::Result<()> {
    let protocol = obj.get("protocol").and_then(Value::as_u64);
    if protocol != Some(u64::from(COORDINATOR_PROTOCOL_VERSION)) {
        return Err(pi_js::Error::msg("Unsupported coordinator protocol"));
    }
    let Some(peer_id) = obj.get("peerId").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return Err(pi_js::Error::msg("Coordinator peerId must be a string"));
    };
    let mut state = lock(&host.state);
    if peer_id == "server" || state.peers.contains_key(peer_id) {
        return Err(pi_js::Error::msg(format!(
            "Coordinator peer is already connected: {peer_id}"
        )));
    }
    state.peers.insert(
        peer_id.to_string(),
        RoutedPeer {
            peer_id: peer_id.to_string(),
            tx: tx.clone(),
        },
    );
    cancel_empty_shutdown(&mut state);
    let mut reg_msg = Map::new();
    reg_msg.insert("type".to_string(), Value::String("peer_registered".to_string()));
    reg_msg.insert("peerId".to_string(), Value::String(peer_id.to_string()));
    if let Some(ref srv) = state.current_server {
        reg_msg.insert(
            "serverConnectionId".to_string(),
            Value::String(srv.server_connection_id.clone()),
        );
    }
    write_routed_line(tx, &Value::Object(reg_msg));
    if let Some(ref srv) = state.current_server {
        let mut peer_msg = Map::new();
        peer_msg.insert("type".to_string(), Value::String("peer_connected".to_string()));
        peer_msg.insert("peerId".to_string(), Value::String(peer_id.to_string()));
        write_routed_line(&srv.tx, &Value::Object(peer_msg));
    }
    Ok(())
}

fn handle_routed_message(host: &Arc<DaemonHost>, from: &str, obj: &Map<String, Value>) -> pi_js::Result<()> {
    let msg_type = obj.get("type").and_then(Value::as_str).unwrap_or("");
    if msg_type == "send" {
        let Some(to) = obj.get("to").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return Err(pi_js::Error::msg("Coordinator message target must be a string"));
        };
        let payload = obj.get("payload").cloned().unwrap_or(Value::Null);
        let state = lock(&host.state);
        let target = if to == "server" {
            state.current_server.as_ref().map(|s| s.tx.clone())
        } else {
            state.peers.get(to).map(|p| p.tx.clone())
        };
        if let Some(tx) = target {
            let mut out = Map::new();
            out.insert("type".to_string(), Value::String("message".to_string()));
            out.insert("from".to_string(), Value::String(from.to_string()));
            out.insert("payload".to_string(), payload);
            write_routed_line(&tx, &Value::Object(out));
        }
        return Ok(());
    }
    if msg_type == "broadcast" {
        if from != "server" {
            return Err(pi_js::Error::msg("Only the current server may broadcast"));
        }
        let payload = obj.get("payload").cloned().unwrap_or(Value::Null);
        let state = lock(&host.state);
        let mut out = Map::new();
        out.insert("type".to_string(), Value::String("message".to_string()));
        out.insert("from".to_string(), Value::String(from.to_string()));
        out.insert("payload".to_string(), payload);
        let val = Value::Object(out);
        for peer in state.peers.values() {
            write_routed_line(&peer.tx, &val);
        }
        return Ok(());
    }
    Err(pi_js::Error::msg(format!(
        "Unknown coordinator routing message: {msg_type}"
    )))
}

fn accept_public_connection(host: Arc<DaemonHost>, mut client: UnixStream) {
    let (conn_id, endpoint, kill_notify) = {
        let mut state = lock(&host.state);
        if state.shutting_down {
            return;
        }
        let Some(endpoint) = state.current_server.as_ref().map(|s| s.endpoint.clone()) else {
            return;
        };
        cancel_empty_shutdown(&mut state);
        let id = state.next_conn_id;
        state.next_conn_id += 1;
        let kill_notify = Arc::new(Notify::new());
        state.public_connections.insert(id, Arc::clone(&kill_notify));
        (id, endpoint, kill_notify)
    };

    tokio::spawn(async move {
        if let Ok(mut upstream) = UnixStream::connect(&endpoint).await {
            tokio::select! {
                _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {}
                _ = kill_notify.notified() => {}
            }
        }
        lock(&host.state).public_connections.shift_remove(&conn_id);
        check_empty(&host);
    });
}

async fn restrict_socket(path: &str) -> pi_js::Result<()> {
    if pi_js::env::platform() != "win32" {
        chmod(path, 0o600).await?;
    }
    Ok(())
}

async fn remove_stale_socket(path: &str) -> pi_js::Result<()> {
    match lstat(path).await {
        Ok(stats) => {
            if !stats.is_socket() {
                return Err(pi_js::Error::msg(format!("Coordinator path is not a socket: {path}")));
            }
        }
        Err(error) if error.code() == Some("ENOENT") => return Ok(()),
        Err(error) => return Err(error),
    }
    if try_connect(path).await?.is_some() {
        return Err(pi_js::Error::msg(format!(
            "Coordinator socket is already active: {path}"
        )));
    }
    cleanup_socket(path).await
}

async fn cleanup_socket(path: &str) -> pi_js::Result<()> {
    if pi_js::env::platform() == "win32" {
        return Ok(());
    }
    match unlink(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.code() == Some("ENOENT") => Ok(()),
        Err(error) => Err(error),
    }
}

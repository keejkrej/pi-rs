//! Port of packages/client/src/unix.ts

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pi_protocol::{DEFAULT_MAX_FRAME_LENGTH, ServerId, is_server_id};
use tokio::sync::{mpsc, oneshot};

use crate::client::Client;
use crate::errors::{DisconnectedError, ServerError};
use crate::transport::{ByteTransport, ByteTransportFactory, ByteTransportHandlers};
use crate::types::ClientOptions;

const DEFAULT_DISCOVERY_TIMEOUT_MS: i64 = 1_000;
const MAX_TIMER_DELAY_MS: i64 = 2_147_483_647;
const UNIX_SOCKET_SUFFIX: &str = ".sock";
const MAX_CONCURRENT_DISCOVERY_PROBES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnixTransportOptions {
    pub path: String,
    pub max_pending_bytes: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnixServerRoute {
    pub server_id: ServerId,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoverUnixServersOptions {
    /// Directory containing server-addressed Unix sockets.
    pub directory: String,
    /// Maximum time for each connection and handshake. Defaults to 1,000 ms.
    pub timeout_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Unix discovery timed out")]
struct UnixDiscoveryTimeoutError;

/// Discover reachable local servers by probing server-addressed Unix sockets.
pub async fn discover_unix_servers(options: DiscoverUnixServersOptions) -> pi_js::Result<Vec<UnixServerRoute>> {
    if pi_js::env::platform() == "win32" {
        return Err(pi_js::Error::msg("Unix transport is not supported on Windows"));
    }

    let timeout_ms = options.timeout_ms.unwrap_or(DEFAULT_DISCOVERY_TIMEOUT_MS);
    if timeout_ms <= 0 || timeout_ms > MAX_TIMER_DELAY_MS {
        return Err(pi_js::Error::js(
            "TypeError",
            format!("Unix discovery timeoutMs must be an integer between 1 and {MAX_TIMER_DELAY_MS}"),
        ));
    }

    let names = match pi_js::fs::promises::readdir(&options.directory).await {
        Ok(names) => names,
        Err(err) if err.code() == Some("ENOENT") => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    let candidates: Vec<UnixServerRoute> = names
        .into_iter()
        .filter_map(|name| {
            let server_id = name.strip_suffix(UNIX_SOCKET_SUFFIX)?;
            if !is_server_id(&serde_json::Value::String(server_id.to_string())) {
                return None;
            }
            let full_path = Path::new(&options.directory).join(&name);
            Some(UnixServerRoute {
                server_id: server_id.to_string(),
                path: full_path.to_string_lossy().into_owned(),
            })
        })
        .collect();

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let candidates = Arc::new(candidates);
    let next_index = Arc::new(AtomicUsize::new(0));
    let routes = Arc::new(Mutex::new(Vec::<UnixServerRoute>::new()));
    let failure = Arc::new(Mutex::new(Option::<pi_js::Error>::None));

    let worker_count = MAX_CONCURRENT_DISCOVERY_PROBES.min(candidates.len());
    let mut handles = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let candidates = Arc::clone(&candidates);
        let next_index = Arc::clone(&next_index);
        let routes = Arc::clone(&routes);
        let failure = Arc::clone(&failure);
        handles.push(tokio::spawn(async move {
            loop {
                if failure.lock().unwrap_or_else(PoisonError::into_inner).is_some() {
                    return;
                }
                let index = next_index.fetch_add(1, Ordering::Relaxed);
                let Some(candidate) = candidates.get(index) else {
                    return;
                };
                match pi_js::fs::promises::lstat(&candidate.path).await {
                    Ok(stats) => {
                        if !stats.is_socket() {
                            continue;
                        }
                    }
                    Err(err) if err.code() == Some("ENOENT") => continue,
                    Err(err) => {
                        let mut fail_guard = failure.lock().unwrap_or_else(PoisonError::into_inner);
                        if fail_guard.is_none() {
                            *fail_guard = Some(err);
                        }
                        return;
                    }
                }
                match probe_unix_server(candidate, timeout_ms).await {
                    Ok(Some(route)) => {
                        routes.lock().unwrap_or_else(PoisonError::into_inner).push(route);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let mut fail_guard = failure.lock().unwrap_or_else(PoisonError::into_inner);
                        if fail_guard.is_none() {
                            *fail_guard = Some(err);
                        }
                        return;
                    }
                }
            }
        }));
    }

    for handle in handles {
        let _ = handle.await;
    }

    if let Some(err) = failure.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(err);
    }

    let mut discovered = std::mem::take(&mut *routes.lock().unwrap_or_else(PoisonError::into_inner));
    discovered.sort_by(|left, right| left.server_id.cmp(&right.server_id));
    Ok(discovered)
}

/// Creates fresh Unix-domain socket transports for Client connection attempts.
pub fn create_unix_transport_factory(options: UnixTransportOptions) -> pi_js::Result<ByteTransportFactory> {
    let max_pending_bytes = validate_unix_transport_options(&options)?;
    let path = options.path;
    Ok(Arc::new(move |handlers| {
        let path = path.clone();
        Box::pin(async move { connect_unix_socket(path, max_pending_bytes, handlers).await })
    }))
}

fn validate_unix_transport_options(options: &UnixTransportOptions) -> pi_js::Result<i64> {
    if options.path.is_empty() {
        return Err(pi_js::Error::js("TypeError", "Unix transport path must not be empty"));
    }
    let max_pending_bytes = options.max_pending_bytes.unwrap_or(DEFAULT_MAX_FRAME_LENGTH * 4);
    if max_pending_bytes <= 0 {
        return Err(pi_js::Error::js(
            "TypeError",
            "Unix transport maxPendingBytes must be a positive safe integer",
        ));
    }
    if pi_js::env::platform() == "win32" {
        return Err(pi_js::Error::msg("Unix transport is not supported on Windows"));
    }
    Ok(max_pending_bytes)
}

#[cfg(unix)]
async fn connect_unix_socket(
    path: String,
    max_pending_bytes: i64,
    handlers: ByteTransportHandlers,
) -> pi_js::Result<Arc<dyn ByteTransport>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    use tokio_util::sync::CancellationToken;

    if pi_js::env::platform() == "win32" {
        return Err(pi_js::Error::msg("Unix transport is not supported on Windows"));
    }

    let stream = UnixStream::connect(&path)
        .await
        .map_err(|e| pi_js::Error::from_io(e, "connect", Some(&path), None))?;
    let (mut reader, mut writer) = stream.into_split();

    let terminal = Arc::new(AtomicBool::new(false));
    let cancel = CancellationToken::new();

    type WriteCommand = (Vec<u8>, oneshot::Sender<pi_js::Result<()>>);
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<WriteCommand>();

    let send_state = Arc::new(Mutex::new(UnixSendState {
        closed: false,
        pending_bytes: 0,
        write_tx: Some(write_tx),
    }));

    let reader_terminal = Arc::clone(&terminal);
    let reader_handlers = handlers.clone();
    let reader_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            tokio::select! {
                _ = reader_cancel.cancelled() => {
                    break;
                }
                res = reader.read(&mut buf) => {
                    match res {
                        Ok(0) => {
                            if !reader_terminal.swap(true, Ordering::AcqRel) {
                                reader_handlers.on_close();
                            }
                            break;
                        }
                        Ok(n) => {
                            if !reader_terminal.load(Ordering::Acquire) {
                                reader_handlers.on_data(&buf[..n]);
                            }
                        }
                        Err(err) => {
                            if !reader_terminal.swap(true, Ordering::AcqRel) {
                                reader_handlers.on_error(pi_js::Error::from_io(err, "read", None, None));
                            }
                            break;
                        }
                    }
                }
            }
        }
    });

    let writer_state = Arc::clone(&send_state);
    let writer_terminal = Arc::clone(&terminal);
    let writer_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            let next_cmd = tokio::select! {
                _ = writer_cancel.cancelled() => None,
                cmd = write_rx.recv() => cmd,
            };
            let Some((chunk, done)) = next_cmd else {
                // Fail any buffered commands on close so waiters never hang.
                write_rx.close();
                while let Some((chunk, done)) = write_rx.recv().await {
                    let mut st = writer_state.lock().unwrap_or_else(PoisonError::into_inner);
                    st.pending_bytes -= chunk.len() as i64;
                    drop(st);
                    let _ = done.send(Err(pi_js::Error::msg("Unix transport closed during write")));
                }
                break;
            };
            let chunk_len = chunk.len() as i64;
            let write_result = if writer_terminal.load(Ordering::Acquire) {
                Err(pi_js::Error::msg("Unix transport is closed"))
            } else {
                tokio::select! {
                    _ = writer_cancel.cancelled() => Err(pi_js::Error::msg("Unix transport closed during write")),
                    res = writer.write_all(&chunk) => res.map_err(|e| pi_js::Error::from_io(e, "write", None, None)),
                }
            };
            {
                let mut st = writer_state.lock().unwrap_or_else(PoisonError::into_inner);
                st.pending_bytes -= chunk_len;
            }
            let _ = done.send(write_result);
        }
    });

    Ok(Arc::new(UnixByteTransport {
        max_pending_bytes,
        terminal,
        send_state,
        cancel,
    }))
}

#[cfg(not(unix))]
async fn connect_unix_socket(
    _path: String,
    _max_pending_bytes: i64,
    _handlers: ByteTransportHandlers,
) -> pi_js::Result<Arc<dyn ByteTransport>> {
    Err(pi_js::Error::msg("Unix transport is not supported on Windows"))
}

struct UnixSendState {
    closed: bool,
    pending_bytes: i64,
    #[cfg(unix)]
    write_tx: Option<mpsc::UnboundedSender<(Vec<u8>, oneshot::Sender<pi_js::Result<()>>)>>,
}

#[cfg(unix)]
struct UnixByteTransport {
    max_pending_bytes: i64,
    terminal: Arc<AtomicBool>,
    send_state: Arc<Mutex<UnixSendState>>,
    cancel: tokio_util::sync::CancellationToken,
}

#[cfg(unix)]
impl ByteTransport for UnixByteTransport {
    fn send(&self, chunk: Vec<u8>) -> pi_js::BoxFuture<pi_js::Result<()>> {
        let len = chunk.len() as i64;
        let rx = {
            let mut st = self.send_state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.closed || self.terminal.load(Ordering::Acquire) {
                return Box::pin(async { Err(pi_js::Error::msg("Unix transport is closed")) });
            }
            if st.pending_bytes + len > self.max_pending_bytes {
                return Box::pin(async { Err(pi_js::Error::msg("Unix transport exceeded its pending byte limit")) });
            }
            let Some(write_tx) = st.write_tx.clone() else {
                return Box::pin(async { Err(pi_js::Error::msg("Unix transport is closed")) });
            };
            st.pending_bytes += len;
            let (tx, rx) = oneshot::channel();
            if write_tx.send((chunk, tx)).is_err() {
                st.pending_bytes -= len;
                return Box::pin(async { Err(pi_js::Error::msg("Unix transport is closed")) });
            }
            rx
        };

        Box::pin(async move {
            match rx.await {
                Ok(res) => res,
                Err(_) => Err(pi_js::Error::msg("Unix transport closed during write")),
            }
        })
    }

    fn close(&self) {
        self.terminal.store(true, Ordering::Release);
        {
            let mut st = self.send_state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.closed {
                return;
            }
            st.closed = true;
            st.write_tx.take();
        }
        self.cancel.cancel();
    }
}

async fn probe_unix_server(route: &UnixServerRoute, timeout_ms: i64) -> pi_js::Result<Option<UnixServerRoute>> {
    let factory = create_unix_transport_factory(UnixTransportOptions {
        path: route.path.clone(),
        max_pending_bytes: None,
    })?;
    let client = Client::new(ClientOptions {
        transport_factory: factory,
        server_id: route.server_id.clone(),
        max_frame_length: None,
        on_listener_error: None,
    })?;

    let connect_fut = client.connect();
    let timeout_fut = pi_js::time::sleep(timeout_ms as u64, None);

    let result = tokio::select! {
        res = connect_fut => res.map(|_| Some(route.clone())),
        _ = timeout_fut => Err(pi_js::Error::typed("UnixDiscoveryTimeoutError", UnixDiscoveryTimeoutError)),
    };

    let _ = client.dispose().await;

    match result {
        Ok(found) => Ok(found),
        Err(err) => {
            if is_ignorable_discovery_error(&err) {
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}

fn is_ignorable_discovery_error(error: &pi_js::Error) -> bool {
    if error.downcast_ref::<UnixDiscoveryTimeoutError>().is_some()
        || error.downcast_ref::<pi_protocol::ProtocolValidationError>().is_some()
        || error.downcast_ref::<pi_protocol::FrameError>().is_some()
        || error.downcast_ref::<pi_protocol::CborError>().is_some()
    {
        return true;
    }
    if let Some(disc) = error.downcast_ref::<DisconnectedError>()
        && disc.cause.is_none()
    {
        return true;
    }
    if let Some(server_err) = error.downcast_ref::<ServerError>()
        && server_err.code == "version"
    {
        return true;
    }
    has_error_code(error, "ENOENT")
        || has_error_code(error, "ECONNREFUSED")
        || has_error_code(error, "ECONNRESET")
        || has_error_code(error, "EPIPE")
        || has_error_code(error, "ETIMEDOUT")
}

fn has_error_code(error: &pi_js::Error, target_code: &str) -> bool {
    let mut current: Option<&pi_js::Error> = Some(error);
    let mut depth = 0;
    while let Some(err) = current {
        depth += 1;
        if depth > 16 {
            break;
        }
        if err.code() == Some(target_code) {
            return true;
        }
        if let Some(disc) = err.downcast_ref::<DisconnectedError>() {
            current = disc.cause();
        } else {
            current = err.cause();
        }
    }
    false
}

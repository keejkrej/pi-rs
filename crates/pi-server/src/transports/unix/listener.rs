//! Port of packages/server/src/transports/unix/listener.ts

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_js::crypto::sha256_hex;
use pi_js::time::{Timeout, set_timeout};
use pi_js::{Error, Result};
use pi_protocol::DEFAULT_MAX_FRAME_LENGTH;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};

use crate::connection::{ByteConnection, ByteConnectionAcceptor, ByteConnectionHandler};
use crate::listener::ServerListener;
use crate::transports::unix::types::UnixListenerOptions;
use crate::types::ServerErrorCallback;

const DEFAULT_SOCKET_MODE: u32 = 0o600;
const DEFAULT_GRACEFUL_CLOSE_TIMEOUT_MS: i64 = 5_000;
const MAX_UINT32: i64 = 0xffff_ffff;
const MAX_TIMER_DELAY_MS: i64 = 2_147_483_647;
#[cfg(unix)]
const SOCKET_PROBE_TIMEOUT_MS: u64 = 1_000;

struct ResolvedUnixListenerOptions {
    path: String,
    mode: u32,
    graceful_close_timeout_ms: i64,
    max_pending_bytes: i64,
    on_error: Option<ServerErrorCallback>,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

/// Abstract socket sink for [`UnixByteConnection`], allowing both live OS streams and controlled
/// transport mocks to drive backpressure and graceful close tests.
#[async_trait]
pub trait UnixByteSocket: Send + Sync {
    fn writable(&self) -> bool;
    fn destroyed(&self) -> bool;
    async fn write_chunk(&self, chunk: &[u8]) -> Result<()>;
    async fn end(&self, final_chunk: Option<&[u8]>) -> Result<()>;
    fn destroy(&self);
    fn subscribe_closed(&self) -> watch::Receiver<bool>;
}

struct AsyncStreamSocket<W> {
    writer: tokio::sync::Mutex<Option<W>>,
    writable: AtomicBool,
    destroyed: AtomicBool,
    closed_tx: watch::Sender<bool>,
    closed_rx: watch::Receiver<bool>,
}

impl<W> AsyncStreamSocket<W> {
    fn new(writer: W) -> Self {
        let (closed_tx, closed_rx) = watch::channel(false);
        Self {
            writer: tokio::sync::Mutex::new(Some(writer)),
            writable: AtomicBool::new(true),
            destroyed: AtomicBool::new(false),
            closed_tx,
            closed_rx,
        }
    }
}

#[async_trait]
impl<W: AsyncWrite + Unpin + Send + 'static> UnixByteSocket for AsyncStreamSocket<W> {
    fn writable(&self) -> bool {
        self.writable.load(Ordering::SeqCst)
    }

    fn destroyed(&self) -> bool {
        self.destroyed.load(Ordering::SeqCst)
    }

    async fn write_chunk(&self, chunk: &[u8]) -> Result<()> {
        if self.destroyed.load(Ordering::SeqCst) || !self.writable.load(Ordering::SeqCst) {
            return Err(Error::msg("Unix connection is closed"));
        }
        let mut guard = self.writer.lock().await;
        let Some(writer) = guard.as_mut() else {
            return Err(Error::msg("Unix connection is closed"));
        };
        writer
            .write_all(chunk)
            .await
            .map_err(|e| Error::from_io(e, "write", None, None))?;
        writer
            .flush()
            .await
            .map_err(|e| Error::from_io(e, "flush", None, None))?;
        Ok(())
    }

    async fn end(&self, final_chunk: Option<&[u8]>) -> Result<()> {
        self.writable.store(false, Ordering::SeqCst);
        let mut guard = self.writer.lock().await;
        let Some(writer) = guard.as_mut() else {
            return Ok(());
        };
        if let Some(bytes) = final_chunk {
            writer
                .write_all(bytes)
                .await
                .map_err(|e| Error::from_io(e, "write", None, None))?;
            writer
                .flush()
                .await
                .map_err(|e| Error::from_io(e, "flush", None, None))?;
        }
        writer
            .shutdown()
            .await
            .map_err(|e| Error::from_io(e, "shutdown", None, None))?;
        Ok(())
    }

    fn destroy(&self) {
        if self.destroyed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.writable.store(false, Ordering::SeqCst);
        self.closed_tx.send_replace(true);
        if let Ok(mut guard) = self.writer.try_lock() {
            guard.take();
        }
    }

    fn subscribe_closed(&self) -> watch::Receiver<bool> {
        self.closed_rx.clone()
    }
}

enum WriteCommand {
    Chunk {
        bytes: Vec<u8>,
        tx: oneshot::Sender<Result<()>>,
        _guard: PendingBytesGuard,
    },
    End {
        final_chunk: Option<Vec<u8>>,
    },
}

struct PendingBytesGuard {
    state: Arc<Mutex<ConnectionWriteState>>,
    len: usize,
}

impl Drop for PendingBytesGuard {
    fn drop(&mut self) {
        let mut guard = self.state.lock().expect("connection write mutex poisoned");
        guard.pending_bytes = guard.pending_bytes.saturating_sub(self.len);
    }
}

struct ConnectionWriteState {
    pending_bytes: usize,
    closed: bool,
    closing: bool,
    close_rx: Option<watch::Receiver<bool>>,
    close_timer: Option<Timeout>,
}

struct UnixByteConnectionInner {
    socket: Arc<dyn UnixByteSocket>,
    graceful_close_timeout_ms: i64,
    max_pending_bytes: usize,
    state: Arc<Mutex<ConnectionWriteState>>,
    closed_tx: watch::Sender<bool>,
    closed_rx: watch::Receiver<bool>,
    cmd_tx: Mutex<Option<mpsc::UnboundedSender<WriteCommand>>>,
}

/// Exported for transport-level verification.
pub struct UnixByteConnection {
    inner: Arc<UnixByteConnectionInner>,
}

impl UnixByteConnection {
    pub fn new(socket: Arc<dyn UnixByteSocket>, graceful_close_timeout_ms: i64, max_pending_bytes: i64) -> Arc<Self> {
        let (closed_tx, closed_rx) = watch::channel(false);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<WriteCommand>();
        let state = Arc::new(Mutex::new(ConnectionWriteState {
            pending_bytes: 0,
            closed: false,
            closing: false,
            close_rx: None,
            close_timer: None,
        }));
        let inner = Arc::new(UnixByteConnectionInner {
            socket: socket.clone(),
            graceful_close_timeout_ms,
            max_pending_bytes: max_pending_bytes.max(0) as usize,
            state,
            closed_tx,
            closed_rx,
            cmd_tx: Mutex::new(Some(cmd_tx)),
        });

        let conn = Arc::new(Self { inner });
        let writer_conn = conn.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                writer_conn.run_writer_loop(cmd_rx).await;
            });
            let watch_conn = conn.clone();
            let mut sock_closed_rx = socket.subscribe_closed();
            handle.spawn(async move {
                if sock_closed_rx.wait_for(|closed| *closed).await.is_ok() {
                    watch_conn.mark_closed();
                }
            });
        }
        conn
    }

    async fn run_writer_loop(&self, mut cmd_rx: mpsc::UnboundedReceiver<WriteCommand>) {
        let mut sock_closed_rx = self.inner.socket.subscribe_closed();
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                WriteCommand::Chunk { bytes, tx, _guard } => {
                    let is_closing = {
                        let guard = self.inner.state.lock().expect("connection write mutex poisoned");
                        guard.closed || guard.closing
                    };
                    if is_closing || self.inner.socket.destroyed() || !self.inner.socket.writable() {
                        drop(_guard);
                        let _ = tx.send(Err(Error::msg("Unix connection is closed")));
                        continue;
                    }
                    let result = tokio::select! {
                        biased;
                        changed = sock_closed_rx.wait_for(|closed| *closed) => {
                            let _ = changed;
                            Err(Error::msg("Unix connection closed during write"))
                        }
                        res = self.inner.socket.write_chunk(&bytes) => res,
                    };
                    drop(_guard);
                    let _ = tx.send(result);
                }
                WriteCommand::End { final_chunk } => {
                    if self.inner.socket.destroyed() {
                        self.mark_closed();
                        break;
                    }
                    if self.inner.socket.end(final_chunk.as_deref()).await.is_err() {
                        self.inner.socket.destroy();
                    }
                    break;
                }
            }
        }
    }

    pub fn mark_closed(&self) {
        {
            let mut guard = self.inner.state.lock().expect("connection write mutex poisoned");
            if guard.closed {
                return;
            }
            guard.closed = true;
            guard.closing = true;
            if let Some(timer) = guard.close_timer.take() {
                timer.clear();
            }
        }
        self.inner.cmd_tx.lock().expect("cmd_tx mutex poisoned").take();
        self.inner.closed_tx.send_replace(true);
    }
}

#[async_trait]
impl ByteConnection for UnixByteConnection {
    fn closed(&self) -> bool {
        self.inner.state.lock().expect("connection write mutex poisoned").closed
    }

    async fn send(&self, chunk: &[u8]) -> Result<()> {
        let pending_guard = {
            let mut guard = self.inner.state.lock().expect("connection write mutex poisoned");
            if guard.closed || guard.closing {
                return Err(Error::msg("Unix connection is closed"));
            }
            if guard.pending_bytes.saturating_add(chunk.len()) > self.inner.max_pending_bytes {
                return Err(Error::msg("Unix connection exceeded its pending byte limit"));
            }
            guard.pending_bytes += chunk.len();
            PendingBytesGuard {
                state: self.inner.state.clone(),
                len: chunk.len(),
            }
        };

        let (tx, rx) = oneshot::channel();
        let send_res = {
            let cmd_tx_guard = self.inner.cmd_tx.lock().expect("cmd_tx mutex poisoned");
            match cmd_tx_guard.as_ref() {
                Some(sender) => sender.send(WriteCommand::Chunk {
                    bytes: chunk.to_vec(),
                    tx,
                    _guard: pending_guard,
                }),
                None => return Err(Error::msg("Unix connection is closed")),
            }
        };
        if send_res.is_err() {
            return Err(Error::msg("Unix connection is closed"));
        }
        rx.await
            .unwrap_or_else(|_| Err(Error::msg("Unix connection closed during write")))
    }

    async fn close(&self, final_chunk: Option<&[u8]>) -> Result<()> {
        let mut rx = {
            let mut guard = self.inner.state.lock().expect("connection write mutex poisoned");
            if guard.closed || self.inner.socket.destroyed() {
                drop(guard);
                self.mark_closed();
                return Ok(());
            }
            if let Some(rx) = guard.close_rx.clone() {
                rx
            } else {
                guard.closing = true;
                let rx = self.inner.closed_rx.clone();
                guard.close_rx = Some(rx.clone());
                let socket_for_timer = self.inner.socket.clone();
                let state_for_timer = self.inner.state.clone();
                let closed_tx_for_timer = self.inner.closed_tx.clone();
                let timer = set_timeout(self.inner.graceful_close_timeout_ms as u64, move || {
                    if !socket_for_timer.destroyed() {
                        socket_for_timer.destroy();
                    }
                    let mut st = state_for_timer.lock().expect("connection write mutex poisoned");
                    st.closed = true;
                    st.closing = true;
                    st.close_timer.take();
                    drop(st);
                    closed_tx_for_timer.send_replace(true);
                });
                timer.unref();
                guard.close_timer = Some(timer);
                rx
            }
        };

        let final_vec = final_chunk.map(|bytes| bytes.to_vec());
        if let Some(sender) = self.inner.cmd_tx.lock().expect("cmd_tx mutex poisoned").take() {
            let _ = sender.send(WriteCommand::End { final_chunk: final_vec });
        }

        let _ = rx.wait_for(|closed| *closed).await;
        Ok(())
    }
}

struct UnixListenerMutableState {
    started: bool,
    closing: bool,
    connections: IndexMap<u64, Arc<UnixByteConnection>>,
    #[cfg(unix)]
    socket_identity: Option<FileIdentity>,
    #[cfg(unix)]
    owned_bind_path: Option<String>,
    shutdown_tx: Option<watch::Sender<bool>>,
    accept_done_rx: Option<watch::Receiver<bool>>,
    close_rx: Option<watch::Receiver<Option<std::result::Result<(), Arc<Error>>>>>,
}

struct UnixListenerInner {
    options: ResolvedUnixListenerOptions,
    next_conn_id: AtomicU64,
    state: Mutex<UnixListenerMutableState>,
}

pub struct UnixListener {
    inner: Arc<UnixListenerInner>,
}

impl UnixListener {
    fn new(options: UnixListenerOptions) -> Result<Self> {
        let resolved = resolve_unix_listener_options(options)?;
        Ok(Self {
            inner: Arc::new(UnixListenerInner {
                options: resolved,
                next_conn_id: AtomicU64::new(1),
                state: Mutex::new(UnixListenerMutableState {
                    started: false,
                    closing: false,
                    connections: IndexMap::new(),
                    #[cfg(unix)]
                    socket_identity: None,
                    #[cfg(unix)]
                    owned_bind_path: None,
                    shutdown_tx: None,
                    accept_done_rx: None,
                    close_rx: None,
                }),
            }),
        })
    }

    fn report_error(&self, error: &Error) {
        if let Some(ref cb) = self.inner.options.on_error {
            cb(error);
        }
    }

    fn spawn_stream_connection<R, W>(
        inner: &Arc<UnixListenerInner>,
        accept: &ByteConnectionAcceptor,
        mut read_half: R,
        write_half: W,
    ) where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let is_closing = inner.state.lock().expect("listener mutex poisoned").closing;
        if is_closing {
            return;
        }
        let socket = Arc::new(AsyncStreamSocket::new(write_half));
        let connection = UnixByteConnection::new(
            socket.clone(),
            inner.options.graceful_close_timeout_ms,
            inner.options.max_pending_bytes,
        );
        let conn_id = inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut guard = inner.state.lock().expect("listener mutex poisoned");
            if guard.closing {
                drop(guard);
                socket.destroy();
                connection.mark_closed();
                return;
            }
            guard.connections.insert(conn_id, connection.clone());
        }
        let handler: Arc<dyn ByteConnectionHandler> = accept(connection.clone());
        let inner_clone = inner.clone();
        let mut sock_closed_rx = socket.subscribe_closed();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                tokio::select! {
                    biased;
                    changed = sock_closed_rx.wait_for(|closed| *closed) => {
                        let _ = changed;
                        break;
                    }
                    read_res = read_half.read(&mut buf) => {
                        match read_res {
                            Ok(0) => break,
                            Ok(n) => {
                                handler.on_data(&buf[..n]);
                            }
                            Err(io_err) => {
                                let err = Error::from_io(io_err, "read", None, None);
                                handler.on_error(err);
                                socket.destroy();
                                break;
                            }
                        }
                    }
                }
            }
            socket.destroy();
            connection.mark_closed();
            {
                let mut guard = inner_clone.state.lock().expect("listener mutex poisoned");
                guard.connections.shift_remove(&conn_id);
            }
            handler.on_close();
        });
    }

    #[cfg(unix)]
    async fn start_unix(&self, accept: ByteConnectionAcceptor) -> Result<()> {
        {
            let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
            if guard.started {
                return Err(Error::msg("Unix listener is already started"));
            }
            if guard.closing {
                return Err(Error::msg("Unix listener is closing or closed"));
            }
            guard.started = true;
        }

        let start_outcome: Result<()> = async {
            let path = self.inner.options.path.clone();
            let owned_bind_path = get_owned_bind_path(&path);
            pi_js::fs::promises::mkdir(&pi_js::path::dirname(&path), true, Some(0o700)).await?;
            remove_stale_socket(&path).await?;
            remove_stale_socket(&owned_bind_path).await?;
            {
                let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
                guard.owned_bind_path = Some(owned_bind_path.clone());
            }

            let listener = tokio::net::UnixListener::bind(&owned_bind_path)
                .map_err(|e| Error::from_io(e, "listen", Some(&owned_bind_path), None))?;
            let stats = pi_js::fs::promises::lstat(&owned_bind_path).await?;
            if !stats.is_socket() {
                return Err(Error::msg(format!(
                    "Unix listener path is not a socket after binding: {owned_bind_path}"
                )));
            }
            {
                let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
                guard.socket_identity = Some(FileIdentity {
                    dev: stats.dev,
                    ino: stats.ino,
                });
            }
            pi_js::fs::promises::link(&owned_bind_path, &path).await?;
            set_socket_mode(&path, self.inner.options.mode).await?;
            remove_path(&owned_bind_path).await?;
            {
                let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
                guard.owned_bind_path = None;
            }

            let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
            let (accept_done_tx, accept_done_rx) = watch::channel(false);
            {
                let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
                guard.shutdown_tx = Some(shutdown_tx);
                guard.accept_done_rx = Some(accept_done_rx);
            }

            let inner = self.inner.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx.wait_for(|done| *done) => break,
                        res = listener.accept() => {
                            match res {
                                Ok((stream, _addr)) => {
                                    let (read_half, write_half) = stream.into_split();
                                    UnixListener::spawn_stream_connection(&inner, &accept, read_half, write_half);
                                }
                                Err(io_err) => {
                                    let err = Error::from_io(io_err, "accept", None, None);
                                    if let Some(ref cb) = inner.options.on_error {
                                        cb(&err);
                                    }
                                }
                            }
                        }
                    }
                }
                accept_done_tx.send_replace(true);
            });

            Ok(())
        }
        .await;

        if let Err(err) = start_outcome {
            let _ = self.cleanup_unix_paths().await;
            let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
            guard.started = false;
            return Err(err);
        }

        Ok(())
    }

    #[cfg(unix)]
    async fn cleanup_unix_paths(&self) -> Result<()> {
        let owned = {
            let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
            guard.owned_bind_path.take()
        };
        if let Some(bind_path) = owned {
            remove_path(&bind_path).await?;
        }
        cleanup_owned_socket(&self.inner).await
    }

    #[cfg(unix)]
    async fn close_unix(&self) -> Result<()> {
        let (shutdown_tx, accept_done_rx, connections) = {
            let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
            let shutdown_tx = guard.shutdown_tx.take();
            let accept_done_rx = guard.accept_done_rx.take();
            let connections: Vec<Arc<UnixByteConnection>> = guard.connections.values().cloned().collect();
            (shutdown_tx, accept_done_rx, connections)
        };
        if let Some(tx) = shutdown_tx {
            tx.send_replace(true);
        }
        let close_conns = futures::future::join_all(
            connections
                .into_iter()
                .map(|conn| async move { conn.close(None).await }),
        );
        let wait_accept = async move {
            if let Some(mut rx) = accept_done_rx {
                let _ = rx.wait_for(|done| *done).await;
            }
        };
        let (conn_results, ()) = tokio::join!(close_conns, wait_accept);
        for res in conn_results {
            if let Err(err) = res {
                self.report_error(&err);
            }
        }
        self.cleanup_unix_paths().await?;
        let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
        guard.connections.clear();
        guard.started = false;
        Ok(())
    }

    #[cfg(windows)]
    async fn start_windows(&self, accept: ByteConnectionAcceptor) -> Result<()> {
        use tokio::net::windows::named_pipe::ServerOptions as PipeServerOptions;

        {
            let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
            if guard.started {
                return Err(Error::msg("Unix listener is already started"));
            }
            if guard.closing {
                return Err(Error::msg("Unix listener is closing or closed"));
            }
            guard.started = true;
        }

        let pipe_name = windows_pipe_name(&self.inner.options.path);
        let first_server = match PipeServerOptions::new().first_pipe_instance(true).create(&pipe_name) {
            Ok(server) => server,
            Err(io_err) => {
                let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
                guard.started = false;
                return Err(Error::from_io(io_err, "listen", Some(&self.inner.options.path), None));
            }
        };

        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let (accept_done_tx, accept_done_rx) = watch::channel(false);
        {
            let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
            guard.shutdown_tx = Some(shutdown_tx);
            guard.accept_done_rx = Some(accept_done_rx);
        }

        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut current = first_server;
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx.wait_for(|done| *done) => break,
                    res = current.connect() => {
                        match res {
                            Ok(()) => {
                                let connected = current;
                                let (read_half, write_half) = tokio::io::split(connected);
                                UnixListener::spawn_stream_connection(&inner, &accept, read_half, write_half);
                                match PipeServerOptions::new().create(&pipe_name) {
                                    Ok(next) => current = next,
                                    Err(io_err) => {
                                        let err = Error::from_io(io_err, "listen", Some(&pipe_name), None);
                                        if let Some(ref cb) = inner.options.on_error {
                                            cb(&err);
                                        }
                                        break;
                                    }
                                }
                            }
                            Err(io_err) => {
                                let err = Error::from_io(io_err, "accept", Some(&pipe_name), None);
                                if let Some(ref cb) = inner.options.on_error {
                                    cb(&err);
                                }
                                break;
                            }
                        }
                    }
                }
            }
            accept_done_tx.send_replace(true);
        });

        Ok(())
    }

    #[cfg(windows)]
    async fn close_windows(&self) -> Result<()> {
        let (shutdown_tx, accept_done_rx, connections) = {
            let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
            let shutdown_tx = guard.shutdown_tx.take();
            let accept_done_rx = guard.accept_done_rx.take();
            let connections: Vec<Arc<UnixByteConnection>> = guard.connections.values().cloned().collect();
            (shutdown_tx, accept_done_rx, connections)
        };
        if let Some(tx) = shutdown_tx {
            tx.send_replace(true);
        }
        let close_conns = futures::future::join_all(
            connections
                .into_iter()
                .map(|conn| async move { conn.close(None).await }),
        );
        let wait_accept = async move {
            if let Some(mut rx) = accept_done_rx {
                let _ = rx.wait_for(|done| *done).await;
            }
        };
        let (conn_results, ()) = tokio::join!(close_conns, wait_accept);
        for res in conn_results {
            if let Err(err) = res {
                self.report_error(&err);
            }
        }
        let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
        guard.connections.clear();
        guard.started = false;
        Ok(())
    }
}

#[async_trait]
impl ServerListener for UnixListener {
    async fn start(&self, accept: ByteConnectionAcceptor) -> Result<()> {
        #[cfg(unix)]
        {
            self.start_unix(accept).await
        }
        #[cfg(windows)]
        {
            self.start_windows(accept).await
        }
    }

    async fn close(&self) -> Result<()> {
        enum CloseRole {
            Waiter(watch::Receiver<Option<std::result::Result<(), Arc<Error>>>>),
            Owner(watch::Sender<Option<std::result::Result<(), Arc<Error>>>>),
        }

        let role = {
            let mut guard = self.inner.state.lock().expect("listener mutex poisoned");
            if let Some(rx) = guard.close_rx.clone() {
                CloseRole::Waiter(rx)
            } else {
                guard.closing = true;
                let (tx, rx) = watch::channel(None);
                guard.close_rx = Some(rx);
                CloseRole::Owner(tx)
            }
        };

        let tx = match role {
            CloseRole::Waiter(mut rx) => {
                let _ = rx.wait_for(|val| val.is_some()).await;
                let outcome = rx.borrow().clone().expect("watch reported Some before borrow");
                return match outcome {
                    Ok(()) => Ok(()),
                    Err(shared) => Err(crate::errors::shared_js_error(shared)),
                };
            }
            CloseRole::Owner(tx) => tx,
        };

        #[cfg(unix)]
        let res = self.close_unix().await;
        #[cfg(windows)]
        let res = self.close_windows().await;

        match res {
            Ok(()) => {
                tx.send_replace(Some(Ok(())));
                Ok(())
            }
            Err(err) => {
                let shared = Arc::new(err);
                tx.send_replace(Some(Err(shared.clone())));
                Err(crate::errors::shared_js_error(shared))
            }
        }
    }
}

pub fn create_unix_listener(options: UnixListenerOptions) -> Result<Arc<dyn ServerListener>> {
    Ok(Arc::new(UnixListener::new(options)?))
}

#[cfg(unix)]
fn get_owned_bind_path(path: &str) -> String {
    let hex = sha256_hex(path.as_bytes());
    let suffix = &hex[..8];
    pi_js::path::join(&[&pi_js::path::dirname(path), &format!("bind-{suffix}")])
}

#[cfg(unix)]
async fn remove_stale_socket(path: &str) -> Result<()> {
    let original = match pi_js::fs::promises::lstat(path).await {
        Ok(stats) => stats,
        Err(err) if err.code() == Some("ENOENT") => return Ok(()),
        Err(err) => return Err(err),
    };
    if !original.is_socket() {
        return Err(Error::msg(format!(
            "Refusing to remove non-socket Unix listener path: {path}"
        )));
    }
    if is_socket_live(path).await? {
        return Err(Error::msg(format!("Unix listener is already running: {path}")));
    }
    let preserved = pi_js::path::join(&[
        &pi_js::path::dirname(path),
        &format!("stale-{}", &pi_js::crypto::random_uuid()[..6]),
    ]);
    match pi_js::fs::promises::rename(path, &preserved).await {
        Ok(()) => {}
        Err(err) if err.code() == Some("ENOENT") => return Ok(()),
        Err(err) => return Err(err),
    }
    let current = pi_js::fs::promises::lstat(&preserved).await?;
    if !current.is_socket() || current.dev != original.dev || current.ino != original.ino {
        match pi_js::fs::promises::lstat(path).await {
            Ok(_) => {}
            Err(err) if err.code() == Some("ENOENT") => {
                pi_js::fs::promises::rename(&preserved, path).await?;
            }
            Err(err) => return Err(err),
        }
        return Err(Error::msg(format!(
            "Unix listener path changed while checking for a stale socket: {path}"
        )));
    }
    remove_path(&preserved).await
}

#[cfg(unix)]
async fn cleanup_owned_socket(inner: &Arc<UnixListenerInner>) -> Result<()> {
    let identity = {
        let mut guard = inner.state.lock().expect("listener mutex poisoned");
        guard.socket_identity.take()
    };
    let Some(identity) = identity else {
        return Ok(());
    };
    let path = &inner.options.path;
    let current = match pi_js::fs::promises::lstat(path).await {
        Ok(stats) => stats,
        Err(err) if err.code() == Some("ENOENT") => return Ok(()),
        Err(err) => return Err(err),
    };
    if !current.is_socket() || current.dev != identity.dev || current.ino != identity.ino {
        return Ok(());
    }
    let preserved = pi_js::path::join(&[
        &pi_js::path::dirname(path),
        &format!("cleanup-{}", &pi_js::crypto::random_uuid()[..6]),
    ]);
    match pi_js::fs::promises::rename(path, &preserved).await {
        Ok(()) => {}
        Err(err) if err.code() == Some("ENOENT") => return Ok(()),
        Err(err) => return Err(err),
    }
    let moved = pi_js::fs::promises::lstat(&preserved).await?;
    if moved.is_socket() && moved.dev == identity.dev && moved.ino == identity.ino {
        return remove_path(&preserved).await;
    }
    match pi_js::fs::promises::lstat(path).await {
        Ok(_) => {}
        Err(err) if err.code() == Some("ENOENT") => {
            pi_js::fs::promises::rename(&preserved, path).await?;
        }
        Err(err) => return Err(err),
    }
    Err(Error::msg(format!(
        "Unix listener path changed during cleanup; preserved replacement at {preserved}"
    )))
}

#[cfg(unix)]
async fn remove_path(path: &str) -> Result<()> {
    match pi_js::fs::promises::unlink(path).await {
        Ok(()) => Ok(()),
        Err(err) if err.code() == Some("ENOENT") => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
async fn is_socket_live(path: &str) -> Result<bool> {
    match tokio::time::timeout(
        std::time::Duration::from_millis(SOCKET_PROBE_TIMEOUT_MS),
        tokio::net::UnixStream::connect(path),
    )
    .await
    {
        Ok(Ok(_stream)) => Ok(true),
        Ok(Err(io_err)) => {
            let err = Error::from_io(io_err, "connect", Some(path), None);
            if matches!(
                err.code(),
                Some("ECONNREFUSED") | Some("ENOENT") | Some("EPIPE") | Some("ECONNRESET")
            ) {
                Ok(false)
            } else {
                Err(err)
            }
        }
        Err(_elapsed) => Ok(true),
    }
}

#[cfg(unix)]
async fn set_socket_mode(path: &str, mode: u32) -> Result<()> {
    if pi_js::env::platform() == "win32" {
        return Ok(());
    }
    match pi_js::fs::promises::chmod(path, mode).await {
        Ok(()) => Ok(()),
        Err(err) if matches!(err.code(), Some("ENOSYS") | Some("ENOTSUP")) => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(windows)]
fn windows_pipe_name(path: &str) -> String {
    if path.starts_with(r"\\.\pipe\") {
        return path.to_string();
    }
    let hex = sha256_hex(path.as_bytes());
    format!(r"\\.\pipe\pi-server-{}", &hex[..16])
}

/// Connect to a local Unix socket (or Windows named pipe) endpoint for testing or local transport clients.
pub async fn connect_unix_stream_socket(path: &str) -> Result<impl AsyncRead + AsyncWrite + Unpin + Send + use<>> {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(path)
            .await
            .map_err(|e| Error::from_io(e, "connect", Some(path), None))
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        let pipe_name = windows_pipe_name(path);
        ClientOptions::new()
            .open(&pipe_name)
            .map_err(|e| Error::from_io(e, "connect", Some(path), None))
    }
}

fn resolve_unix_listener_options(options: UnixListenerOptions) -> Result<ResolvedUnixListenerOptions> {
    if options.path.is_empty() {
        return Err(Error::js("TypeError", "Server Unix socket path must not be empty"));
    }
    let mode = options.mode.unwrap_or(DEFAULT_SOCKET_MODE);
    if mode > 0o777 {
        return Err(Error::js(
            "TypeError",
            "Server Unix socket mode must be an integer between 0 and 0o777",
        ));
    }
    let max_frame_length = options.max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    if max_frame_length <= 0 || max_frame_length > MAX_UINT32 {
        return Err(Error::js(
            "TypeError",
            format!("Server maxFrameLength must be an integer between 1 and {MAX_UINT32}"),
        ));
    }
    let max_pending_bytes = options.max_pending_bytes.unwrap_or(max_frame_length * 4);
    if max_pending_bytes < max_frame_length + 4 {
        return Err(Error::js(
            "TypeError",
            "Server maxPendingBytes must be a safe integer at least maxFrameLength + 4",
        ));
    }
    let graceful_close_timeout_ms = options
        .graceful_close_timeout_ms
        .unwrap_or(DEFAULT_GRACEFUL_CLOSE_TIMEOUT_MS);
    if graceful_close_timeout_ms <= 0 || graceful_close_timeout_ms > MAX_TIMER_DELAY_MS {
        return Err(Error::js(
            "TypeError",
            format!("Server gracefulCloseTimeoutMs must be an integer between 1 and {MAX_TIMER_DELAY_MS}"),
        ));
    }
    Ok(ResolvedUnixListenerOptions {
        path: options.path,
        mode,
        graceful_close_timeout_ms,
        max_pending_bytes,
        on_error: options.on_error,
    })
}

//! Port of packages/server/test/server.test.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_js::{Error, Result};
use pi_protocol::{ProtocolError, ServerHelloError, ServerMessage, ServerMessageDecoder};
use pi_server::connection::{ByteConnection, ByteConnectionAcceptor};
use pi_server::testing::TestServerHost;
use pi_server::transports::unix::{UnixServerOptions, create_unix_server};
use pi_server::{Server, ServerListener, ServerOptions};
use tempfile::TempDir;
use tokio::sync::Notify;

fn server_id() -> String {
    "00000000-0000-4000-8000-000000000001".to_string()
}

fn make_socket_path() -> (TempDir, String) {
    let dir = tempfile::Builder::new()
        .prefix("pss-")
        .tempdir_in("/tmp")
        .expect("tempdir in /tmp");
    let path = dir.path().join("server.sock").to_string_lossy().into_owned();
    (dir, path)
}

#[test]
fn requires_explicit_listeners_and_a_canonical_uuidv4_server_identity() {
    let host = TestServerHost::new();
    let empty_err = Server::new(
        host.clone(),
        ServerOptions {
            listeners: Vec::new(),
            server_id: String::new(),
            ..Default::default()
        },
    )
    .err()
    .expect("empty server_id should fail");
    assert!(empty_err.message().contains("serverId"));

    let invalid_err = Server::new(
        host,
        ServerOptions {
            listeners: Vec::new(),
            server_id: "invalid-server".to_string(),
            ..Default::default()
        },
    )
    .err()
    .expect("invalid server_id should fail");
    assert!(invalid_err.message().contains("serverId"));
}

#[tokio::test]
async fn rejects_concurrent_start_calls_without_leaking_the_unix_listener() {
    let host = TestServerHost::new();
    let (_dir, path) = make_socket_path();
    let server = create_unix_server(
        host,
        UnixServerOptions {
            path: path.clone(),
            server_id: server_id(),
            ..Default::default()
        },
    )
    .expect("create unix server");

    let starting = server.start();
    let concurrent = server.start().await;
    let err = concurrent.err().expect("concurrent start must fail");
    assert!(err.message().contains("starting"));

    let started = starting.await.expect("initial start completes");
    started.close().await.expect("close server");
    let stat_err = pi_js::fs::promises::lstat(&path)
        .await
        .expect_err("socket path should be removed");
    assert_eq!(stat_err.code(), Some("ENOENT"));
}

#[tokio::test]
async fn handshake_timeout_closes_with_a_final_hello_error_frame() {
    struct TimedOutConnection {
        closed: AtomicBool,
        final_chunk: Mutex<Option<Vec<u8>>>,
        closed_notify: Notify,
    }

    #[async_trait]
    impl ByteConnection for TimedOutConnection {
        fn closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }

        async fn send(&self, _chunk: &[u8]) -> Result<()> {
            Err(Error::msg("handshake timeout must use the terminal close frame"))
        }

        async fn close(&self, final_chunk: Option<&[u8]>) -> Result<()> {
            *self.final_chunk.lock().expect("final_chunk mutex poisoned") = final_chunk.map(|b| b.to_vec());
            self.closed.store(true, Ordering::SeqCst);
            self.closed_notify.notify_waiters();
            Ok(())
        }
    }

    let host = TestServerHost::new();
    let core = Server::new(
        host,
        ServerOptions {
            listeners: Vec::new(),
            server_id: server_id(),
            max_frame_length: Some(1024),
            handshake_timeout_ms: Some(10),
            ..Default::default()
        },
    )
    .expect("create server");

    let connection = Arc::new(TimedOutConnection {
        closed: AtomicBool::new(false),
        final_chunk: Mutex::new(None),
        closed_notify: Notify::new(),
    });
    let notified = connection.closed_notify.notified();
    let _handler = core.accept(connection.clone());

    notified.await;
    assert!(connection.closed());
    let final_chunk = connection
        .final_chunk
        .lock()
        .expect("final_chunk mutex poisoned")
        .clone()
        .expect("final close frame present");
    let messages = ServerMessageDecoder::new(None)
        .push(&final_chunk)
        .expect("decode final frame");
    assert_eq!(messages.len(), 1);
    match &messages[0] {
        ServerMessage::HelloError(ServerHelloError {
            error: ProtocolError { code, .. },
        }) => assert_eq!(code, "invalid_request"),
        other => panic!("Expected hello_error, got {other:?}"),
    }
    core.close().await.expect("close server");
}

#[test]
fn rejects_timeout_values_above_nodes_maximum_timer_delay() {
    let host = TestServerHost::new();
    let path = "/tmp/pi-server-timeout-test.sock".to_string();

    let handshake_err = create_unix_server(
        host.clone(),
        UnixServerOptions {
            path: path.clone(),
            server_id: server_id(),
            handshake_timeout_ms: Some(2_147_483_648),
            ..Default::default()
        },
    )
    .err()
    .expect("oversized handshakeTimeoutMs should fail");
    assert!(handshake_err.message().contains("handshakeTimeoutMs"));

    let graceful_err = create_unix_server(
        host,
        UnixServerOptions {
            path,
            server_id: server_id(),
            graceful_close_timeout_ms: Some(2_147_483_648),
            ..Default::default()
        },
    )
    .err()
    .expect("oversized gracefulCloseTimeoutMs should fail");
    assert!(graceful_err.message().contains("gracefulCloseTimeoutMs"));
}

#[test]
fn rejects_pending_byte_limits_smaller_than_one_maximum_frame() {
    let host = TestServerHost::new();
    let (_dir, path) = make_socket_path();
    let err = create_unix_server(
        host,
        UnixServerOptions {
            path,
            server_id: server_id(),
            max_frame_length: Some(128),
            max_pending_bytes: Some(131),
            ..Default::default()
        },
    )
    .err()
    .expect("small maxPendingBytes should fail");
    assert!(err.message().contains("maxPendingBytes"));
}

#[tokio::test]
async fn rejects_close_and_closed_when_listener_shutdown_fails() {
    struct FailingCloseListener;

    #[async_trait]
    impl ServerListener for FailingCloseListener {
        async fn start(&self, _accept: ByteConnectionAcceptor) -> Result<()> {
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Err(Error::msg("listener close failed"))
        }
    }

    let host = TestServerHost::new();
    let core = Server::new(
        host,
        ServerOptions {
            listeners: vec![Arc::new(FailingCloseListener)],
            server_id: server_id(),
            ..Default::default()
        },
    )
    .expect("create server");
    core.start().await.expect("start server");

    let close_err = core.close().await.expect_err("close should fail");
    assert_eq!(close_err.message(), "listener close failed");
    let closed_err = core.closed().await.expect_err("closed should fail");
    assert_eq!(closed_err.message(), "listener close failed");
}

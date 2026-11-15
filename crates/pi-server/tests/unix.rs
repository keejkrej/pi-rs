//! Port of packages/server/test/unix.test.ts

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use pi_js::fs::promises::{lstat, read_file, unlink, write_file};
use pi_js::path::{dirname, join};
use pi_protocol::ServerMessage;
use pi_server::Server;
use pi_server::testing::{TestServerHost, connect_unix_test_client};
use pi_server::transports::unix::{UnixServerOptions, create_unix_server, get_unix_socket_path};
use tempfile::TempDir;

const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";

fn make_socket_path(nested: bool) -> (TempDir, String) {
    let dir = tempfile::Builder::new()
        .prefix("ps-")
        .tempdir_in("/tmp")
        .expect("tempdir in /tmp");
    let base = dir.path().to_string_lossy().into_owned();
    let path = if nested {
        join(&[&base, "p", "n", "server.sock"])
    } else {
        join(&[&base, "server.sock"])
    };
    (dir, path)
}

fn make_server(path: &str) -> Server {
    create_unix_server(
        TestServerHost::new(),
        UnixServerOptions {
            path: path.to_string(),
            server_id: SERVER_ID.to_string(),
            ..Default::default()
        },
    )
    .expect("create unix server")
}

#[tokio::test]
async fn creates_an_in_memory_server_id_and_derives_its_explicit_unix_socket_path() {
    let dir = tempfile::Builder::new()
        .prefix("pi-server-")
        .tempdir_in("/tmp")
        .expect("tempdir in /tmp");
    let directory = dir.path().to_string_lossy().into_owned();
    let path = get_unix_socket_path(SERVER_ID, &directory).expect("socket path");
    assert_eq!(path, join(&[&directory, &format!("{SERVER_ID}.sock")]));

    let first = make_server(&path);
    first.start().await.expect("start first");
    let first_client = connect_unix_test_client(&path).await.expect("connect first client");
    match first_client.hello(None).await.expect("first hello") {
        ServerMessage::Hello(h) => assert_eq!(h.server_id, SERVER_ID),
        other => panic!("Expected hello, got {other:?}"),
    }
    first_client.close().await.expect("close first client");
    first.close().await.expect("close first server");

    let replacement = make_server(&path);
    replacement.start().await.expect("start replacement");
    let replacement_client = connect_unix_test_client(&path)
        .await
        .expect("connect replacement client");
    match replacement_client.hello(None).await.expect("replacement hello") {
        ServerMessage::Hello(h) => assert_eq!(h.server_id, SERVER_ID),
        other => panic!("Expected hello, got {other:?}"),
    }
    replacement_client.close().await.expect("close replacement client");
    replacement.close().await.expect("close replacement server");
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_a_live_listener_without_unlinking_it() {
    let (_dir, path) = make_socket_path(false);
    let first = make_server(&path);
    first.start().await.expect("start first");
    let first_identity = lstat(&path).await.expect("lstat first");

    let second = make_server(&path);
    let err = second.start().await.expect_err("second start must fail while live");
    assert!(err.message().contains("already running"));

    let current_identity = lstat(&path).await.expect("lstat current");
    assert!(current_identity.is_socket());
    assert_eq!(
        (current_identity.dev, current_identity.ino),
        (first_identity.dev, first_identity.ino)
    );

    let client = connect_unix_test_client(&path).await.expect("connect to live server");
    match client.hello(None).await.expect("hello") {
        ServerMessage::Hello(_) => {}
        other => panic!("Expected hello, got {other:?}"),
    }
    client.close().await.expect("close client");
    first.close().await.expect("close first");
}

#[cfg(unix)]
#[tokio::test]
async fn never_unlinks_a_regular_file_at_the_configured_path() {
    let (_dir, path) = make_socket_path(false);
    write_file(&path, b"do not remove", Some(0o640))
        .await
        .expect("write regular file");
    let server = make_server(&path);
    let err = server.start().await.expect_err("non-socket must fail");
    assert!(err.message().contains("non-socket"));
    let content = read_file(&path).await.expect("read preserved file");
    assert_eq!(content, b"do not remove");
}

#[cfg(unix)]
#[tokio::test]
async fn creates_nested_temp_parents_restricts_permissions_and_removes_its_own_socket() {
    let (_dir, path) = make_socket_path(true);
    let server = make_server(&path);
    server.start().await.expect("start server");

    let stats = lstat(&path).await.expect("lstat socket");
    assert!(stats.is_socket());
    if pi_js::env::platform() != "win32" {
        assert_eq!(stats.mode & 0o777, 0o600);
    }
    let parent = dirname(&path);
    let mut entries = std::fs::read_dir(&parent)
        .expect("readdir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries, vec!["server.sock"]);

    server.close().await.expect("close server");
    let stat_err = lstat(&path).await.expect_err("socket should be unlinked on close");
    assert_eq!(stat_err.code(), Some("ENOENT"));
}

#[cfg(unix)]
#[tokio::test]
async fn does_not_remove_a_replacement_inode_during_shutdown() {
    let (_dir, path) = make_socket_path(false);
    let server = make_server(&path);
    server.start().await.expect("start server");
    unlink(&path).await.expect("unlink original socket");
    write_file(&path, b"replacement", None)
        .await
        .expect("write replacement file");

    let srv = server.clone();
    let closing = tokio::spawn(async move { srv.close().await });
    assert_eq!(read_file(&path).await.expect("read during close"), b"replacement");
    closing.await.expect("close task").expect("close server");
    assert_eq!(read_file(&path).await.expect("read after close"), b"replacement");
}

#[cfg(unix)]
#[tokio::test]
async fn removes_a_genuinely_stale_socket_before_binding() {
    let (_dir, path) = make_socket_path(false);
    let bin = env!("CARGO_BIN_EXE_fixture-stale-socket-server");
    let mut child = Command::new(bin)
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn stale socket fixture");

    {
        let stdout = child.stdout.take().expect("child stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read fixture ready line");
        assert_eq!(line.trim(), "listening");
    }

    let stale_identity = lstat(&path).await.expect("lstat stale socket");
    assert!(stale_identity.is_socket());
    child.kill().expect("kill fixture process");
    let _ = child.wait().expect("wait fixture exit");

    let server = make_server(&path);
    server.start().await.expect("start server over stale socket");
    let live_identity = lstat(&path).await.expect("lstat live socket");
    assert!(live_identity.is_socket());

    let client = connect_unix_test_client(&path).await.expect("connect to new server");
    match client.hello(None).await.expect("hello") {
        ServerMessage::Hello(_) => {}
        other => panic!("Expected hello, got {other:?}"),
    }
    client.close().await.expect("close client");
    server.close().await.expect("close server");
}

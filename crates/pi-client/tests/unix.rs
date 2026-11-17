//! Port of packages/client/test/unix.test.ts

#[cfg(unix)]
mod unix_tests {
    use std::path::Path;
    use std::process::Stdio;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pi_client::unix::{DiscoverUnixServersOptions, UnixServerRoute, discover_unix_servers};
    use pi_protocol::{
        ClientMessage, ClientMessageDecoder, PROTOCOL_VERSION, ServerHello, ServerMessage, encode_server_message,
    };
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::process::Command;
    use tokio::task::JoinHandle;

    fn server_id(value: usize) -> String {
        format!("00000000-0000-4000-8000-{value:012x}")
    }

    struct SpawnedServer {
        task: JoinHandle<()>,
    }

    impl Drop for SpawnedServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn start_handshake_server(
        directory: &Path,
        file_server_id: &str,
        reported_server_id: Option<&str>,
    ) -> SpawnedServer {
        let reported = reported_server_id.unwrap_or(file_server_id).to_string();
        let sock_path = directory.join(format!("{file_server_id}.sock"));
        let listener = UnixListener::bind(&sock_path).unwrap();
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let reported = reported.clone();
                tokio::spawn(async move {
                    let decoder = ClientMessageDecoder::new(None);
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = socket.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        if let Ok(messages) = decoder.push(&buf[..n]) {
                            for msg in messages {
                                if matches!(msg, ClientMessage::Hello(_))
                                    && let Ok(frame) = encode_server_message(
                                        &ServerMessage::Hello(ServerHello {
                                            version: PROTOCOL_VERSION,
                                            server_id: reported.clone(),
                                        }),
                                        None,
                                    )
                                {
                                    let _ = socket.write_all(&frame).await;
                                }
                            }
                        }
                    }
                });
            }
        });
        SpawnedServer { task }
    }

    struct SilentCounters {
        active: AtomicUsize,
        maximum: AtomicUsize,
        total: AtomicUsize,
    }

    async fn start_silent_socket(path: &Path, counters: Option<Arc<SilentCounters>>) -> SpawnedServer {
        let listener = UnixListener::bind(path).unwrap();
        let task = tokio::spawn(async move {
            let mut open_sockets = Vec::new();
            while let Ok((mut socket, _)) = listener.accept().await {
                if let Some(ref c) = counters {
                    let cur = c.active.fetch_add(1, Ordering::AcqRel) + 1;
                    c.total.fetch_add(1, Ordering::AcqRel);
                    let mut prev = c.maximum.load(Ordering::Acquire);
                    while cur > prev {
                        match c
                            .maximum
                            .compare_exchange(prev, cur, Ordering::AcqRel, Ordering::Acquire)
                        {
                            Ok(_) => break,
                            Err(actual) => prev = actual,
                        }
                    }
                    let c_drop = Arc::clone(c);
                    let handle = tokio::spawn(async move {
                        let mut buf = [0u8; 64];
                        while let Ok(n) = socket.read(&mut buf).await {
                            if n == 0 {
                                break;
                            }
                        }
                        c_drop.active.fetch_sub(1, Ordering::AcqRel);
                    });
                    open_sockets.push(handle);
                } else {
                    let handle = tokio::spawn(async move {
                        let mut buf = [0u8; 64];
                        while let Ok(n) = socket.read(&mut buf).await {
                            if n == 0 {
                                break;
                            }
                        }
                    });
                    open_sockets.push(handle);
                }
            }
        });
        SpawnedServer { task }
    }

    #[tokio::test]
    async fn returns_no_routes_when_the_server_directory_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let routes = discover_unix_servers(DiscoverUnixServersOptions {
            directory: missing.to_string_lossy().into_owned(),
            timeout_ms: None,
        })
        .await
        .unwrap();
        assert!(routes.is_empty());
    }

    #[tokio::test]
    async fn discovers_reachable_servers_in_server_id_order() {
        let dir = tempfile::tempdir().unwrap();
        let first = server_id(1);
        let second = server_id(2);
        let _s2 = start_handshake_server(dir.path(), &second, None).await;
        let _s1 = start_handshake_server(dir.path(), &first, None).await;

        let routes = discover_unix_servers(DiscoverUnixServersOptions {
            directory: dir.path().to_string_lossy().into_owned(),
            timeout_ms: None,
        })
        .await
        .unwrap();

        assert_eq!(
            routes,
            vec![
                UnixServerRoute {
                    server_id: first.clone(),
                    path: dir.path().join(format!("{first}.sock")).to_string_lossy().into_owned(),
                },
                UnixServerRoute {
                    server_id: second.clone(),
                    path: dir.path().join(format!("{second}.sock")).to_string_lossy().into_owned(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn ignores_malformed_entries_non_sockets_and_mismatched_servers() {
        let dir = tempfile::tempdir().unwrap();
        pi_js::fs::promises::write_file(
            &dir.path().join(format!("{}.sock", server_id(1))).to_string_lossy(),
            b"not a socket",
            None,
        )
        .await
        .unwrap();
        pi_js::fs::promises::write_file(
            &dir.path().join("not-a-server.sock").to_string_lossy(),
            b"ignored",
            None,
        )
        .await
        .unwrap();
        pi_js::fs::promises::mkdir(
            &dir.path().join(format!("{}.sock", server_id(2))).to_string_lossy(),
            false,
            None,
        )
        .await
        .unwrap();
        let _s3 = start_handshake_server(dir.path(), &server_id(3), Some(&server_id(4))).await;

        let routes = discover_unix_servers(DiscoverUnixServersOptions {
            directory: dir.path().to_string_lossy().into_owned(),
            timeout_ms: None,
        })
        .await
        .unwrap();
        assert!(routes.is_empty());
    }

    #[tokio::test]
    async fn ignores_stale_sockets_without_deleting_them() {
        let dir = tempfile::tempdir().unwrap();
        let id = server_id(1);
        let sock_path = dir.path().join(format!("{id}.sock"));
        let bin = env!("CARGO_BIN_EXE_fixture-stale-socket-server");
        let mut child = Command::new(bin)
            .arg(&sock_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).await.unwrap();
        assert_eq!(line.trim(), "listening");

        child.kill().await.unwrap();
        let _ = child.wait().await;

        let routes = discover_unix_servers(DiscoverUnixServersOptions {
            directory: dir.path().to_string_lossy().into_owned(),
            timeout_ms: None,
        })
        .await
        .unwrap();
        assert!(routes.is_empty());
        assert!(
            pi_js::fs::promises::lstat(&sock_path.to_string_lossy())
                .await
                .unwrap()
                .is_socket()
        );
    }

    #[tokio::test]
    async fn times_out_an_unresponsive_socket_without_deleting_it() {
        let dir = tempfile::tempdir().unwrap();
        let id = server_id(1);
        let sock_path = dir.path().join(format!("{id}.sock"));
        let _silent = start_silent_socket(&sock_path, None).await;

        let routes = discover_unix_servers(DiscoverUnixServersOptions {
            directory: dir.path().to_string_lossy().into_owned(),
            timeout_ms: Some(20),
        })
        .await
        .unwrap();
        assert!(routes.is_empty());
        assert!(
            pi_js::fs::promises::lstat(&sock_path.to_string_lossy())
                .await
                .unwrap()
                .is_socket()
        );
    }

    #[tokio::test]
    async fn limits_concurrent_probes_to_16() {
        let dir = tempfile::tempdir().unwrap();
        let counters = Arc::new(SilentCounters {
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
        });
        let mut servers = Vec::new();
        for index in 1..=20 {
            let sock_path = dir.path().join(format!("{}.sock", server_id(index)));
            servers.push(start_silent_socket(&sock_path, Some(Arc::clone(&counters))).await);
        }

        let directory = dir.path().to_string_lossy().into_owned();
        let discovery = tokio::spawn(async move {
            discover_unix_servers(DiscoverUnixServersOptions {
                directory,
                timeout_ms: Some(100),
            })
            .await
        });

        for _ in 0..50 {
            if counters.active.load(Ordering::Acquire) == 16 {
                break;
            }
            let _ = pi_js::time::sleep(5, None).await;
        }
        assert_eq!(counters.active.load(Ordering::Acquire), 16);
        assert_eq!(counters.maximum.load(Ordering::Acquire), 16);

        let routes = discovery.await.unwrap().unwrap();
        assert!(routes.is_empty());
        assert_eq!(counters.total.load(Ordering::Acquire), 20);
    }

    #[tokio::test]
    async fn ignores_an_endpoint_that_closes_before_its_handshake() {
        let dir = tempfile::tempdir().unwrap();
        let id = server_id(1);
        let sock_path = dir.path().join(format!("{id}.sock"));
        let listener = UnixListener::bind(&sock_path).unwrap();
        let task = tokio::spawn(async move {
            if let Ok((socket, _)) = listener.accept().await {
                drop(socket);
            }
        });

        let routes = discover_unix_servers(DiscoverUnixServersOptions {
            directory: dir.path().to_string_lossy().into_owned(),
            timeout_ms: None,
        })
        .await
        .unwrap();
        assert!(routes.is_empty());
        task.abort();
    }

    #[tokio::test]
    async fn propagates_unexpected_filesystem_errors() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-directory");
        pi_js::fs::promises::write_file(&file.to_string_lossy(), b"content", None)
            .await
            .unwrap();

        let err = discover_unix_servers(DiscoverUnixServersOptions {
            directory: file.to_string_lossy().into_owned(),
            timeout_ms: None,
        })
        .await
        .err()
        .unwrap();
        assert_eq!(err.code(), Some("ENOTDIR"));
    }
}

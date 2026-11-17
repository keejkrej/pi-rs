//! Port of packages/client/test/unix-transport.test.ts

use pi_client::unix::{UnixTransportOptions, create_unix_transport_factory};

#[test]
fn rejects_invalid_unix_transport_options() {
    let empty_err = create_unix_transport_factory(UnixTransportOptions {
        path: String::new(),
        max_pending_bytes: None,
    })
    .err()
    .unwrap();
    assert!(empty_err.message().contains("must not be empty"));

    let zero_err = create_unix_transport_factory(UnixTransportOptions {
        path: "/tmp/pi.sock".to_string(),
        max_pending_bytes: Some(0),
    })
    .err()
    .unwrap();
    assert!(zero_err.message().contains("positive"));
}

#[cfg(unix)]
mod unix_transport_tests {
    use std::sync::{Arc, Mutex};

    use pi_chord::{ServiceCall, parse_service_call};
    use pi_client::unix::{UnixTransportOptions, create_unix_transport_factory};
    use pi_client::{ByteTransportHandlers, Client, ClientOptions};
    use pi_protocol::protocol::ServerTarget;
    use pi_protocol::{
        ClientMessage, ClientMessageDecoder, PROTOCOL_VERSION, ResponseEnvelope, RpcTarget, ServerHello, ServerMessage,
        encode_server_message,
    };
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";

    #[tokio::test]
    async fn carries_a_complete_client_handshake_and_request_over_a_real_unix_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pi.sock");
        let received_members = Arc::new(Mutex::new(Vec::<String>::new()));
        let received_clone = Arc::clone(&received_members);

        let listener = UnixListener::bind(&path).unwrap();
        let server_task = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let decoder = ClientMessageDecoder::new(None);
                let mut buf = [0u8; 4096];
                while let Ok(n) = socket.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    for message in decoder.push(&buf[..n]).unwrap() {
                        match message {
                            ClientMessage::Hello(_) => {
                                let frame = encode_server_message(
                                    &ServerMessage::Hello(ServerHello {
                                        version: PROTOCOL_VERSION,
                                        server_id: SERVER_ID.to_string(),
                                    }),
                                    None,
                                )
                                .unwrap();
                                for byte in frame {
                                    let _ = socket.write_all(&[byte]).await;
                                }
                            }
                            ClientMessage::Cancel(_) => continue,
                            ClientMessage::Request(req) => {
                                let call = parse_service_call(&req.call).unwrap();
                                received_clone
                                    .lock()
                                    .unwrap()
                                    .push(format!("{}.{}", call.service_id, call.member));
                                let frame = encode_server_message(
                                    &ServerMessage::Response(ResponseEnvelope {
                                        id: req.id,
                                        ok: true,
                                        result: Some(json!([])),
                                        error: None,
                                    }),
                                    None,
                                )
                                .unwrap();
                                let split = frame.len() / 2;
                                let _ = socket.write_all(&frame[..split]).await;
                                let _ = socket.write_all(&frame[split..]).await;
                            }
                        }
                    }
                }
            }
        });

        let client = Client::new(ClientOptions {
            server_id: SERVER_ID.to_string(),
            transport_factory: create_unix_transport_factory(UnixTransportOptions {
                path: path.to_string_lossy().into_owned(),
                max_pending_bytes: None,
            })
            .unwrap(),
            max_frame_length: None,
            on_listener_error: None,
        })
        .unwrap();

        let hello = client.connect().await.unwrap();
        assert_eq!(hello.server_id, SERVER_ID);

        let res = client
            .request(
                RpcTarget::Server(ServerTarget {
                    server_id: SERVER_ID.to_string(),
                }),
                ServiceCall {
                    service_id: "test.server".to_string(),
                    instance: None,
                    member: "list".to_string(),
                    args: vec![],
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(res, Some(json!([])));
        assert_eq!(*received_members.lock().unwrap(), vec!["test.server.list"]);

        client.dispose().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn reports_truncated_final_frames_through_client() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pi.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server_task = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let decoder = ClientMessageDecoder::new(None);
                let mut buf = [0u8; 4096];
                while let Ok(n) = socket.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    for message in decoder.push(&buf[..n]).unwrap() {
                        if matches!(message, ClientMessage::Hello(_)) {
                            let frame = encode_server_message(
                                &ServerMessage::Hello(ServerHello {
                                    version: PROTOCOL_VERSION,
                                    server_id: SERVER_ID.to_string(),
                                }),
                                None,
                            )
                            .unwrap();
                            let _ = socket.write_all(&frame).await;
                        } else {
                            let _ = socket.write_all(&[0, 0, 0, 2, 1]).await;
                            let _ = socket.shutdown().await;
                            return;
                        }
                    }
                }
            }
        });

        let client = Client::new(ClientOptions {
            server_id: SERVER_ID.to_string(),
            transport_factory: create_unix_transport_factory(UnixTransportOptions {
                path: path.to_string_lossy().into_owned(),
                max_pending_bytes: None,
            })
            .unwrap(),
            max_frame_length: None,
            on_listener_error: None,
        })
        .unwrap();

        client.connect().await.unwrap();
        let err = client
            .request(
                RpcTarget::Server(ServerTarget {
                    server_id: SERVER_ID.to_string(),
                }),
                ServiceCall {
                    service_id: "test.server".to_string(),
                    instance: None,
                    member: "list".to_string(),
                    args: vec![],
                },
                None,
            )
            .await
            .err()
            .unwrap();
        assert_eq!(err.name(), "ProtocolValidationError");
        assert!(err.message().to_lowercase().contains("truncated"));
        assert_eq!(client.connection_state().as_str(), "disconnected");

        client.dispose().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn rejects_connection_attempts_to_missing_sockets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pi.sock");
        let factory = create_unix_transport_factory(UnixTransportOptions {
            path: path.to_string_lossy().into_owned(),
            max_pending_bytes: None,
        })
        .unwrap();
        let err = factory(ByteTransportHandlers::new(|_| {}, || {}, |_| {}))
            .await
            .err()
            .unwrap();
        assert_eq!(err.code(), Some("ENOENT"));
    }
}

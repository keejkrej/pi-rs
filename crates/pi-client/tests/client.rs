//! Port of packages/client/test/client.test.ts

#[allow(dead_code)]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::{ServiceCall, ServiceMode, ServiceProviderUpdate};
use pi_client::{
    ByteTransport, ByteTransportFactory, Client, ClientDisposedError, ClientOptions, DisconnectedError, ServerError,
    create_client_service_transport,
};
use pi_js::abort::{AbortController, AbortReason};
use pi_protocol::protocol::ServerTarget;
use pi_protocol::{
    AttachmentEnvelope, ClientMessage, PROTOCOL_VERSION, ProtocolError, ResponseEnvelope, RpcTarget, ServerHello,
    ServerHelloError, ServerMessage, ServiceEventEnvelope, SessionTarget, encode_cbor, encode_frame,
    encode_server_message,
};
use serde_json::{Value, json};
use support::support::MemoryByteServer;

const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";

fn server_target() -> RpcTarget {
    RpcTarget::Server(ServerTarget {
        server_id: SERVER_ID.to_string(),
    })
}

async fn connect_client(server: &MemoryByteServer, expected_server_id: &str) -> pi_js::Result<Client> {
    let srv = server.clone();
    Client::connect_options(ClientOptions {
        server_id: expected_server_id.to_string(),
        transport_factory: Arc::new(move |handlers| {
            let srv = srv.clone();
            Box::pin(async move { Ok(srv.connect(handlers)) })
        }),
        max_frame_length: None,
        on_listener_error: None,
    })
    .await
}

async fn attach_client(client: &Client, server: &MemoryByteServer, session_id: &str) {
    let expected_messages = server.messages().len() + 1;
    let attaching = client.request(
        server_target(),
        ServiceCall {
            service_id: "pi.session-management".to_string(),
            instance: None,
            member: "attach".to_string(),
            args: vec![Value::String(session_id.to_string())],
        },
        None,
    );
    server.wait_for_messages(expected_messages).await;
    let messages = server.messages();
    let request = match messages.last() {
        Some(ClientMessage::Request(req)) => req.clone(),
        other => panic!("Missing attach request, got: {other:?}"),
    };
    server
        .send(ServerMessage::Attachment(AttachmentEnvelope {
            attachment: Some(SessionTarget {
                server_id: SERVER_ID.to_string(),
                session_id: session_id.to_string(),
                attachment_id: format!("attachment-{session_id}"),
            }),
        }))
        .unwrap();
    server
        .send(ServerMessage::Response(ResponseEnvelope {
            id: request.id,
            ok: true,
            result: Some(Value::Null),
            error: None,
        }))
        .unwrap();
    attaching.await.unwrap();
}

#[test]
fn requires_a_canonical_uuidv4_server_identity() {
    let factory: ByteTransportFactory = Arc::new(|_| Box::pin(async { Err(pi_js::Error::msg("unused")) }));
    let err = Client::new(ClientOptions {
        server_id: "invalid-server".to_string(),
        transport_factory: factory,
        max_frame_length: None,
        on_listener_error: None,
    })
    .err()
    .expect("expected TypeError for invalid serverId");
    assert_eq!(err.name(), "TypeError");
    assert!(err.message().contains("serverId"));
}

#[tokio::test]
async fn connects_only_to_the_expected_logical_server() {
    let matching = MemoryByteServer::new(None);
    let client = connect_client(&matching, SERVER_ID).await.unwrap();
    assert_eq!(client.hello().unwrap().server_id, SERVER_ID);
    client.dispose().await.unwrap();

    let wrong = MemoryByteServer::new(Some("00000000-0000-4000-8000-000000000002"));
    let err = connect_client(&wrong, SERVER_ID).await.err().unwrap();
    assert_eq!(err.name(), "ProtocolValidationError");
}

#[tokio::test]
async fn updates_attachment_state_from_out_of_band_server_routing() {
    let server = MemoryByteServer::new(None);
    let client = connect_client(&server, SERVER_ID).await.unwrap();
    let changes = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let changes_clone = Arc::clone(&changes);
    let _unsub = client
        .on_attachment_change(Arc::new(move |attachment| {
            changes_clone.lock().unwrap().push(attachment.map(|a| a.session_id));
            Ok(())
        }))
        .unwrap();

    attach_client(&client, &server, "session-1").await;
    assert_eq!(
        client.attachment().unwrap(),
        SessionTarget {
            server_id: SERVER_ID.to_string(),
            session_id: "session-1".to_string(),
            attachment_id: "attachment-session-1".to_string(),
        }
    );
    match &server.messages()[1] {
        ClientMessage::Request(req) => {
            assert_eq!(req.target, server_target());
            assert_eq!(
                req.call,
                json!({
                    "serviceId": "pi.session-management",
                    "member": "attach",
                    "args": ["session-1"]
                })
            );
        }
        other => panic!("expected request, got {other:?}"),
    }

    server
        .send(ServerMessage::Attachment(AttachmentEnvelope { attachment: None }))
        .unwrap();
    assert!(client.attachment().is_none());
    assert_eq!(*changes.lock().unwrap(), vec![Some("session-1".to_string()), None]);
    client.dispose().await.unwrap();
}

#[tokio::test]
async fn buffers_service_updates_until_the_subscription_snapshot_arrives() {
    let server = MemoryByteServer::new(None);
    let client = connect_client(&server, SERVER_ID).await.unwrap();
    attach_client(&client, &server, "session-1").await;
    let target = client.attachment().unwrap();
    let client_for_target = client.clone();
    let transport =
        create_client_service_transport(&client, move || client_for_target.attachment().map(RpcTarget::Session));

    let updates = Arc::new(Mutex::new(Vec::<ServiceProviderUpdate>::new()));
    let updates_clone = Arc::clone(&updates);
    let listener: Arc<dyn Fn(ServiceProviderUpdate, pi_chord::Context) + Send + Sync> = Arc::new(move |update, _| {
        updates_clone.lock().unwrap().push(update);
    });

    let srv_clone = server.clone();
    let opening = tokio::spawn({
        let transport = transport.clone();
        let ctx = BACKGROUND_CONTEXT.clone();
        async move {
            transport
                .subscribe("pi.models", ServiceMode::Singleton, listener, &ctx)
                .await
        }
    });

    srv_clone.wait_for_messages(3).await;
    match &srv_clone.messages()[2] {
        ClientMessage::Request(req) => {
            assert_eq!(req.target, RpcTarget::Session(target));
            assert_eq!(
                req.call,
                json!({
                    "serviceId": "$chord.service",
                    "member": "subscribe",
                    "args": ["service-1", "pi.models", "singleton"]
                })
            );
        }
        other => panic!("expected subscribe request, got {other:?}"),
    }

    srv_clone
        .send(ServerMessage::ServiceUpdate(ServiceEventEnvelope {
            subscription_id: "service-1".to_string(),
            update: json!({
                "type": "state",
                "member": "state",
                "sequence": 1,
                "ops": [["s", ["revision"], 1]]
            }),
        }))
        .unwrap();
    tokio::task::yield_now().await;
    assert!(updates.lock().unwrap().is_empty());

    srv_clone
        .send(ServerMessage::Response(ResponseEnvelope {
            id: "request-2".to_string(),
            ok: true,
            result: Some(json!({
                "serviceId": "pi.models",
                "mode": "singleton",
                "instances": [{
                    "members": [{
                        "name": "state",
                        "kind": "state",
                        "sequence": 0,
                        "ops": [["r", { "revision": 0 }]]
                    }]
                }]
            })),
            error: None,
        }))
        .unwrap();

    let subscription = opening.await.unwrap().unwrap();
    assert!(updates.lock().unwrap().is_empty());

    srv_clone
        .send(ServerMessage::ServiceUpdate(ServiceEventEnvelope {
            subscription_id: "closed-subscription".to_string(),
            update: json!({
                "type": "state",
                "member": "state",
                "sequence": 99,
                "ops": [["s", 99, 99]]
            }),
        }))
        .unwrap();
    tokio::task::yield_now().await;
    assert!(client.connected());

    subscription.activate().unwrap();
    for _ in 0..50 {
        if updates.lock().unwrap().len() == 1 {
            break;
        }
        let _ = pi_js::time::sleep(5, None).await;
    }
    assert_eq!(updates.lock().unwrap().len(), 1);

    srv_clone
        .send(ServerMessage::ServiceUpdate(ServiceEventEnvelope {
            subscription_id: "service-1".to_string(),
            update: json!({
                "type": "state",
                "member": "state",
                "sequence": 2,
                "ops": [["s", ["revision"], 2]]
            }),
        }))
        .unwrap();
    srv_clone
        .send(ServerMessage::ServiceUpdate(ServiceEventEnvelope {
            subscription_id: "service-1".to_string(),
            update: json!({
                "type": "state",
                "member": "state",
                "sequence": 3,
                "ops": [["#", 0, ["revision"]], ["s", 0, 3]]
            }),
        }))
        .unwrap();

    for _ in 0..50 {
        if updates.lock().unwrap().len() == 3 {
            break;
        }
        let _ = pi_js::time::sleep(5, None).await;
    }
    {
        let u = updates.lock().unwrap();
        assert_eq!(u.len(), 3);
        match &u[2] {
            ServiceProviderUpdate::State { ops, .. } => {
                assert_eq!(serde_json::to_value(ops).unwrap(), json!([["s", ["revision"], 3]]));
            }
            other => panic!("expected State update, got {other:?}"),
        }
    }

    let disposing = tokio::spawn({
        let subscription = subscription.clone();
        async move { subscription.close(Some(BACKGROUND_CONTEXT.clone())).await }
    });
    srv_clone.wait_for_messages(4).await;
    match &srv_clone.messages()[3] {
        ClientMessage::Request(req) => {
            assert_eq!(
                req.call,
                json!({
                    "serviceId": "$chord.service",
                    "member": "unsubscribe",
                    "args": ["service-1"]
                })
            );
        }
        other => panic!("expected unsubscribe request, got {other:?}"),
    }
    srv_clone
        .send(ServerMessage::Response(ResponseEnvelope {
            id: "request-3".to_string(),
            ok: true,
            result: None,
            error: None,
        }))
        .unwrap();
    disposing.await.unwrap().unwrap();
    client.dispose().await.unwrap();
}

#[tokio::test]
async fn correlates_out_of_order_generic_service_responses() {
    let server = MemoryByteServer::new(None);
    let client = connect_client(&server, SERVER_ID).await.unwrap();
    let first = client.request(
        server_target(),
        ServiceCall {
            service_id: "test".to_string(),
            instance: None,
            member: "first".to_string(),
            args: vec![],
        },
        None,
    );
    let second = client.request(
        server_target(),
        ServiceCall {
            service_id: "test".to_string(),
            instance: None,
            member: "second".to_string(),
            args: vec![],
        },
        None,
    );
    server.wait_for_messages(3).await;
    server
        .send(ServerMessage::Response(ResponseEnvelope {
            id: "request-2".to_string(),
            ok: true,
            result: Some(json!("second")),
            error: None,
        }))
        .unwrap();
    server
        .send(ServerMessage::Response(ResponseEnvelope {
            id: "request-1".to_string(),
            ok: true,
            result: Some(json!("first")),
            error: None,
        }))
        .unwrap();
    let (r1, r2) = tokio::join!(first, second);
    assert_eq!(r1.unwrap(), Some(json!("first")));
    assert_eq!(r2.unwrap(), Some(json!("second")));
    client.dispose().await.unwrap();
}

#[tokio::test]
async fn exposes_bounded_server_errors() {
    let server = MemoryByteServer::new(None);
    let client = connect_client(&server, SERVER_ID).await.unwrap();
    let pending = client.request(
        server_target(),
        ServiceCall {
            service_id: "test".to_string(),
            instance: None,
            member: "missing".to_string(),
            args: vec![],
        },
        None,
    );
    server.wait_for_messages(2).await;
    server
        .send(ServerMessage::Response(ResponseEnvelope {
            id: "request-1".to_string(),
            ok: false,
            result: None,
            error: Some(ProtocolError {
                code: "session_not_found".to_string(),
                message: "Unknown session".to_string(),
            }),
        }))
        .unwrap();
    let err = pending.await.err().unwrap();
    let server_err = err.downcast_ref::<ServerError>().unwrap();
    assert_eq!(server_err.code, "session_not_found");
    assert_eq!(server_err.message, "Unknown session");
    client.dispose().await.unwrap();
}

#[tokio::test]
async fn does_not_send_a_pre_aborted_untyped_rpc_request() {
    let server = MemoryByteServer::new(None);
    let client = connect_client(&server, SERVER_ID).await.unwrap();
    let controller = AbortController::new();
    controller.abort(Some(AbortReason::error("Error", "already cancelled")));

    let err = client
        .request(
            server_target(),
            ServiceCall {
                service_id: "test".to_string(),
                instance: None,
                member: "noop".to_string(),
                args: vec![],
            },
            Some(controller.signal()),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(err.message(), "already cancelled");
    assert_eq!(server.messages().len(), 1);
    client.dispose().await.unwrap();
}

#[tokio::test]
async fn cancels_one_untyped_rpc_request_without_disconnecting() {
    let server = MemoryByteServer::new(None);
    let client = connect_client(&server, SERVER_ID).await.unwrap();
    let controller = AbortController::new();
    let pending = client.request(
        server_target(),
        ServiceCall {
            service_id: "test".to_string(),
            instance: None,
            member: "mutate".to_string(),
            args: vec![json!({ "value": 42 })],
        },
        Some(controller.signal()),
    );
    server.wait_for_messages(2).await;
    match &server.messages()[1] {
        ClientMessage::Request(req) => {
            assert_eq!(req.id, "request-1");
            assert_eq!(req.target, server_target());
        }
        other => panic!("expected Request, got {other:?}"),
    }

    controller.abort(Some(AbortReason::error("Error", "stop this request")));
    let err = pending.await.err().unwrap();
    assert_eq!(err.message(), "stop this request");
    server.wait_for_messages(3).await;
    match &server.messages()[2] {
        ClientMessage::Cancel(c) => {
            assert_eq!(c.id, "request-1");
            assert_eq!(c.target, server_target());
        }
        other => panic!("expected Cancel, got {other:?}"),
    }

    server
        .send(ServerMessage::Response(ResponseEnvelope {
            id: "request-1".to_string(),
            ok: false,
            result: None,
            error: Some(ProtocolError {
                code: "cancelled".to_string(),
                message: "cancelled".to_string(),
            }),
        }))
        .unwrap();
    assert!(client.connected());
    client.dispose().await.unwrap();
}

#[tokio::test]
async fn rejects_pending_requests_after_disconnect_or_disposal() {
    let server = MemoryByteServer::new(None);
    let client = connect_client(&server, SERVER_ID).await.unwrap();
    let pending = client.request(
        server_target(),
        ServiceCall {
            service_id: "test".to_string(),
            instance: None,
            member: "pending".to_string(),
            args: vec![],
        },
        None,
    );
    server.disconnect();
    let err = pending.await.err().unwrap();
    assert!(err.downcast_ref::<DisconnectedError>().is_some());

    client.dispose().await.unwrap();
    let err2 = client
        .request(
            server_target(),
            ServiceCall {
                service_id: "test".to_string(),
                instance: None,
                member: "disposed".to_string(),
                args: vec![],
            },
            None,
        )
        .await
        .err()
        .unwrap();
    assert!(err2.downcast_ref::<ClientDisposedError>().is_some());
}

#[tokio::test]
async fn rejects_server_data_delivered_before_the_client_hello_is_sent() {
    struct SyncEarlyDataTransport {
        send_count: Arc<AtomicUsize>,
        close_count: Arc<AtomicUsize>,
    }
    impl ByteTransport for SyncEarlyDataTransport {
        fn send(&self, _chunk: Vec<u8>) -> pi_js::BoxFuture<pi_js::Result<()>> {
            self.send_count.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Ok(()) })
        }
        fn close(&self) {
            self.close_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    let send_count = Arc::new(AtomicUsize::new(0));
    let close_count = Arc::new(AtomicUsize::new(0));
    let send_count_ref = Arc::clone(&send_count);
    let close_count_ref = Arc::clone(&close_count);

    let client = Client::new(ClientOptions {
        server_id: SERVER_ID.to_string(),
        transport_factory: Arc::new(move |handlers| {
            let send_count = Arc::clone(&send_count_ref);
            let close_count = Arc::clone(&close_count_ref);
            Box::pin(async move {
                let bytes = encode_server_message(
                    &ServerMessage::Hello(ServerHello {
                        version: PROTOCOL_VERSION,
                        server_id: SERVER_ID.to_string(),
                    }),
                    None,
                )?;
                handlers.on_data(&bytes);
                Ok(Arc::new(SyncEarlyDataTransport {
                    send_count,
                    close_count,
                }) as Arc<dyn ByteTransport>)
            })
        }),
        max_frame_length: None,
        on_listener_error: None,
    })
    .unwrap();

    let err = client.connect().await.err().unwrap();
    assert_eq!(err.name(), "ProtocolValidationError");
    assert_eq!(err.message(), "Received server data before the client hello was sent");
    assert_eq!(client.connection_state().as_str(), "disconnected");
    assert_eq!(send_count.load(Ordering::Acquire), 0);
    assert_eq!(close_count.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn rejects_typed_handshake_errors_and_closes_the_transport() {
    struct HelloErrorTransport {
        handlers: pi_client::ByteTransportHandlers,
        close_count: Arc<AtomicUsize>,
    }
    impl ByteTransport for HelloErrorTransport {
        fn send(&self, _chunk: Vec<u8>) -> pi_js::BoxFuture<pi_js::Result<()>> {
            let handlers = self.handlers.clone();
            Box::pin(async move {
                let bytes = encode_server_message(
                    &ServerMessage::HelloError(ServerHelloError {
                        error: ProtocolError {
                            code: "version".to_string(),
                            message: "Unsupported protocol version".to_string(),
                        },
                    }),
                    None,
                )?;
                handlers.on_data(&bytes);
                Ok(())
            })
        }
        fn close(&self) {
            self.close_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    let close_count = Arc::new(AtomicUsize::new(0));
    let close_count_ref = Arc::clone(&close_count);

    let client = Client::new(ClientOptions {
        server_id: SERVER_ID.to_string(),
        transport_factory: Arc::new(move |handlers| {
            let close_count = Arc::clone(&close_count_ref);
            Box::pin(
                async move { Ok(Arc::new(HelloErrorTransport { handlers, close_count }) as Arc<dyn ByteTransport>) },
            )
        }),
        max_frame_length: None,
        on_listener_error: None,
    })
    .unwrap();

    let err = client.connect().await.err().unwrap();
    assert_eq!(err.name(), "ServerError");
    let server_err = err.downcast_ref::<ServerError>().unwrap();
    assert_eq!(server_err.code, "version");
    assert_eq!(server_err.message, "Unsupported protocol version");
    assert_eq!(client.connection_state().as_str(), "disconnected");
    assert_eq!(close_count.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn rejects_pending_requests_and_reconnects_through_a_fresh_transport() {
    let first = MemoryByteServer::new(None);
    let second = MemoryByteServer::new(None);
    let connection_count = Arc::new(AtomicUsize::new(0));
    let first_clone = first.clone();
    let second_clone = second.clone();
    let count_clone = Arc::clone(&connection_count);

    let client = Client::new(ClientOptions {
        server_id: SERVER_ID.to_string(),
        transport_factory: Arc::new(move |handlers| {
            let idx = count_clone.fetch_add(1, Ordering::AcqRel);
            let target_srv = if idx == 0 {
                first_clone.clone()
            } else {
                second_clone.clone()
            };
            Box::pin(async move { Ok(target_srv.connect(handlers)) })
        }),
        max_frame_length: None,
        on_listener_error: None,
    })
    .unwrap();

    let states = Arc::new(Mutex::new(Vec::<String>::new()));
    let states_clone = Arc::clone(&states);
    let _unsub = client
        .on_connection_state_change(Arc::new(move |change| {
            states_clone.lock().unwrap().push(change.state.as_str().to_string());
            Ok(())
        }))
        .unwrap();

    client.connect().await.unwrap();
    attach_client(&client, &first, "session-1").await;
    let target = RpcTarget::Session(client.attachment().unwrap());
    let pending = client.request(
        target,
        ServiceCall {
            service_id: "test.session".to_string(),
            instance: None,
            member: "run".to_string(),
            args: vec![],
        },
        None,
    );
    first.wait_for_messages(3).await;
    first.disconnect();

    let err = pending.await.err().unwrap();
    assert!(err.downcast_ref::<DisconnectedError>().is_some());
    let hello = client.reconnect().await.unwrap();
    assert_eq!(hello.server_id, SERVER_ID);
    assert_eq!(connection_count.load(Ordering::Acquire), 2);
    assert!(client.connected());
    assert_eq!(second.messages().len(), 1);
    assert_eq!(
        *states.lock().unwrap(),
        vec!["connecting", "connected", "disconnected", "connecting", "connected"]
    );
    client.dispose().await.unwrap();
}

#[tokio::test]
async fn reports_transport_failures_without_leaving_requests_pending() {
    let server = MemoryByteServer::new(None);
    let client = connect_client(&server, SERVER_ID).await.unwrap();
    let pending = client.request(
        server_target(),
        ServiceCall {
            service_id: "test".to_string(),
            instance: None,
            member: "pending".to_string(),
            args: vec![],
        },
        None,
    );
    server.wait_for_messages(2).await;
    server.error(pi_js::Error::msg("read failed"));

    let err = pending.await.err().unwrap();
    assert_eq!(err.name(), "DisconnectedError");
    assert_eq!(err.message(), "read failed");
    let disc = err.downcast_ref::<DisconnectedError>().unwrap();
    assert_eq!(disc.cause().unwrap().message(), "read failed");
    assert_eq!(client.connection_state().as_str(), "disconnected");
}

#[tokio::test]
async fn disconnects_on_invalid_or_truncated_server_framing() {
    let invalid_server = MemoryByteServer::new(None);
    let invalid_client = connect_client(&invalid_server, SERVER_ID).await.unwrap();
    let mut map = IndexMap::new();
    map.insert(
        "type".to_string(),
        pi_protocol::CborValue::String("response".to_string()),
    );
    map.insert("id".to_string(), pi_protocol::CborValue::String("unknown".to_string()));
    map.insert("ok".to_string(), pi_protocol::CborValue::Bool(true));
    map.insert("result".to_string(), pi_protocol::CborValue::Number(1.0));
    let encoded = encode_frame(&encode_cbor(&pi_protocol::CborValue::Map(map), None).unwrap()).unwrap();
    invalid_server.send_raw(&encoded).unwrap();
    assert_eq!(invalid_client.connection_state().as_str(), "disconnected");

    let truncated_server = MemoryByteServer::new(None);
    let truncated_client = connect_client(&truncated_server, SERVER_ID).await.unwrap();
    let pending = truncated_client.request(
        server_target(),
        ServiceCall {
            service_id: "test".to_string(),
            instance: None,
            member: "pending".to_string(),
            args: vec![],
        },
        None,
    );
    truncated_server.wait_for_messages(2).await;
    truncated_server.send_raw(&[0, 0, 0, 2, 1]).unwrap();
    truncated_server.disconnect();

    let err = pending.await.err().unwrap();
    assert_eq!(err.name(), "ProtocolValidationError");
    assert!(err.message().to_lowercase().contains("truncated"));
    assert_eq!(truncated_client.connection_state().as_str(), "disconnected");
}

#[tokio::test]
async fn disconnects_when_a_response_has_no_matching_request() {
    let server = MemoryByteServer::new(None);
    let client = connect_client(&server, SERVER_ID).await.unwrap();
    server
        .send(ServerMessage::Response(ResponseEnvelope {
            id: "unknown-request".to_string(),
            ok: true,
            result: Some(json!([])),
            error: None,
        }))
        .unwrap();

    assert_eq!(client.connection_state().as_str(), "disconnected");
    assert_eq!(server.client_close_count(), 1);
}

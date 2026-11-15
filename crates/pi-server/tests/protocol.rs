//! Port of packages/server/test/protocol.test.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::ServiceCall;
use pi_js::{Error, Result};
use pi_protocol::cbor::{CborValue, encode_cbor};
use pi_protocol::protocol::ServerTarget;
use pi_protocol::{
    ClientHello, ClientMessage, PROTOCOL_VERSION, RequestEnvelope, RpcTarget, ServerHello, ServerHelloError,
    ServerMessage, encode_client_message, encode_frame,
};
use pi_server::connection::{ByteConnection, ByteConnectionHandler};
use pi_server::testing::{ProtocolTestClient, TestServerHost, WireChannel};
use pi_server::{Server, ServerOptions};
use serde_json::json;

const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";

struct MemoryWireState {
    closed: AtomicBool,
    client: Mutex<Option<ProtocolTestClient>>,
    handler: Mutex<Option<Arc<dyn ByteConnectionHandler>>>,
}

struct MemoryByteConnection {
    state: Arc<MemoryWireState>,
}

#[async_trait]
impl ByteConnection for MemoryByteConnection {
    fn closed(&self) -> bool {
        self.state.closed.load(Ordering::SeqCst)
    }

    async fn send(&self, chunk: &[u8]) -> Result<()> {
        let client = self.state.client.lock().expect("client mutex poisoned").clone();
        if let Some(client) = client {
            client.receive(chunk);
        }
        Ok(())
    }

    async fn close(&self, final_chunk: Option<&[u8]>) -> Result<()> {
        let client = self.state.client.lock().expect("client mutex poisoned").clone();
        if let Some(client) = &client {
            if let Some(fc) = final_chunk {
                client.receive(fc);
            }
            self.state.closed.store(true, Ordering::SeqCst);
            client.mark_closed();
        } else {
            self.state.closed.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

struct MemoryWireChannel {
    state: Arc<MemoryWireState>,
}

#[async_trait]
impl WireChannel for MemoryWireChannel {
    async fn send(&self, chunk: &[u8]) -> Result<()> {
        let handler = self
            .state
            .handler
            .lock()
            .expect("handler mutex poisoned")
            .clone()
            .expect("handler registered");
        handler.on_data(chunk);
        Ok(())
    }

    async fn send_fragmented(&self, chunk: &[u8], split_at: usize) -> Result<()> {
        let handler = self
            .state
            .handler
            .lock()
            .expect("handler mutex poisoned")
            .clone()
            .expect("handler registered");
        let split = split_at.min(chunk.len());
        handler.on_data(&chunk[..split]);
        handler.on_data(&chunk[split..]);
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        if self.state.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let handler = self.state.handler.lock().expect("handler mutex poisoned").clone();
        if let Some(handler) = handler {
            handler.on_close();
        }
        let client = self.state.client.lock().expect("client mutex poisoned").clone();
        if let Some(client) = client {
            client.mark_closed();
        }
        Ok(())
    }
}

fn connect_in_memory() -> (Server, ProtocolTestClient) {
    let server = Server::new(
        TestServerHost::new(),
        ServerOptions {
            listeners: Vec::new(),
            server_id: SERVER_ID.to_string(),
            ..Default::default()
        },
    )
    .expect("create server");
    let state = Arc::new(MemoryWireState {
        closed: AtomicBool::new(false),
        client: Mutex::new(None),
        handler: Mutex::new(None),
    });
    let channel = Arc::new(MemoryWireChannel { state: state.clone() });
    let client = ProtocolTestClient::new(channel);
    *state.client.lock().expect("client mutex poisoned") = Some(client.clone());
    let conn = Arc::new(MemoryByteConnection { state: state.clone() });
    let handler = server.accept(conn);
    *state.handler.lock().expect("handler mutex poisoned") = Some(handler);
    (server, client)
}

#[tokio::test]
async fn requires_hello_as_the_first_message() {
    let (server, client) = connect_in_memory();
    let call = serde_json::to_value(ServiceCall {
        service_id: "pi.session-directory".to_string(),
        instance: None,
        member: "list".to_string(),
        args: Vec::new(),
    })
    .expect("service call json");
    client
        .send_message(&ClientMessage::Request(RequestEnvelope {
            id: "request-1".to_string(),
            target: RpcTarget::Server(ServerTarget {
                server_id: SERVER_ID.to_string(),
            }),
            call,
        }))
        .await
        .expect("send request");

    let msg = client
        .next(|m| matches!(m, ServerMessage::HelloError(_)))
        .await
        .expect("hello_error");
    match msg {
        ServerMessage::HelloError(ServerHelloError { error }) => {
            assert_eq!(error.code, "invalid_request");
        }
        other => panic!("Expected hello_error, got {other:?}"),
    }
    client.wait_for_close().await;
    server.close().await.expect("close server");
}

#[tokio::test]
async fn rejects_unsupported_protocol_versions() {
    let (server, client) = connect_in_memory();
    let msg = client.hello(Some(PROTOCOL_VERSION + 1)).await.expect("hello error");
    match msg {
        ServerMessage::HelloError(ServerHelloError { error }) => {
            assert_eq!(error.code, "version");
        }
        other => panic!("Expected hello_error, got {other:?}"),
    }
    client.wait_for_close().await;
    server.close().await.expect("close server");
}

#[tokio::test]
async fn accepts_fragmented_hello_and_request_frames() {
    let (server, client) = connect_in_memory();
    let hello_msg = ClientMessage::Hello(ClientHello {
        version: PROTOCOL_VERSION,
    });
    let hello_bytes = encode_client_message(&hello_msg, None).expect("encode hello");
    let c1 = client.clone();
    let hello_fut = tokio::spawn(async move { c1.next(|m| matches!(m, ServerMessage::Hello(_))).await });
    client
        .send_fragmented_message(&hello_msg, hello_bytes.len() / 2)
        .await
        .expect("send fragmented hello");
    let hello_resp = hello_fut.await.expect("hello wait task").expect("hello response");
    match hello_resp {
        ServerMessage::Hello(ServerHello { server_id, .. }) => assert_eq!(server_id, SERVER_ID),
        other => panic!("Expected hello, got {other:?}"),
    }

    let c2 = client.clone();
    let resp_fut = tokio::spawn(async move { c2.next(|m| matches!(m, ServerMessage::Response(_))).await });
    let call = json!({
        "serviceId": "pi.session-directory",
        "member": "list",
        "args": []
    });
    let request = ClientMessage::Request(RequestEnvelope {
        id: "request-1".to_string(),
        target: RpcTarget::Server(ServerTarget {
            server_id: SERVER_ID.to_string(),
        }),
        call,
    });
    let frame = encode_client_message(&request, None).expect("encode request");
    client
        .send_fragmented_message(&request, frame.len() / 2)
        .await
        .expect("send fragmented request");
    let resp = resp_fut.await.expect("resp wait task").expect("response");
    match resp {
        ServerMessage::Response(env) => {
            assert!(!env.ok);
            assert_eq!(env.error.expect("error").code, "internal_error");
        }
        other => panic!("Expected response, got {other:?}"),
    }
    server.close().await.expect("close server");
}

#[tokio::test]
async fn rejects_hostile_framed_input() {
    let mut invalid_map = IndexMap::new();
    invalid_map.insert("type".to_string(), CborValue::String("hello".to_string()));
    invalid_map.insert("version".to_string(), CborValue::Number(1.0));
    invalid_map.insert("extra".to_string(), CborValue::Bool(true));

    let cases = vec![
        ("malformed CBOR", encode_frame(&[0xff]).expect("frame")),
        (
            "schema-invalid CBOR",
            encode_frame(&encode_cbor(&CborValue::Map(invalid_map), None).expect("cbor")).expect("frame"),
        ),
        ("oversized frame", vec![1, 0, 0, 1]),
    ];

    for (label, bytes) in cases {
        let (server, client) = connect_in_memory();
        client.send_bytes(&bytes).await.expect(label);
        let msg = client
            .next(|m| matches!(m, ServerMessage::HelloError(_)))
            .await
            .expect(label);
        match msg {
            ServerMessage::HelloError(ServerHelloError { error }) => {
                assert_eq!(error.code, "invalid_request", "case: {label}");
            }
            other => panic!("Expected hello_error for {label}, got {other:?}"),
        }
        client.wait_for_close().await;
        server.close().await.expect("close server");
    }
}

#[tokio::test]
async fn rejects_a_second_hello_after_completing_the_handshake() {
    let (server, client) = connect_in_memory();
    client.hello(None).await.expect("initial hello");
    client
        .send_message(&ClientMessage::Hello(ClientHello {
            version: PROTOCOL_VERSION,
        }))
        .await
        .expect("second hello");
    let msg = client
        .next(|m| matches!(m, ServerMessage::HelloError(_)))
        .await
        .expect("hello_error");
    match msg {
        ServerMessage::HelloError(ServerHelloError { error }) => {
            assert_eq!(error.code, "invalid_request");
            assert!(error.message.contains("first message"));
        }
        other => panic!("Expected hello_error, got {other:?}"),
    }
    client.wait_for_close().await;
    server.close().await.expect("close server");
}

#[tokio::test]
async fn processes_a_hello_and_request_coalesced_in_one_byte_chunk() {
    let (server, client) = connect_in_memory();
    let hello = encode_client_message(
        &ClientMessage::Hello(ClientHello {
            version: PROTOCOL_VERSION,
        }),
        None,
    )
    .expect("encode hello");
    let request = encode_client_message(
        &ClientMessage::Request(RequestEnvelope {
            id: "request-1".to_string(),
            target: RpcTarget::Server(ServerTarget {
                server_id: SERVER_ID.to_string(),
            }),
            call: json!({
                "serviceId": "pi.session-directory",
                "member": "list",
                "args": []
            }),
        }),
        None,
    )
    .expect("encode request");
    let mut wire = Vec::with_capacity(hello.len() + request.len());
    wire.extend_from_slice(&hello);
    wire.extend_from_slice(&request);

    client.send_bytes(&wire).await.expect("send coalesced");
    let hello_msg = client
        .next(|m| matches!(m, ServerMessage::Hello(_)))
        .await
        .expect("hello");
    assert!(matches!(hello_msg, ServerMessage::Hello(_)));

    let resp_msg = client
        .next(|m| matches!(m, ServerMessage::Response(_)))
        .await
        .expect("response");
    match resp_msg {
        ServerMessage::Response(env) => {
            assert_eq!(env.id, "request-1");
            assert!(!env.ok);
            assert_eq!(env.error.expect("error").code, "internal_error");
        }
        other => panic!("Expected response, got {other:?}"),
    }
    server.close().await.expect("close server");
}

#[tokio::test]
async fn reports_a_truncated_final_frame_when_the_peer_closes() {
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let errors_cb = errors.clone();
    let server = Server::new(
        TestServerHost::new(),
        ServerOptions {
            listeners: Vec::new(),
            server_id: SERVER_ID.to_string(),
            on_error: Some(Arc::new(move |err: &Error| {
                errors_cb.lock().expect("errors mutex poisoned").push(err.message());
            })),
            ..Default::default()
        },
    )
    .expect("create server");

    struct PassiveConn(Arc<AtomicBool>);

    #[async_trait]
    impl ByteConnection for PassiveConn {
        fn closed(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }

        async fn send(&self, _chunk: &[u8]) -> Result<()> {
            Ok(())
        }

        async fn close(&self, _final_chunk: Option<&[u8]>) -> Result<()> {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    let closed_flag = Arc::new(AtomicBool::new(false));
    let conn = Arc::new(PassiveConn(closed_flag.clone()));
    let handler = server.accept(conn);
    handler.on_data(&[0, 0, 0, 2, 1]);
    handler.on_close();

    assert!(!closed_flag.load(Ordering::SeqCst));
    let recorded = errors.lock().expect("errors mutex poisoned").clone();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].to_ascii_lowercase().contains("truncated"));
    server.close().await.expect("close server");
}

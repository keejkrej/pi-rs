//! Port of packages/server/test/conformance.test.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::{Context, JsonValue, ServiceCall, ServiceUpdatePublisher};
use pi_js::{BoxFuture, Error, Result};
use pi_protocol::protocol::ServerTarget;
use pi_protocol::{
    AttachmentEnvelope, PROTOCOL_VERSION, RequestEnvelope, ResponseEnvelope, RpcTarget, ServerHello, ServerMessage,
    SessionTarget,
};
use pi_server::connection::{ByteConnection, ByteConnectionHandler};
use pi_server::testing::{
    Deferred, ProtocolTestClient, TestServerHost, WireChannel, create_test_server_services, poll_until,
};
use pi_server::{
    DefaultSessionMetadata, RoutedServerPresentation, RoutedServerServiceAttachment, RoutedServerServiceHost,
    RoutedSessionAttachment, RoutedSessionHandle, Server, ServerHost, ServerOptions, SessionAmbiguousError,
    SessionMetadata,
};
use serde_json::json;

const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";
const OTHER_SERVER_ID: &str = "00000000-0000-4000-8000-000000000002";

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

fn create_server<TMetadata: SessionMetadata>(host: Arc<dyn ServerHost<TMetadata>>) -> Server<TMetadata> {
    Server::new(
        host,
        ServerOptions {
            listeners: Vec::new(),
            server_id: SERVER_ID.to_string(),
            ..Default::default()
        },
    )
    .expect("create server")
}

fn connect<TMetadata: SessionMetadata>(server: &Server<TMetadata>) -> ProtocolTestClient {
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
    client
}

fn session_call(member: &str, args: Vec<JsonValue>) -> ServiceCall {
    ServiceCall {
        service_id: "test.session".to_string(),
        instance: None,
        member: member.to_string(),
        args,
    }
}

fn latest_attachment_id(client: &ProtocolTestClient, session_id: &str) -> String {
    for message in client.messages().into_iter().rev() {
        if let ServerMessage::Attachment(AttachmentEnvelope { attachment: Some(att) }) = message
            && att.session_id == session_id
        {
            return att.attachment_id;
        }
    }
    panic!("Missing attachment for {session_id}");
}

#[tokio::test]
async fn handshake_identifies_the_logical_server_without_listing_sessions() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    let server = create_server(host.clone());
    let client = connect(&server);

    let hello = client.hello(None).await.expect("hello");
    match hello {
        ServerMessage::Hello(ServerHello { server_id, version }) => {
            assert_eq!(server_id, SERVER_ID);
            assert_eq!(version, PROTOCOL_VERSION);
        }
        other => panic!("Expected hello, got {other:?}"),
    }
    assert_eq!(host.harnesses_count(), 0);
    server.close().await.expect("close server");
}

#[tokio::test]
async fn rejects_a_semantically_invalid_service_call_after_envelope_decoding() {
    let host = TestServerHost::new();
    let server = create_server(host);
    let client = connect(&server);
    client.hello(None).await.expect("hello");

    let wait_client = client.clone();
    let wait_task = tokio::spawn(async move {
        wait_client
            .next(|m| matches!(m, ServerMessage::Response(env) if env.id == "invalid-call"))
            .await
    });
    client
        .send_message(&pi_protocol::ClientMessage::Request(RequestEnvelope {
            id: "invalid-call".to_string(),
            target: RpcTarget::Server(ServerTarget {
                server_id: SERVER_ID.to_string(),
            }),
            call: json!({ "arbitrary": true }),
        }))
        .await
        .expect("send invalid call");

    let msg = wait_task.await.expect("wait task").expect("response");
    match msg {
        ServerMessage::Response(env) => {
            assert!(!env.ok);
            assert_eq!(env.error.expect("error").code, "invalid_request");
        }
        other => panic!("Expected response, got {other:?}"),
    }
    server.close().await.expect("close server");
}

#[tokio::test]
async fn attach_passes_concrete_repository_metadata_to_the_harness_host() {
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct BackendMetadata {
        id: String,
        path: String,
        modified_at: u64,
    }

    impl SessionMetadata for BackendMetadata {
        fn id(&self) -> &str {
            &self.id
        }
    }

    struct CustomAttachment;
    #[async_trait]
    impl RoutedSessionAttachment for CustomAttachment {
        async fn invoke_service(
            &self,
            _call: &ServiceCall,
            _publish: ServiceUpdatePublisher,
            _context: Context,
        ) -> Result<Option<JsonValue>> {
            Ok(None)
        }

        async fn release(&self, _context: Context) -> Result<()> {
            Ok(())
        }
    }

    struct CustomHandle;
    #[async_trait]
    impl RoutedSessionHandle for CustomHandle {
        async fn attach_client(&self, _context: Context) -> Result<Arc<dyn RoutedSessionAttachment>> {
            Ok(Arc::new(CustomAttachment))
        }

        async fn close(&self, _context: Context) -> Result<()> {
            Ok(())
        }
    }

    struct CustomHost {
        metadata: BackendMetadata,
        received: Mutex<Option<BackendMetadata>>,
    }

    #[async_trait]
    impl ServerHost<BackendMetadata> for CustomHost {
        fn server_services(&self) -> Arc<dyn RoutedServerServiceHost> {
            create_test_server_services()
        }

        async fn resolve_session(&self, _session_id: &str, _context: Context) -> Result<BackendMetadata> {
            Ok(self.metadata.clone())
        }

        async fn open_session(
            &self,
            candidate: BackendMetadata,
            _context: Context,
        ) -> Result<Arc<dyn RoutedSessionHandle>> {
            *self.received.lock().expect("received mutex poisoned") = Some(candidate);
            Ok(Arc::new(CustomHandle))
        }
    }

    let expected = BackendMetadata {
        id: "session-1".to_string(),
        path: "/sessions/session-1.jsonl".to_string(),
        modified_at: 2,
    };
    let host = Arc::new(CustomHost {
        metadata: expected.clone(),
        received: Mutex::new(None),
    });
    let server = create_server(host.clone());
    let client = connect(&server);
    client.hello(None).await.expect("hello");

    let resp = client.attach(SERVER_ID, "session-1").await.expect("attach");
    assert!(resp.ok);
    assert_eq!(
        host.received.lock().expect("received mutex poisoned").clone(),
        Some(expected)
    );
    server.close().await.expect("close server");
}

#[tokio::test]
async fn routes_opaque_server_services_and_publishes_attachment_changes_out_of_band() {
    let backing = TestServerHost::new();
    backing.seed("session-1").await;
    let release_count = Arc::new(AtomicUsize::new(0));

    struct WrapperHost {
        backing: Arc<TestServerHost>,
        release_count: Arc<AtomicUsize>,
    }

    struct WrapperServicesHost {
        release_count: Arc<AtomicUsize>,
    }

    struct WrapperServicesAttachment {
        presentation: Arc<dyn RoutedServerPresentation>,
        release_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl RoutedServerServiceHost for WrapperServicesHost {
        async fn attach_client(
            &self,
            presentation: Arc<dyn RoutedServerPresentation>,
            _context: Context,
        ) -> Result<Arc<dyn RoutedServerServiceAttachment>> {
            Ok(Arc::new(WrapperServicesAttachment {
                presentation,
                release_count: self.release_count.clone(),
            }))
        }
    }

    #[async_trait]
    impl RoutedServerServiceAttachment for WrapperServicesAttachment {
        async fn invoke_service(
            &self,
            call: &ServiceCall,
            _publish: ServiceUpdatePublisher,
            context: Context,
        ) -> Result<Option<JsonValue>> {
            if call.service_id != "pi.session-management" {
                return Err(Error::msg("Unexpected service"));
            }
            if call.member == "attach"
                && let Some(session_id) = call.args.first().and_then(|v| v.as_str())
            {
                self.presentation.attach_session(session_id, context).await?;
                return Ok(Some(JsonValue::Null));
            }
            if call.member == "detach" {
                self.presentation.detach_session(context).await?;
                return Ok(Some(JsonValue::Null));
            }
            Err(Error::msg("Unexpected service member"))
        }

        async fn release(&self, _context: Context) -> Result<()> {
            self.release_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait]
    impl ServerHost<DefaultSessionMetadata> for WrapperHost {
        fn server_services(&self) -> Arc<dyn RoutedServerServiceHost> {
            Arc::new(WrapperServicesHost {
                release_count: self.release_count.clone(),
            })
        }

        async fn resolve_session(&self, session_id: &str, context: Context) -> Result<DefaultSessionMetadata> {
            self.backing.resolve_session(session_id, context).await
        }

        async fn open_session(
            &self,
            metadata: DefaultSessionMetadata,
            context: Context,
        ) -> Result<Arc<dyn RoutedSessionHandle>> {
            self.backing.open_session(metadata, context).await
        }
    }

    let host = Arc::new(WrapperHost {
        backing: backing.clone(),
        release_count: release_count.clone(),
    });
    let server = create_server(host);
    let client = connect(&server);
    client.hello(None).await.expect("hello");

    let c1 = client.clone();
    let attached_wait = tokio::spawn(async move {
        c1.next(|m| matches!(m, ServerMessage::Attachment(AttachmentEnvelope { attachment: Some(_) })))
            .await
    });
    let attach_resp = client.attach(SERVER_ID, "session-1").await.expect("attach");
    assert!(attach_resp.ok);
    match attached_wait.await.expect("wait task").expect("attached") {
        ServerMessage::Attachment(AttachmentEnvelope { attachment: Some(att) }) => {
            assert_eq!(att.session_id, "session-1");
            assert!(!att.attachment_id.is_empty());
        }
        other => panic!("Expected attachment, got {other:?}"),
    }
    assert_eq!(
        backing.latest_harness("session-1").expect("harness").attached_clients(),
        1
    );

    let msg_count = client.messages().len();
    let c2 = client.clone();
    let detached_wait = tokio::spawn(async move {
        c2.next_from(msg_count, |m| {
            matches!(m, ServerMessage::Attachment(AttachmentEnvelope { attachment: None }))
        })
        .await
    });
    let detach_resp = client
        .request_service(
            RpcTarget::Server(ServerTarget {
                server_id: SERVER_ID.to_string(),
            }),
            ServiceCall {
                service_id: "pi.session-management".to_string(),
                instance: None,
                member: "detach".to_string(),
                args: Vec::new(),
            },
            None,
        )
        .await
        .expect("detach");
    assert!(detach_resp.ok);
    assert!(matches!(
        detached_wait.await.expect("wait task").expect("detached"),
        ServerMessage::Attachment(AttachmentEnvelope { attachment: None })
    ));
    assert_eq!(
        backing.latest_harness("session-1").expect("harness").attached_clients(),
        0
    );

    client.close().await.expect("close client");
    poll_until(|| release_count.load(Ordering::SeqCst) == 1)
        .await
        .expect("serverServices released");
    server.close().await.expect("close server");
}

#[tokio::test]
async fn permits_multiple_client_attachments_per_session() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    let server = create_server(host.clone());
    let first = connect(&server);
    let second = connect(&server);
    tokio::try_join!(first.hello(None), second.hello(None)).expect("hellos");

    assert!(first.attach(SERVER_ID, "session-1").await.expect("attach 1").ok);
    assert!(first.attach(SERVER_ID, "session-1").await.expect("re-attach 1").ok);
    assert_eq!(host.latest_harness("session-1").expect("harness").attached_clients(), 1);
    assert!(second.attach(SERVER_ID, "session-1").await.expect("attach 2").ok);
    assert_eq!(host.harnesses_for("session-1").len(), 1);
    assert_eq!(host.latest_harness("session-1").expect("harness").attached_clients(), 2);

    first.close().await.expect("close first");
    poll_until(|| host.latest_harness("session-1").expect("harness").attached_clients() == 1)
        .await
        .expect("first attachment released");
    assert!(second.attach(SERVER_ID, "session-1").await.expect("second still ok").ok);
    server.close().await.expect("close server");
}

#[tokio::test]
async fn clears_connection_ownership_when_attachment_release_fails() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let errors_cb = errors.clone();
    let server = Server::new(
        host.clone(),
        ServerOptions {
            listeners: Vec::new(),
            server_id: SERVER_ID.to_string(),
            on_error: Some(Arc::new(move |err: &Error| {
                errors_cb.lock().expect("errors mutex").push(err.message());
            })),
            ..Default::default()
        },
    )
    .expect("create server");

    let first = connect(&server);
    let second = connect(&server);
    tokio::try_join!(first.hello(None), second.hello(None)).expect("hellos");
    assert!(first.attach(SERVER_ID, "session-1").await.expect("attach 1").ok);
    let harness = host.latest_harness("session-1").expect("harness");
    harness.set_fail_attachment_release(Some(Error::msg("release failed")));

    first.close().await.expect("close first");
    poll_until(|| harness.attachment_release_count() == 1)
        .await
        .expect("release attempted");
    poll_until(|| {
        errors
            .lock()
            .expect("errors mutex")
            .iter()
            .any(|msg| msg == "release failed")
    })
    .await
    .expect("release error reported");

    harness.set_fail_attachment_release(None);
    assert!(second.attach(SERVER_ID, "session-1").await.expect("attach 2").ok);
    server.close().await.expect("close server");
}

#[tokio::test]
async fn requires_the_requesting_client_to_hold_the_targeted_session_attachment() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    host.seed("session-2").await;
    let server = create_server(host.clone());
    let attached = connect(&server);
    let unattached = connect(&server);
    tokio::try_join!(attached.hello(None), unattached.hello(None)).expect("hellos");

    let unattached_resp = unattached
        .request_session_service(SERVER_ID, "session-1", session_call("run", Vec::new()), None)
        .await
        .expect("response");
    assert!(!unattached_resp.ok);
    assert_eq!(unattached_resp.error.expect("error").code, "session_not_attached");

    assert!(attached.attach(SERVER_ID, "session-1").await.expect("attach").ok);
    let wrong_session_resp = attached
        .request_session_service(SERVER_ID, "session-2", session_call("run", Vec::new()), None)
        .await
        .expect("response");
    assert!(!wrong_session_resp.ok);
    assert_eq!(wrong_session_resp.error.expect("error").code, "session_not_attached");

    let ok_resp = attached
        .request_session_service(SERVER_ID, "session-1", session_call("run", vec![json!("Hello")]), None)
        .await
        .expect("response");
    assert!(ok_resp.ok);
    assert_eq!(ok_resp.result, Some(json!({ "ok": true })));
    assert_eq!(
        host.latest_harness("session-1").expect("harness").service_calls(),
        vec![session_call("run", vec![json!("Hello")])]
    );
    server.close().await.expect("close server");
}

#[tokio::test]
async fn rejects_a_stale_attachment_route_after_switching_sessions() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    host.seed("session-2").await;
    let server = create_server(host.clone());
    let client = connect(&server);
    client.hello(None).await.expect("hello");
    client.attach(SERVER_ID, "session-1").await.expect("attach 1");
    let first_attachment_id = latest_attachment_id(&client, "session-1");
    client.attach(SERVER_ID, "session-2").await.expect("attach 2");

    let resp = client
        .request_service(
            RpcTarget::Session(SessionTarget {
                server_id: SERVER_ID.to_string(),
                session_id: "session-1".to_string(),
                attachment_id: first_attachment_id,
            }),
            session_call("run", vec![json!("stale")]),
            None,
        )
        .await
        .expect("response");
    assert!(!resp.ok);
    assert_eq!(resp.error.expect("error").code, "session_not_attached");
    assert!(
        host.latest_harness("session-1")
            .expect("harness")
            .service_calls()
            .is_empty()
    );
    server.close().await.expect("close server");
}

#[tokio::test]
async fn preserves_opaque_service_results_and_bounds_adapter_defects() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    let server = create_server(host.clone());
    let client = connect(&server);
    client.hello(None).await.expect("hello");
    client.attach(SERVER_ID, "session-1").await.expect("attach");
    let harness = host.latest_harness("session-1").expect("harness");

    harness.set_next_service_result(Some(json!({ "accepted": false, "reason": "closed" })));
    let resp = client
        .request_session_service(SERVER_ID, "session-1", session_call("run", Vec::new()), None)
        .await
        .expect("response");
    assert!(resp.ok);
    assert_eq!(resp.result, Some(json!({ "accepted": false, "reason": "closed" })));

    harness.set_next_service_error(Some(Error::msg("private adapter detail")));
    let err_resp = client
        .request_session_service(SERVER_ID, "session-1", session_call("run", Vec::new()), None)
        .await
        .expect("response");
    assert!(!err_resp.ok);
    let err = err_resp.error.expect("error");
    assert_eq!(err.code, "internal_error");
    assert_eq!(err.message, "Internal server error");
    server.close().await.expect("close server");
}

#[tokio::test]
async fn admits_concurrent_service_calls_to_the_attached_session() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    let server = create_server(host.clone());
    let client = connect(&server);
    client.hello(None).await.expect("hello");
    client.attach(SERVER_ID, "session-1").await.expect("attach");
    let harness = host.latest_harness("session-1").expect("harness");
    let gate = harness.gate_next_service_call();

    let c1 = client.clone();
    let first = tokio::spawn(async move {
        c1.request_session_service(SERVER_ID, "session-1", session_call("run", vec![json!("first")]), None)
            .await
    });
    gate.entered.wait().await;

    let second = client
        .request_session_service(SERVER_ID, "session-1", session_call("run", vec![json!("second")]), None)
        .await
        .expect("second response");
    assert!(second.ok);
    assert_eq!(
        harness.service_calls(),
        vec![
            session_call("run", vec![json!("first")]),
            session_call("run", vec![json!("second")]),
        ]
    );
    gate.release.resolve(());
    let first_resp = first.await.expect("first task").expect("first response");
    assert!(first_resp.ok);
    server.close().await.expect("close server");
}

#[tokio::test]
async fn keeps_attachment_demand_until_an_accepted_service_call_settles_after_disconnect() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    let server = create_server(host.clone());
    let client = connect(&server);
    client.hello(None).await.expect("hello");
    client.attach(SERVER_ID, "session-1").await.expect("attach");
    let harness = host.latest_harness("session-1").expect("harness");
    let gate = harness.gate_next_service_call();

    let c1 = client.clone();
    let calling = tokio::spawn(async move {
        c1.request_session_service(SERVER_ID, "session-1", session_call("run", Vec::new()), None)
            .await
    });
    gate.entered.wait().await;

    client.close().await.expect("close client");
    assert_eq!(harness.attached_clients(), 1);
    gate.release.resolve(());

    let call_err = calling
        .await
        .expect("calling task")
        .expect_err("disconnected call must fail");
    assert!(call_err.message().to_ascii_lowercase().contains("closed"));
    poll_until(|| harness.attached_clients() == 0)
        .await
        .expect("attachment released after in-flight call");
    server.close().await.expect("close server");
}

#[tokio::test]
async fn rejects_requests_addressed_to_another_server_before_repository_access() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    let server = create_server(host.clone());
    let client = connect(&server);
    client.hello(None).await.expect("hello");

    let resp = client.attach(OTHER_SERVER_ID, "session-1").await.expect("response");
    assert!(!resp.ok);
    assert_eq!(resp.error.expect("error").code, "wrong_server");
    assert_eq!(host.harnesses_count(), 0);
    server.close().await.expect("close server");
}

#[tokio::test]
async fn reports_an_unknown_session_without_creating_a_harness() {
    let host = TestServerHost::new();
    let server = create_server(host.clone());
    let client = connect(&server);
    client.hello(None).await.expect("hello");

    let resp = client.attach(SERVER_ID, "missing").await.expect("response");
    assert!(!resp.ok);
    assert_eq!(resp.error.expect("error").code, "session_not_found");
    assert_eq!(host.harnesses_count(), 0);
    server.close().await.expect("close server");
}

#[tokio::test]
async fn rejects_an_ambiguous_session_id_without_creating_a_harness() {
    #[derive(Clone)]
    struct BackendMetadata {
        id: String,
    }
    impl SessionMetadata for BackendMetadata {
        fn id(&self) -> &str {
            &self.id
        }
    }

    struct AmbiguousHost;
    #[async_trait]
    impl ServerHost<BackendMetadata> for AmbiguousHost {
        fn server_services(&self) -> Arc<dyn RoutedServerServiceHost> {
            create_test_server_services()
        }

        async fn resolve_session(&self, _session_id: &str, _context: Context) -> Result<BackendMetadata> {
            Err(SessionAmbiguousError::new().into())
        }

        async fn open_session(
            &self,
            _metadata: BackendMetadata,
            _context: Context,
        ) -> Result<Arc<dyn RoutedSessionHandle>> {
            Err(Error::msg("must not create a Harness for an ambiguous session"))
        }
    }

    let server = create_server(Arc::new(AmbiguousHost));
    let client = connect(&server);
    client.hello(None).await.expect("hello");

    let resp = client.attach(SERVER_ID, "duplicate").await.expect("response");
    assert!(!resp.ok);
    assert_eq!(resp.error.expect("error").code, "session_ambiguous");
    server.close().await.expect("close server");
}

#[tokio::test]
async fn invalidates_a_terminated_harness_handle_and_allows_a_later_attach() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    let server = create_server(host.clone());
    let client = connect(&server);
    client.hello(None).await.expect("hello");
    assert!(client.attach(SERVER_ID, "session-1").await.expect("attach").ok);
    let first_harness = host.latest_harness("session-1").expect("first harness");

    first_harness.terminate(Error::msg("worker crashed"));
    poll_until(|| first_harness.attached_clients() == 0)
        .await
        .expect("terminated harness invalidated");
    assert_eq!(first_harness.attachment_release_count(), 1);

    assert!(client.attach(SERVER_ID, "session-1").await.expect("re-attach").ok);
    assert_eq!(host.harnesses_for("session-1").len(), 2);
    server.close().await.expect("close server");
}

#[tokio::test]
async fn connection_loss_releases_its_attachment_while_server_shutdown_closes_the_harness() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    let server = create_server(host.clone());
    let client = connect(&server);
    client.hello(None).await.expect("hello");
    assert!(client.attach(SERVER_ID, "session-1").await.expect("attach").ok);
    let harness = host.latest_harness("session-1").expect("harness");

    client.close().await.expect("close client");
    poll_until(|| harness.attached_clients() == 0)
        .await
        .expect("attachment released");
    assert_eq!(harness.close_count(), 0);
    server.close().await.expect("close server");
    assert_eq!(harness.close_count(), 1);
}

#[tokio::test]
async fn releases_a_lease_acquired_concurrently_with_harness_termination() {
    let metadata = DefaultSessionMetadata::new("session-1");
    let acquiring = Deferred::<()>::new();
    let continue_acquiring = Deferred::<()>::new();
    let terminated = Deferred::<Option<Error>>::new();
    let release_count = Arc::new(AtomicUsize::new(0));

    struct RaceAttachment {
        release_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl RoutedSessionAttachment for RaceAttachment {
        async fn invoke_service(
            &self,
            _call: &ServiceCall,
            _publish: ServiceUpdatePublisher,
            _context: Context,
        ) -> Result<Option<JsonValue>> {
            Ok(None)
        }

        async fn release(&self, _context: Context) -> Result<()> {
            self.release_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct RaceHandle {
        acquiring: Deferred<()>,
        continue_acquiring: Deferred<()>,
        terminated: Deferred<Option<Error>>,
        terminated_taken: AtomicBool,
        release_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl RoutedSessionHandle for RaceHandle {
        async fn attach_client(&self, _context: Context) -> Result<Arc<dyn RoutedSessionAttachment>> {
            self.acquiring.resolve(());
            self.continue_acquiring.wait().await;
            Ok(Arc::new(RaceAttachment {
                release_count: self.release_count.clone(),
            }))
        }

        fn terminated(&self) -> Option<BoxFuture<Option<Error>>> {
            if self.terminated_taken.swap(true, Ordering::SeqCst) {
                return None;
            }
            let terminated = self.terminated.clone();
            Some(Box::pin(async move { terminated.take_wait().await }))
        }

        async fn close(&self, _context: Context) -> Result<()> {
            Ok(())
        }
    }

    struct RaceHost {
        metadata: DefaultSessionMetadata,
        acquiring: Deferred<()>,
        continue_acquiring: Deferred<()>,
        terminated: Deferred<Option<Error>>,
        release_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ServerHost<DefaultSessionMetadata> for RaceHost {
        fn server_services(&self) -> Arc<dyn RoutedServerServiceHost> {
            create_test_server_services()
        }

        async fn resolve_session(&self, _session_id: &str, _context: Context) -> Result<DefaultSessionMetadata> {
            Ok(self.metadata.clone())
        }

        async fn open_session(
            &self,
            _metadata: DefaultSessionMetadata,
            _context: Context,
        ) -> Result<Arc<dyn RoutedSessionHandle>> {
            Ok(Arc::new(RaceHandle {
                acquiring: self.acquiring.clone(),
                continue_acquiring: self.continue_acquiring.clone(),
                terminated: self.terminated.clone(),
                terminated_taken: AtomicBool::new(false),
                release_count: self.release_count.clone(),
            }))
        }
    }

    let host = Arc::new(RaceHost {
        metadata,
        acquiring: acquiring.clone(),
        continue_acquiring: continue_acquiring.clone(),
        terminated: terminated.clone(),
        release_count: release_count.clone(),
    });
    let server = create_server(host);
    let client = connect(&server);
    client.hello(None).await.expect("hello");

    let c1 = client.clone();
    let attach = tokio::spawn(async move { c1.attach(SERVER_ID, "session-1").await });
    acquiring.wait().await;

    terminated.resolve(Some(Error::msg("worker crashed")));
    tokio::task::yield_now().await;
    continue_acquiring.resolve(());

    let resp: ResponseEnvelope = attach.await.expect("attach task").expect("response");
    assert!(!resp.ok);
    assert_eq!(resp.error.expect("error").code, "server_draining");
    assert_eq!(release_count.load(Ordering::SeqCst), 1);
    server.close().await.expect("close server");
}

#[tokio::test]
async fn shares_a_harness_creation_failure_releases_the_session_and_allows_a_later_retry() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    host.set_next_open_session_error(Some(Error::msg("Harness creation failed")));
    let server = create_server(host.clone());
    let first = connect(&server);
    let second = connect(&server);
    tokio::try_join!(first.hello(None), second.hello(None)).expect("hellos");
    let gate = host.gate_next_open_session();

    let f1 = first.clone();
    let first_attach = tokio::spawn(async move { f1.attach(SERVER_ID, "session-1").await });
    gate.entered.wait().await;
    let f2 = second.clone();
    let second_attach = tokio::spawn(async move { f2.attach(SERVER_ID, "session-1").await });
    tokio::task::yield_now().await;
    gate.release.resolve(());

    let (r1, r2) = tokio::join!(first_attach, second_attach);
    let r1 = r1.expect("task 1").expect("resp 1");
    let r2 = r2.expect("task 2").expect("resp 2");
    assert!(!r1.ok);
    assert_eq!(r1.error.expect("err 1").code, "internal_error");
    assert!(!r2.ok);
    assert_eq!(r2.error.expect("err 2").code, "internal_error");
    assert_eq!(host.open_session_count(), 1);

    let retry = first.attach(SERVER_ID, "session-1").await.expect("retry");
    assert!(retry.ok);
    assert_eq!(host.open_session_count(), 2);
    assert_eq!(host.harnesses_for("session-1").len(), 1);
    server.close().await.expect("close server");
}

#[tokio::test]
async fn closes_a_harness_acquired_while_server_shutdown_is_in_progress() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    let server = create_server(host.clone());
    let client = connect(&server);
    client.hello(None).await.expect("hello");
    let gate = host.gate_next_open_session();

    let c1 = client.clone();
    let attach = tokio::spawn(async move { c1.attach(SERVER_ID, "session-1").await });
    gate.entered.wait().await;
    let srv = server.clone();
    let closing = tokio::spawn(async move { srv.close().await });
    tokio::task::yield_now().await;
    gate.release.resolve(());

    closing.await.expect("closing task").expect("server close succeeds");
    let attach_err = attach
        .await
        .expect("attach task")
        .expect_err("attach fails due to closed connection");
    assert!(attach_err.message().to_ascii_lowercase().contains("closed"));
    assert_eq!(host.latest_harness("session-1").expect("harness").close_count(), 1);
}

#[tokio::test]
async fn fails_shutdown_when_an_in_flight_acquisition_cannot_release_its_harness() {
    let host = TestServerHost::new();
    host.seed("session-1").await;
    host.set_next_harness_close_error(Some(Error::msg("close failed")));
    let server = create_server(host.clone());
    let client = connect(&server);
    client.hello(None).await.expect("hello");
    let gate = host.gate_next_open_session();

    let c1 = client.clone();
    let attach = tokio::spawn(async move { c1.attach(SERVER_ID, "session-1").await });
    gate.entered.wait().await;

    let srv = server.clone();
    let closing = tokio::spawn(async move { srv.close().await });
    tokio::task::yield_now().await;
    gate.release.resolve(());

    let close_err = closing.await.expect("closing task").expect_err("close should fail");
    assert!(close_err.message().contains("Failed to close routed Sessions"));
    let closed_err = server.closed().await.expect_err("server.closed should fail");
    assert!(closed_err.message().contains("Failed to close routed Sessions"));
    let attach_err = attach.await.expect("attach task").expect_err("attach fails");
    assert!(attach_err.message().to_ascii_lowercase().contains("closed"));
    let harness = host.latest_harness("session-1").expect("harness");
    assert_eq!(harness.close_count(), 1);
    harness
        .close(BACKGROUND_CONTEXT.clone())
        .await
        .expect("cleanup harness close");
}

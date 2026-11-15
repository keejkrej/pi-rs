use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use pi_js::abort::{AbortController, AbortReason};
use pi_js::testing::Recorder;
use pi_js::{BoxFuture, Error};
use pi_mcp::client::{ClientState, McpClientRoots};
use pi_mcp::protocol::jsonrpc::JsonRpcVersion;
use pi_mcp::testing::{InMemoryTransport, create_in_memory_transport_pair};
use pi_mcp::{
    JsonRpcErrorObject, JsonRpcErrorResponse, JsonRpcId, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse,
    JsonRpcSuccessResponse, LATEST_PROTOCOL_VERSION, McpAbortError, McpClient, McpClientOptions, McpError,
    McpRequestOptions, McpTimeoutError, McpTransport, ProgressNotification,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, from_value, json};
use tokio::sync::oneshot;

type ServerHandler = Arc<dyn Fn(JsonRpcRequest) -> BoxFuture<pi_js::Result<Value>> + Send + Sync>;

struct TestServer {
    transport: Arc<InMemoryTransport>,
    messages: Arc<Mutex<Vec<JsonRpcMessage>>>,
    handlers: Arc<Mutex<IndexMap<String, ServerHandler>>>,
}

impl TestServer {
    fn set_handler<F>(&self, method: &str, handler: F)
    where
        F: Fn(JsonRpcRequest) -> BoxFuture<pi_js::Result<Value>> + Send + Sync + 'static,
    {
        self.handlers
            .lock()
            .unwrap()
            .insert(method.to_string(), Arc::new(handler));
    }

    fn messages(&self) -> Vec<JsonRpcMessage> {
        self.messages.lock().unwrap().clone()
    }
}

async fn create_server() -> (Arc<InMemoryTransport>, TestServer) {
    let pair = create_in_memory_transport_pair();
    let client_transport = Arc::new(pair.client);
    let server_transport = Arc::new(pair.server);
    let handlers: Arc<Mutex<IndexMap<String, ServerHandler>>> = Arc::new(Mutex::new(IndexMap::new()));
    let messages: Arc<Mutex<Vec<JsonRpcMessage>>> = Arc::new(Mutex::new(Vec::new()));

    let msg_store = messages.clone();
    let handler_map = handlers.clone();
    let transport_for_cb = server_transport.clone();
    let _ = server_transport.on_message(Arc::new(move |message| {
        msg_store.lock().unwrap().push(message.clone());
        let JsonRpcMessage::Request(request) = message else {
            return;
        };
        let request = request.clone();
        let handler = handler_map.lock().unwrap().get(&request.method).cloned();
        let transport = transport_for_cb.clone();
        tokio::spawn(async move {
            let reply = match handler {
                None => Err(Error::typed(
                    "McpError",
                    McpError::new(-32601, format!("Method not found: {}", request.method), None),
                )),
                Some(h) => h(request.clone()).await,
            };
            let response = match reply {
                Ok(result) => JsonRpcMessage::Response(JsonRpcResponse::Success(JsonRpcSuccessResponse {
                    jsonrpc: JsonRpcVersion::V2,
                    id: request.id,
                    result,
                })),
                Err(err) => {
                    let (code, message, data) = if let Some(mcp_err) = err.downcast_ref::<McpError>() {
                        (mcp_err.code, mcp_err.message.clone(), mcp_err.data.clone())
                    } else {
                        (-32603, err.to_string(), None)
                    };
                    JsonRpcMessage::Response(JsonRpcResponse::Error(JsonRpcErrorResponse {
                        jsonrpc: JsonRpcVersion::V2,
                        id: request.id,
                        error: JsonRpcErrorObject { code, message, data },
                    }))
                }
            };
            let _ = transport.send(response).await;
        });
    }));

    server_transport.start().await.expect("start server transport");
    let server = TestServer {
        transport: server_transport,
        messages,
        handlers,
    };
    server.set_handler("initialize", |_| {
        Box::pin(async {
            Ok(json!({
                "protocolVersion": LATEST_PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": { "name": "test-server", "version": "1.0.0" },
                "instructions": "Use test tools."
            }))
        })
    });
    (client_transport, server)
}

async fn connect() -> (McpClient, TestServer) {
    let (client_transport, server) = create_server().await;
    let client = McpClient::new(McpClientOptions {
        name: "test-client".to_string(),
        version: "2.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: None,
        roots: None,
    });
    client.connect(client_transport).await.expect("connect");
    (client, server)
}

#[tokio::test]
async fn initializes_the_connection_before_exposing_server_information() {
    let (client, server) = connect().await;
    assert_eq!(client.connection_state(), ClientState::Connected);
    assert_eq!(client.protocol_version().as_deref(), Some(LATEST_PROTOCOL_VERSION));
    assert_eq!(
        client.server_info(),
        Some(from_value(json!({ "name": "test-server", "version": "1.0.0" })).unwrap())
    );
    assert_eq!(
        client.server_capabilities(),
        Some(from_value(json!({ "tools": { "listChanged": true } })).unwrap())
    );
    assert_eq!(client.instructions().as_deref(), Some("Use test tools."));

    let expected_messages: Vec<JsonRpcMessage> = from_value(json!([
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": LATEST_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "2.0.0" }
            }
        },
        { "jsonrpc": "2.0", "method": "notifications/initialized" }
    ]))
    .unwrap();
    assert_eq!(server.messages(), expected_messages);
    client.close().await.expect("close");
}

#[tokio::test]
async fn paginates_tools_and_preserves_protocol_tool_definitions() {
    let (client, server) = connect().await;
    server.set_handler("tools/list", |request| {
        Box::pin(async move {
            let cursor = request
                .params
                .as_ref()
                .and_then(|p| p.get("cursor"))
                .and_then(|v| v.as_str());
            if cursor.is_none() {
                Ok(json!({
                    "tools": [{ "name": "search", "description": "Search", "inputSchema": { "type": "object" } }],
                    "nextCursor": "page-2"
                }))
            } else {
                // Some servers end pagination with an empty cursor instead of omitting it.
                Ok(json!({
                    "tools": [{
                        "name": "read",
                        "inputSchema": { "type": "object" },
                        "outputSchema": { "type": "object" },
                        "annotations": { "readOnlyHint": true }
                    }],
                    "nextCursor": ""
                }))
            }
        })
    });

    let tools = client.list_tools(None).await.expect("list_tools");
    let expected: Vec<pi_mcp::Tool> = from_value(json!([
        { "name": "search", "description": "Search", "inputSchema": { "type": "object" } },
        {
            "name": "read",
            "inputSchema": { "type": "object" },
            "outputSchema": { "type": "object" },
            "annotations": { "readOnlyHint": true }
        }
    ]))
    .unwrap();
    assert_eq!(tools, expected);
    client.close().await.expect("close");
}

#[tokio::test]
async fn lists_and_reads_resources() {
    let (client, server) = connect().await;
    server.set_handler("resources/list", |request| {
        Box::pin(async move {
            let cursor = request
                .params
                .as_ref()
                .and_then(|p| p.get("cursor"))
                .and_then(|v| v.as_str());
            if cursor.is_none() {
                Ok(json!({
                    "resources": [{ "uri": "file:///a", "name": "a", "mimeType": "text/plain" }],
                    "nextCursor": "2"
                }))
            } else {
                Ok(json!({
                    "resources": [{ "uri": "file:///b" }]
                }))
            }
        })
    });
    server.set_handler("resources/templates/list", |_| {
        Box::pin(async {
            Ok(json!({
                "resourceTemplates": [{ "uriTemplate": "repo://{owner}/{repo}", "name": "repo" }]
            }))
        })
    });
    server.set_handler("resources/read", |request| {
        Box::pin(async move {
            let uri = request
                .params
                .as_ref()
                .and_then(|p| p.get("uri"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Ok(json!({
                "contents": [{ "uri": uri, "text": "hello" }]
            }))
        })
    });

    // A missing name falls back to the URI.
    let expected_resources: Vec<pi_mcp::Resource> = from_value(json!([
        { "uri": "file:///a", "name": "a", "mimeType": "text/plain" },
        { "uri": "file:///b", "name": "file:///b" }
    ]))
    .unwrap();
    assert_eq!(client.list_resources(None).await.unwrap(), expected_resources);

    let expected_templates: Vec<pi_mcp::ResourceTemplate> = from_value(json!([
        { "uriTemplate": "repo://{owner}/{repo}", "name": "repo" }
    ]))
    .unwrap();
    assert_eq!(client.list_resource_templates(None).await.unwrap(), expected_templates);

    // Single pages pass the cursor through.
    assert_eq!(
        client.list_resources_page(None, None).await.unwrap(),
        from_value(json!({
            "resources": [{ "uri": "file:///a", "name": "a", "mimeType": "text/plain" }],
            "nextCursor": "2"
        }))
        .unwrap()
    );
    assert_eq!(
        client.list_resources_page(Some("2"), None).await.unwrap(),
        from_value(json!({
            "resources": [{ "uri": "file:///b", "name": "file:///b" }]
        }))
        .unwrap()
    );
    assert_eq!(
        client.read_resource("file:///a", None).await.unwrap(),
        from_value(json!({
            "contents": [{ "uri": "file:///a", "text": "hello" }]
        }))
        .unwrap()
    );

    server.set_handler("resources/read", |_| {
        Box::pin(async { Ok(json!({ "contents": [{ "uri": "file:///a" }] })) })
    });
    let err = client.read_resource("file:///a", None).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("Invalid contents in MCP resources/read result"),
        "unexpected error: {err}"
    );

    server.set_handler("resources/list", |_| {
        Box::pin(async { Ok(json!({ "resources": [{ "name": "no uri" }] })) })
    });
    let err = client.list_resources(None).await.unwrap_err();
    assert!(
        err.to_string().contains("Invalid entry in MCP resources/list result"),
        "unexpected error: {err}"
    );

    client.close().await.expect("close");
}

#[tokio::test]
async fn returns_structured_tool_content_and_surfaces_json_rpc_errors() {
    let (client, server) = connect().await;
    server.set_handler("tools/call", |request| {
        Box::pin(async move {
            let params = request.params.as_ref().unwrap();
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name == "fail" {
                return Err(Error::typed(
                    "McpError",
                    McpError::new(1234, "tool failed", Some(json!({ "retryable": false }))),
                ));
            }
            let count = params.get("arguments").and_then(|a| a.get("count")).cloned();
            Ok(json!({
                "content": [{ "type": "text", "text": "ok" }],
                "structuredContent": { "count": count }
            }))
        })
    });

    let mut args = IndexMap::new();
    args.insert("count".to_string(), json!(3));
    assert_eq!(
        client.call_tool("count", Some(args), None).await.unwrap(),
        from_value(json!({
            "content": [{ "type": "text", "text": "ok" }],
            "structuredContent": { "count": 3 }
        }))
        .unwrap()
    );

    let err = client.call_tool("fail", None, None).await.unwrap_err();
    let mcp_err = err.downcast_ref::<McpError>().expect("should downcast to McpError");
    assert_eq!(mcp_err.code, 1234);
    assert_eq!(mcp_err.message, "tool failed");
    assert_eq!(mcp_err.data, Some(json!({ "retryable": false })));
    client.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn renews_the_timeout_on_progress() {
    let (client, server) = connect().await;
    let client = Arc::new(client);
    let server_transport = server.transport.clone();
    server.set_handler("tools/call", move |request| {
        let transport = server_transport.clone();
        Box::pin(async move {
            let token = request
                .params
                .as_ref()
                .and_then(|p| p.get("_meta"))
                .and_then(|m| m.get("progressToken"))
                .cloned()
                .unwrap();
            let t2 = transport.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                let msg: JsonRpcMessage = from_value(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": { "progressToken": token, "progress": 1, "total": 2 }
                }))
                .unwrap();
                let _ = t2.send(msg).await;
            });
            tokio::time::sleep(Duration::from_millis(80)).await;
            Ok(json!({ "content": [{ "type": "text", "text": "done" }] }))
        })
    });

    let progress = Recorder::<ProgressNotification>::new();
    let progress_cb = progress.clone();
    let client_for_task = client.clone();
    let call_task = tokio::spawn(async move {
        client_for_task
            .call_tool(
                "slow",
                None,
                Some(McpRequestOptions {
                    signal: None,
                    timeout_ms: Some(50),
                    on_progress: Some(Arc::new(move |note| progress_cb.push(note))),
                }),
            )
            .await
    });

    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(40)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(40)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    let result = call_task.await.unwrap().expect("call_tool succeeds");
    assert_eq!(
        result,
        from_value(json!({ "content": [{ "type": "text", "text": "done" }] })).unwrap()
    );
    assert_eq!(
        progress.calls(),
        vec![ProgressNotification {
            progress_token: JsonRpcId::Number(2.0),
            progress: 1.0,
            total: Some(2.0),
            message: None,
        }]
    );
    client.close().await.expect("close");
}

#[tokio::test]
async fn cancels_aborted_and_timed_out_requests() {
    let (client, server) = connect().await;
    let client = Arc::new(client);
    server.set_handler("tools/call", |_| {
        Box::pin(async { std::future::pending::<pi_js::Result<Value>>().await })
    });

    let controller = AbortController::new();
    let signal = controller.signal();
    let client_clone = client.clone();
    let aborted = tokio::spawn(async move {
        client_clone
            .call_tool(
                "wait",
                None,
                Some(McpRequestOptions {
                    signal: Some(signal),
                    timeout_ms: None,
                    on_progress: None,
                }),
            )
            .await
    });
    tokio::task::yield_now().await;
    controller.abort(Some(AbortReason::value(json!("stop"))));
    let err = aborted.await.unwrap().unwrap_err();
    assert!(
        err.downcast_ref::<McpAbortError>().is_some(),
        "expected McpAbortError, got {err}"
    );
    tokio::time::sleep(Duration::from_millis(10)).await;
    let cancelled_note: JsonRpcMessage = from_value(json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": 2, "reason": "stop" }
    }))
    .unwrap();
    assert!(
        server.messages().contains(&cancelled_note),
        "messages: {:?}",
        server.messages()
    );

    let timed_out = client
        .call_tool(
            "wait",
            None,
            Some(McpRequestOptions {
                signal: None,
                timeout_ms: Some(5),
                on_progress: None,
            }),
        )
        .await
        .unwrap_err();
    assert!(
        timed_out.downcast_ref::<McpTimeoutError>().is_some(),
        "expected McpTimeoutError, got {timed_out}"
    );
    client.close().await.expect("close");
}

#[tokio::test]
async fn reports_transport_errors_without_failing_pending_requests() {
    let (client_transport, server) = create_server().await;
    let client = Arc::new(McpClient::new(McpClientOptions {
        name: "test-client".to_string(),
        version: "1.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: None,
        roots: None,
    }));
    client.connect(client_transport.clone()).await.unwrap();

    let errors = Recorder::<String>::new();
    let errors_cb = errors.clone();
    let _ = client.on_error(Arc::new(move |err| errors_cb.push(err.to_string())));

    let (tx, rx) = oneshot::channel::<()>();
    let rx = Arc::new(Mutex::new(Some(rx)));
    server.set_handler("tools/call", move |_| {
        let rx = rx.lock().unwrap().take().unwrap();
        Box::pin(async move {
            let _ = rx.await;
            Ok(json!({ "content": [] }))
        })
    });

    let client_clone = client.clone();
    let call = tokio::spawn(async move { client_clone.call_tool("wait", None, None).await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    client_transport.emit_error(&Error::msg("stray log line"));
    let _ = tx.send(());

    assert_eq!(
        call.await.unwrap().unwrap(),
        from_value(json!({ "content": [] })).unwrap()
    );
    assert_eq!(errors.calls(), vec!["stray log line".to_string()]);
    client.close().await.expect("close");
}

#[tokio::test]
async fn accepts_servers_that_answer_with_an_older_protocol_version() {
    let (client_transport, server) = create_server().await;
    server.set_handler("initialize", |_| {
        Box::pin(async {
            Ok(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "serverInfo": { "name": "old-server", "version": "0.1.0" }
            }))
        })
    });
    let client = McpClient::new(McpClientOptions {
        name: "test-client".to_string(),
        version: "1.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: None,
        roots: None,
    });
    client.connect(client_transport).await.unwrap();
    assert_eq!(client.protocol_version().as_deref(), Some("2024-11-05"));
    client.close().await.unwrap();

    let (unsupported_transport, unsupported_server) = create_server().await;
    unsupported_server.set_handler("initialize", |_| {
        Box::pin(async {
            Ok(json!({
                "protocolVersion": "1999-01-01",
                "capabilities": {},
                "serverInfo": { "name": "ancient-server", "version": "0.1.0" }
            }))
        })
    });
    let rejected = McpClient::new(McpClientOptions {
        name: "test-client".to_string(),
        version: "1.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: None,
        roots: None,
    });
    let err = rejected.connect(unsupported_transport).await.unwrap_err();
    assert!(
        err.to_string().contains("unsupported protocol version"),
        "unexpected error: {err}"
    );
    assert_eq!(rejected.connection_state(), ClientState::Closed);
}

#[tokio::test]
async fn defaults_missing_tool_result_content_to_an_empty_list() {
    let (client, server) = connect().await;
    server.set_handler("tools/call", |_| {
        Box::pin(async { Ok(json!({ "structuredContent": { "ok": true } })) })
    });
    assert_eq!(
        client.call_tool("structured", None, None).await.unwrap(),
        from_value(json!({ "content": [], "structuredContent": { "ok": true } })).unwrap()
    );

    server.set_handler("tools/call", |_| {
        Box::pin(async { Ok(json!({ "content": "not a list" })) })
    });
    let err = client.call_tool("broken", None, None).await.unwrap_err();
    assert!(
        err.to_string().contains("Invalid MCP tools/call result"),
        "unexpected error: {err}"
    );
    client.close().await.expect("close");
}

#[tokio::test]
async fn does_not_send_notifications_cancelled_for_a_timed_out_initialize() {
    let (client_transport, server) = create_server().await;
    server.set_handler("initialize", |_| {
        Box::pin(async { std::future::pending::<pi_js::Result<Value>>().await })
    });
    let client = McpClient::new(McpClientOptions {
        name: "test-client".to_string(),
        version: "1.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: Some(5),
        roots: None,
    });
    let err = client.connect(client_transport).await.unwrap_err();
    assert!(
        err.downcast_ref::<McpTimeoutError>().is_some(),
        "expected McpTimeoutError, got {err}"
    );
    tokio::time::sleep(Duration::from_millis(10)).await;
    let has_cancelled = server.messages().iter().any(|msg| {
        matches!(
            msg,
            JsonRpcMessage::Notification(n) if n.method == "notifications/cancelled"
        )
    });
    assert!(!has_cancelled);
}

#[tokio::test]
async fn notifies_close_listeners_once_when_the_transport_drops() {
    let (client, server) = connect().await;
    let client = Arc::new(client);
    let closed = Recorder::<()>::new();
    let closed_cb = closed.clone();
    let _ = client.on_close(Arc::new(move || closed_cb.push(())));
    server.set_handler("tools/call", |_| {
        Box::pin(async { std::future::pending::<pi_js::Result<Value>>().await })
    });

    let client_clone = client.clone();
    let pending = tokio::spawn(async move { client_clone.call_tool("wait", None, None).await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    server.transport.close().await.unwrap();

    let err = pending.await.unwrap().unwrap_err();
    assert!(
        err.to_string().contains("MCP connection closed"),
        "unexpected error: {err}"
    );
    assert_eq!(client.connection_state(), ClientState::Closed);
    client.close().await.unwrap();
    assert_eq!(closed.len(), 1);
}

#[tokio::test]
async fn answers_roots_list_and_dispatches_notifications() {
    let (client_transport, server) = create_server().await;
    let roots = from_value(json!([{ "uri": "file:///workspace", "name": "workspace" }])).unwrap();
    let client = McpClient::new(McpClientOptions {
        name: "test-client".to_string(),
        version: "1.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: None,
        roots: Some(McpClientRoots::List(roots)),
    });
    client.connect(client_transport).await.unwrap();

    let changed = Recorder::<Option<Value>>::new();
    let changed_cb = changed.clone();
    let _ = client.on_notification(
        "notifications/tools/list_changed",
        Arc::new(move |params| changed_cb.push(params)),
    );

    let roots_req: JsonRpcMessage =
        from_value(json!({ "jsonrpc": "2.0", "id": "roots", "method": "roots/list" })).unwrap();
    let tools_note: JsonRpcMessage =
        from_value(json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" })).unwrap();
    server.transport.send(roots_req).await.unwrap();
    server.transport.send(tools_note).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    let expected_roots_resp: JsonRpcMessage = from_value(json!({
        "jsonrpc": "2.0",
        "id": "roots",
        "result": { "roots": [{ "uri": "file:///workspace", "name": "workspace" }] }
    }))
    .unwrap();
    assert!(
        server.messages().contains(&expected_roots_resp),
        "messages: {:?}",
        server.messages()
    );
    assert_eq!(changed.calls(), vec![None]);
    client.close().await.unwrap();
}

#[tokio::test]
async fn rejects_malformed_optional_fields_in_resources_without_panicking() {
    let (client, server) = connect().await;
    server.set_handler("resources/list", |_| {
        Box::pin(async {
            Ok(json!({
                "resources": [{ "uri": "file:///bad", "title": 123 }]
            }))
        })
    });
    server.set_handler("resources/templates/list", |_| {
        Box::pin(async {
            Ok(json!({
                "resourceTemplates": [{ "uriTemplate": "repo://{owner}", "mimeType": false }]
            }))
        })
    });

    assert!(client.list_resources(None).await.is_err());
    assert!(client.list_resource_templates(None).await.is_err());
    client.close().await.unwrap();
}

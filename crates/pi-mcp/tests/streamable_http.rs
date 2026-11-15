mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use bytes::Bytes;
use indexmap::IndexMap;
use pi_js::BoxFuture;
use pi_js::fetch::testing::{chunked_response, mock_fetch};
use pi_js::fetch::{Headers, Request, Response};
use pi_mcp::auth_provider::McpFetchInit;
use pi_mcp::transports::streamable_http::{
    ConsumeSseOptions, SseEvent, StreamableHttpReconnectOptions, consume_sse_stream,
};
use pi_mcp::{
    AuthProvider, LATEST_PROTOCOL_VERSION, McpAuthRequiredError, McpClient, McpClientOptions, McpFetch,
    McpSessionExpiredError, StreamableHttpTransport, StreamableHttpTransportOptions, Tool, UnauthorizedContext,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, from_value, json};
use support::helpers::{TestRequest, TestResponse, TestServers};
use tokio::sync::watch;

#[derive(Clone)]
struct RecordedRequest {
    method: String,
    headers: hyper::HeaderMap,
    message: Option<Value>,
}

fn default_client() -> McpClient {
    McpClient::new(McpClientOptions {
        name: "http-test".to_string(),
        version: "1.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: None,
        roots: None,
    })
}

async fn start_server<F>(servers: &mut TestServers, handler: F) -> (String, Arc<Mutex<Vec<RecordedRequest>>>)
where
    F: Fn(TestRequest, Arc<Mutex<Vec<RecordedRequest>>>) -> BoxFuture<TestResponse> + Send + Sync + 'static,
{
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_handler = requests.clone();
    let origin = servers
        .listen(move |req, _origin| handler(req, requests_for_handler.clone()))
        .await;
    (format!("{origin}/mcp"), requests)
}

async fn protocol_handler(
    request: TestRequest,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    body_override: Option<Value>,
) -> TestResponse {
    if request.method == "GET" {
        requests.lock().unwrap().push(RecordedRequest {
            method: "GET".to_string(),
            headers: request.headers,
            message: None,
        });
        return TestResponse::empty(405);
    }
    if request.method == "DELETE" {
        requests.lock().unwrap().push(RecordedRequest {
            method: "DELETE".to_string(),
            headers: request.headers,
            message: None,
        });
        return TestResponse::empty(200);
    }
    let message = match body_override {
        Some(v) => v,
        None => request.body_json().unwrap_or(Value::Null),
    };
    requests.lock().unwrap().push(RecordedRequest {
        method: request.method.clone(),
        headers: request.headers.clone(),
        message: Some(message.clone()),
    });
    if message.get("id").is_none() {
        return TestResponse::empty(202);
    }
    let method = message.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    if method == "initialize" {
        return TestResponse::json_with_headers(
            200,
            &[("mcp-session-id", "session-1")],
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "http-fixture", "version": "1.0.0" }
                }
            }),
        );
    }
    if method == "tools/list" {
        return TestResponse::json(
            200,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] }
            }),
        );
    }
    let payload = pi_js::json::stringify(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "content": [{ "type": "text", "text": "hello" }] }
    }));
    TestResponse::sse_end(200, format!("id: tool-result\ndata: {payload}\n\n"))
}

#[tokio::test]
async fn parses_chunked_crlf_events_comments_ids_and_multiline_data() {
    let chunks = vec![": keepalive\r\nid: 7\r\ndata: {\"one\":\r\n", "data: 1}\r\n\r\n"];
    let response = chunked_response(200, &[], chunks);
    let stream = response.body_stream().expect("body stream");
    let events = Arc::new(Mutex::new(Vec::<SseEvent>::new()));
    let events_cb = events.clone();

    consume_sse_stream(
        stream,
        ConsumeSseOptions {
            max_event_bytes: None,
            on_event: Arc::new(move |ev| events_cb.lock().unwrap().push(ev)),
            on_id: None,
            on_retry: None,
        },
    )
    .await
    .expect("consume_sse_stream");

    assert_eq!(
        *events.lock().unwrap(),
        vec![SseEvent {
            event: None,
            data: "{\"one\":\n1}".to_string(),
            id: Some("7".to_string()),
        }]
    );
}

#[tokio::test]
async fn rejects_events_whose_data_lines_exceed_the_limit_without_a_blank_line() {
    let sent = Arc::new(AtomicUsize::new(0));
    let sent_cb = sent.clone();
    let pull = futures::stream::poll_fn(move |_cx| {
        let idx = sent_cb.fetch_add(1, Ordering::SeqCst);
        if idx > 1000 {
            Poll::Ready(None)
        } else {
            Poll::Ready(Some(Ok(Bytes::from_static(b"data: xxxxxxxxxxxxxxxx\n"))))
        }
    });
    let response = Response::from_stream(200, Headers::new(), pull);
    let stream = response.body_stream().expect("body stream");

    let err = consume_sse_stream(
        stream,
        ConsumeSseOptions {
            max_event_bytes: Some(256),
            on_event: Arc::new(|_| {}),
            on_id: None,
            on_retry: None,
        },
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("MCP SSE event exceeds 256 bytes"),
        "unexpected error: {err}"
    );
    assert!(sent.load(Ordering::SeqCst) < 100);
}

#[tokio::test]
async fn handles_json_and_sse_responses_with_session_and_protocol_headers() {
    let mut servers = TestServers::new();
    let (url, requests) = start_server(&mut servers, |req, reqs| Box::pin(protocol_handler(req, reqs, None))).await;

    let transport = Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
        url,
        headers: None,
        fetch: None,
        open_get_stream: None,
        max_message_bytes: None,
        auth_provider: None,
        reconnect: None,
    }));
    let client = default_client();
    client.connect(transport.clone()).await.expect("connect");

    assert_eq!(transport.session_id().as_deref(), Some("session-1"));
    let expected_tools: Vec<Tool> =
        from_value(json!([{ "name": "echo", "inputSchema": { "type": "object" } }])).unwrap();
    assert_eq!(client.list_tools(None).await.unwrap(), expected_tools);

    let mut args = IndexMap::new();
    args.insert("text".to_string(), json!("hello"));
    assert_eq!(
        client.call_tool("echo", Some(args), None).await.unwrap(),
        from_value(json!({ "content": [{ "type": "text", "text": "hello" }] })).unwrap()
    );
    client.close().await.unwrap();

    let recorded = requests.lock().unwrap();
    let list_req = recorded
        .iter()
        .find(|entry| {
            entry
                .message
                .as_ref()
                .and_then(|m| m.get("method"))
                .and_then(|v| v.as_str())
                == Some("tools/list")
        })
        .expect("tools/list request recorded");
    assert_eq!(
        list_req.headers.get("mcp-session-id").and_then(|v| v.to_str().ok()),
        Some("session-1")
    );
    assert_eq!(
        list_req
            .headers
            .get("mcp-protocol-version")
            .and_then(|v| v.to_str().ok()),
        Some(LATEST_PROTOCOL_VERSION)
    );
    assert!(recorded.iter().any(|entry| entry.method == "GET"));
    assert!(recorded.iter().any(|entry| entry.method == "DELETE"));
    drop(recorded);
    servers.close().await;
}

#[tokio::test]
async fn classifies_authentication_failures() {
    let mut servers = TestServers::new();
    let (url, _) = start_server(&mut servers, |_req, _| {
        Box::pin(async {
            TestResponse::with_headers(
                401,
                &[(
                    "www-authenticate",
                    "Bearer resource_metadata=\"https://example.com/meta\"",
                )],
                "login required",
            )
        })
    })
    .await;

    let client = default_client();
    let err = client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url,
            headers: None,
            fetch: None,
            open_get_stream: None,
            max_message_bytes: None,
            auth_provider: None,
            reconnect: None,
        })))
        .await
        .unwrap_err();
    let auth_err = err
        .downcast_ref::<McpAuthRequiredError>()
        .expect("expected McpAuthRequiredError");
    assert_eq!(auth_err.status, 401);
    assert_eq!(auth_err.body, "login required");
    assert_eq!(
        auth_err.www_authenticate.as_deref(),
        Some("Bearer resource_metadata=\"https://example.com/meta\"")
    );
    servers.close().await;
}

#[tokio::test]
async fn fails_only_the_request_whose_sse_stream_breaks() {
    let mut servers = TestServers::new();
    let (gate_tx, gate_rx) = watch::channel(false);
    let (url, _) = start_server(&mut servers, move |req, reqs| {
        let mut gate_rx = gate_rx.clone();
        Box::pin(async move {
            if req.method != "POST" {
                return protocol_handler(req, reqs, None).await;
            }
            let message = req.body_json().unwrap_or(Value::Null);
            let name = message
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if name == "broken" {
                return TestResponse::sse_end(200, "data: not json\n\n");
            }
            if name == "slow" {
                let _ = gate_rx.wait_for(|&v| v).await;
            }
            protocol_handler(req, reqs, Some(message)).await
        })
    })
    .await;

    let client = Arc::new(default_client());
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let errors_cb = errors.clone();
    let _ = client.on_error(Arc::new(move |err| errors_cb.lock().unwrap().push(err.to_string())));
    client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url,
            headers: None,
            fetch: None,
            open_get_stream: Some(false),
            max_message_bytes: None,
            auth_provider: None,
            reconnect: None,
        })))
        .await
        .unwrap();

    let client_for_slow = client.clone();
    let slow = tokio::spawn(async move { client_for_slow.call_tool("slow", None, None).await });
    let broken_err = client.call_tool("broken", None, None).await.unwrap_err();
    assert!(
        broken_err.to_string().contains("MCP response stream failed"),
        "unexpected error: {broken_err}"
    );
    let _ = gate_tx.send(true);
    assert_eq!(
        slow.await.unwrap().unwrap(),
        from_value(json!({ "content": [{ "type": "text", "text": "hello" }] })).unwrap()
    );
    assert_eq!(errors.lock().unwrap().len(), 1);
    client.close().await.unwrap();
    servers.close().await;
}

#[tokio::test]
async fn opens_the_get_stream_after_initialization_and_sends_last_event_id_only_when_resuming() {
    let mut servers = TestServers::new();
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let order_cb = order.clone();
    let (url, requests) = start_server(&mut servers, move |req, reqs| {
        let order = order_cb.clone();
        Box::pin(async move {
            if req.method == "POST" {
                let message = req.body_json().unwrap_or(Value::Null);
                let method = message.get("method").and_then(|v| v.as_str()).unwrap_or("").to_string();
                order.lock().unwrap().push(method);
                return protocol_handler(req, reqs, Some(message)).await;
            }
            order.lock().unwrap().push(req.method.clone());
            protocol_handler(req, reqs, None).await
        })
    })
    .await;

    let client = default_client();
    client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url,
            headers: None,
            fetch: None,
            open_get_stream: None,
            max_message_bytes: None,
            auth_provider: None,
            reconnect: None,
        })))
        .await
        .unwrap();

    let _ = client.call_tool("echo", None, None).await.unwrap();
    let _ = client.list_tools(None).await.unwrap();
    client.close().await.unwrap();

    let ord = order.lock().unwrap();
    let get_idx = ord.iter().position(|m| m == "GET").expect("GET recorded");
    let init_idx = ord
        .iter()
        .position(|m| m == "notifications/initialized")
        .expect("notifications/initialized recorded");
    assert!(get_idx > init_idx, "order: {:?}", *ord);
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|entry| !entry.headers.contains_key("last-event-id"))
    );
    servers.close().await;
}

#[tokio::test]
async fn resumes_a_response_stream_the_server_closed_before_answering() {
    let mut servers = TestServers::new();
    let resume_headers = Arc::new(Mutex::new(Vec::<String>::new()));
    let resume_cb = resume_headers.clone();
    let (url, _) = start_server(&mut servers, move |req, reqs| {
        let resume_headers = resume_cb.clone();
        Box::pin(async move {
            if req.method == "GET"
                && let Some(last_id) = req.header("last-event-id")
            {
                resume_headers.lock().unwrap().push(last_id.to_string());
                let payload = pi_js::json::stringify(&json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": { "content": [{ "type": "text", "text": "resumed" }] }
                }));
                return TestResponse::sse_end(200, format!("id: 2\ndata: {payload}\n\n"));
            }
            if req.method != "POST" {
                return protocol_handler(req, reqs, None).await;
            }
            let message = req.body_json().unwrap_or(Value::Null);
            if message.get("method").and_then(|v| v.as_str()) != Some("tools/call") {
                return protocol_handler(req, reqs, Some(message)).await;
            }
            // Priming event (ID, no data) and a retry hint, then the server drops the stream.
            TestResponse::sse_end(200, "id: 1\nretry: 5\ndata:\n\n")
        })
    })
    .await;

    let client = default_client();
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let errors_cb = errors.clone();
    let _ = client.on_error(Arc::new(move |err| errors_cb.lock().unwrap().push(err.to_string())));
    client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url,
            headers: None,
            fetch: None,
            open_get_stream: Some(false),
            max_message_bytes: None,
            auth_provider: None,
            reconnect: None,
        })))
        .await
        .unwrap();

    assert_eq!(
        client.call_tool("echo", None, None).await.unwrap(),
        from_value(json!({ "content": [{ "type": "text", "text": "resumed" }] })).unwrap()
    );
    assert_eq!(*resume_headers.lock().unwrap(), vec!["1".to_string()]);
    assert!(errors.lock().unwrap().is_empty());
    client.close().await.unwrap();
    servers.close().await;
}

#[tokio::test]
async fn fails_a_request_whose_response_stream_ends_without_an_answer() {
    let mut servers = TestServers::new();
    let (url, _) = start_server(&mut servers, |req, reqs| {
        Box::pin(async move {
            if req.method != "POST" {
                return protocol_handler(req, reqs, None).await;
            }
            let message = req.body_json().unwrap_or(Value::Null);
            if message.get("method").and_then(|v| v.as_str()) != Some("tools/call") {
                return protocol_handler(req, reqs, Some(message)).await;
            }
            TestResponse::sse_end(200, ": nothing here\n\n")
        })
    })
    .await;

    let client = default_client();
    client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url,
            headers: None,
            fetch: None,
            open_get_stream: Some(false),
            max_message_bytes: None,
            auth_provider: None,
            reconnect: None,
        })))
        .await
        .unwrap();

    let err = client
        .call_tool(
            "echo",
            None,
            Some(pi_mcp::McpRequestOptions {
                signal: None,
                timeout_ms: Some(5_000),
                on_progress: None,
            }),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("MCP response stream failed: stream ended without a response"),
        "unexpected error: {err}"
    );
    client.close().await.unwrap();
    servers.close().await;
}

#[tokio::test]
async fn reconnects_the_get_stream_after_it_drops() {
    let mut servers = TestServers::new();
    let gets = Arc::new(AtomicUsize::new(0));
    let last_event_ids = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let gets_cb = gets.clone();
    let ids_cb = last_event_ids.clone();
    let (url, _) = start_server(&mut servers, move |req, reqs| {
        let gets = gets_cb.clone();
        let last_event_ids = ids_cb.clone();
        Box::pin(async move {
            if req.method != "GET" {
                return protocol_handler(req, reqs, None).await;
            }
            let count = gets.fetch_add(1, Ordering::SeqCst) + 1;
            last_event_ids
                .lock()
                .unwrap()
                .push(req.header("last-event-id").map(str::to_string));
            let notification = pi_js::json::stringify(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/tools/list_changed"
            }));
            if count == 1 {
                return TestResponse::sse_end(200, format!("id: g1\ndata: {notification}\n\n"));
            }
            TestResponse::sse_open(200, format!("id: g2\ndata: {notification}\n\n"))
        })
    })
    .await;

    let client = default_client();
    let changes = Arc::new(AtomicUsize::new(0));
    let (done_tx, mut done_rx) = watch::channel(false);
    let changes_cb = changes.clone();
    let _ = client.on_notification(
        "notifications/tools/list_changed",
        Arc::new(move |_| {
            if changes_cb.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                let _ = done_tx.send(true);
            }
        }),
    );

    client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url,
            headers: None,
            fetch: None,
            open_get_stream: None,
            max_message_bytes: None,
            auth_provider: None,
            reconnect: Some(StreamableHttpReconnectOptions {
                initial_delay_ms: Some(1),
                max_delay_ms: None,
                max_retries: None,
            }),
        })))
        .await
        .unwrap();

    let _ = done_rx.wait_for(|&v| v).await;
    assert_eq!(*last_event_ids.lock().unwrap(), vec![None, Some("g1".to_string())]);
    client.close().await.unwrap();
    servers.close().await;
}

#[tokio::test]
async fn rejects_a_request_the_server_accepts_without_a_response() {
    let mut servers = TestServers::new();
    let (url, _) = start_server(&mut servers, |req, reqs| {
        Box::pin(async move {
            if req.method != "POST" {
                return protocol_handler(req, reqs, None).await;
            }
            let message = req.body_json().unwrap_or(Value::Null);
            if message.get("method").and_then(|v| v.as_str()) != Some("tools/call") {
                return protocol_handler(req, reqs, Some(message)).await;
            }
            TestResponse::empty(202)
        })
    })
    .await;

    let client = default_client();
    client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url,
            headers: None,
            fetch: None,
            open_get_stream: Some(false),
            max_message_bytes: None,
            auth_provider: None,
            reconnect: None,
        })))
        .await
        .unwrap();

    let err = client.call_tool("echo", None, None).await.unwrap_err();
    assert!(
        err.to_string().contains("without a response"),
        "unexpected error: {err}"
    );
    client.close().await.unwrap();
    servers.close().await;
}

#[tokio::test]
async fn includes_the_response_body_in_http_errors() {
    let mut servers = TestServers::new();
    let (url, _) = start_server(&mut servers, |_req, _| {
        Box::pin(async { TestResponse::text(400, "Invalid Accept header") })
    })
    .await;

    let client = default_client();
    let err = client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url,
            headers: None,
            fetch: None,
            open_get_stream: None,
            max_message_bytes: None,
            auth_provider: None,
            reconnect: None,
        })))
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("MCP HTTP request failed with status 400: Invalid Accept header"),
        "unexpected error: {err}"
    );
    servers.close().await;
}

struct SequenceAuthProvider {
    token: Mutex<Option<String>>,
    seen: Mutex<Vec<(u16, Option<String>)>>,
}

#[async_trait::async_trait]
impl AuthProvider for SequenceAuthProvider {
    async fn token(&self) -> pi_js::Result<Option<String>> {
        Ok(self.token.lock().unwrap().clone())
    }

    fn on_unauthorized(&self, context: UnauthorizedContext) -> Option<BoxFuture<pi_js::Result<()>>> {
        let status = context.response.status();
        self.seen.lock().unwrap().push((status, context.token));
        let mut guard = self.token.lock().unwrap();
        if status == 401 {
            *guard = Some("new".to_string());
        } else {
            *guard = Some("admin".to_string());
        }
        Some(Box::pin(async { Ok(()) }))
    }
}

#[tokio::test]
async fn hands_401_and_insufficient_scope_403_responses_to_the_auth_provider_with_the_rejected_token() {
    let mut servers = TestServers::new();
    let (url, _) = start_server(&mut servers, |req, reqs| {
        Box::pin(async move {
            if req.method != "POST" {
                return protocol_handler(req, reqs, None).await;
            }
            let auth = req.header("authorization").map(str::to_string);
            let message = req.body_json().unwrap_or(Value::Null);
            if auth.as_deref() == Some("Bearer old") {
                return TestResponse::with_headers(401, &[("www-authenticate", "Bearer")], "");
            }
            if message.get("method").and_then(|v| v.as_str()) == Some("tools/call")
                && auth.as_deref() == Some("Bearer new")
            {
                return TestResponse::with_headers(
                    403,
                    &[(
                        "www-authenticate",
                        "Bearer error=\"insufficient_scope\", scope=\"admin\"",
                    )],
                    "",
                );
            }
            protocol_handler(req, reqs, Some(message)).await
        })
    })
    .await;

    let provider = Arc::new(SequenceAuthProvider {
        token: Mutex::new(Some("old".to_string())),
        seen: Mutex::new(Vec::new()),
    });
    let client = default_client();
    client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url,
            headers: None,
            fetch: None,
            open_get_stream: Some(false),
            max_message_bytes: None,
            auth_provider: Some(provider.clone()),
            reconnect: None,
        })))
        .await
        .unwrap();

    assert_eq!(
        client.call_tool("echo", None, None).await.unwrap(),
        from_value(json!({ "content": [{ "type": "text", "text": "hello" }] })).unwrap()
    );
    assert_eq!(
        *provider.seen.lock().unwrap(),
        vec![(401, Some("old".to_string())), (403, Some("new".to_string())),]
    );
    client.close().await.unwrap();
    servers.close().await;
}

struct StrictFetchAuth {
    token: Arc<Mutex<Option<String>>>,
    url: String,
}

#[async_trait::async_trait]
impl AuthProvider for StrictFetchAuth {
    async fn token(&self) -> pi_js::Result<Option<String>> {
        Ok(self.token.lock().unwrap().clone())
    }

    fn on_unauthorized(&self, context: UnauthorizedContext) -> Option<BoxFuture<pi_js::Result<()>>> {
        let token = self.token.clone();
        let url = self.url.clone();
        Some(Box::pin(async move {
            let resp = (context.fetch)(&url, None).await?;
            resp.cancel_body();
            *token.lock().unwrap() = Some("token".to_string());
            Ok(())
        }))
    }
}

fn build_request(url: &str, init: Option<McpFetchInit>) -> Request {
    let method = init
        .as_ref()
        .and_then(|i| i.method.clone())
        .unwrap_or_else(|| "GET".to_string());
    let mut req = Request::new(method, url);
    if let Some(init) = init {
        if let Some(headers) = init.headers {
            req = req.with_headers(headers);
        }
        if let Some(body) = init.body {
            req = req.body(body);
        }
        if let Some(signal) = init.signal {
            req = req.signal(signal);
        }
    }
    req
}

async fn forward_with_reqwest(req: Request) -> pi_js::Result<Response> {
    let client = reqwest::Client::new();
    let method = reqwest::Method::from_bytes(req.method.as_bytes()).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    let mut builder = client.request(method, &req.url);
    for (k, v) in req.headers.entries() {
        builder = builder.header(k.as_str(), v.as_str());
    }
    if let Some(body) = req.body {
        builder = builder.body(body.as_bytes().to_vec());
    }
    let resp = builder.send().await.map_err(|e| pi_js::Error::msg(e.to_string()))?;
    let status = resp.status().as_u16();
    let mut headers = Headers::new();
    for (k, v) in resp.headers() {
        if let Ok(s) = v.to_str() {
            headers.append(k.as_str(), s);
        }
    }
    let bytes = resp.bytes().await.map_err(|e| pi_js::Error::msg(e.to_string()))?;
    Ok(Response::new(status, headers, bytes))
}

#[tokio::test]
async fn uses_custom_fetch_and_global_fetch_consistently_including_in_auth_context() {
    let mut servers = TestServers::new();
    let (url, _) = start_server(&mut servers, |req, reqs| {
        Box::pin(async move {
            if req.method == "POST" && req.header("authorization").is_none() {
                return TestResponse::with_headers(401, &[("www-authenticate", "Bearer")], "");
            }
            protocol_handler(req, reqs, None).await
        })
    })
    .await;

    let run_connect = |url: String, custom_fetch: Option<McpFetch>| async move {
        let client = default_client();
        let auth = Arc::new(StrictFetchAuth {
            token: Arc::new(Mutex::new(None)),
            url: url.clone(),
        });
        client
            .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
                url,
                headers: None,
                fetch: custom_fetch,
                open_get_stream: Some(false),
                max_message_bytes: None,
                auth_provider: Some(auth),
                reconnect: None,
            })))
            .await
            .unwrap();
        let expected_tools: Vec<Tool> =
            from_value(json!([{ "name": "echo", "inputSchema": { "type": "object" } }])).unwrap();
        assert_eq!(client.list_tools(None).await.unwrap(), expected_tools);
        client.close().await.unwrap();
    };

    let explicit_calls = Arc::new(AtomicUsize::new(0));
    let explicit_cb = explicit_calls.clone();
    let custom_fetch: McpFetch = Arc::new(move |url, init| {
        let url = url.to_string();
        let calls = explicit_cb.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            pi_js::fetch::fetch(build_request(&url, init)).await
        })
    });
    run_connect(url.clone(), Some(custom_fetch)).await;
    assert!(explicit_calls.load(Ordering::SeqCst) > 0);

    let global_calls = Arc::new(AtomicUsize::new(0));
    let global_cb = global_calls.clone();
    let _guard = mock_fetch(move |req| {
        let calls = global_cb.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            forward_with_reqwest(req).await
        })
    });
    run_connect(url, None).await;
    assert!(global_calls.load(Ordering::SeqCst) > 0);
    drop(_guard);
    servers.close().await;
}

#[tokio::test]
async fn classifies_an_expired_established_session() {
    let mut servers = TestServers::new();
    let posts = Arc::new(AtomicUsize::new(0));
    let posts_cb = posts.clone();
    let (url, _) = start_server(&mut servers, move |req, reqs| {
        let posts = posts_cb.clone();
        Box::pin(async move {
            if req.method == "POST" && posts.fetch_add(1, Ordering::SeqCst) >= 2 {
                return TestResponse::text(404, "gone");
            }
            protocol_handler(req, reqs, None).await
        })
    })
    .await;

    let client = default_client();
    client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url,
            headers: None,
            fetch: None,
            open_get_stream: Some(false),
            max_message_bytes: None,
            auth_provider: None,
            reconnect: None,
        })))
        .await
        .unwrap();

    let err = client.list_tools(None).await.unwrap_err();
    assert!(
        err.downcast_ref::<McpSessionExpiredError>().is_some(),
        "expected McpSessionExpiredError, got {err}"
    );
    client.close().await.unwrap();
    servers.close().await;
}

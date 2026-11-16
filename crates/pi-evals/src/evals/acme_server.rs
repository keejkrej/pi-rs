//! Port of packages/evals/evals/acme-server.ts

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use pi_js::{Error, Result};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub const OPENAI_PROVIDER_ID: &str = "acme";
pub const OPENAI_MODEL_ID: &str = "acme-chat";
pub const OPENAI_PROBE_PROMPT: &str = "Reply with ACME_OK.";
pub const OPENAI_PROBE_RESPONSE: &str = "ACME_OK";

pub const STREAM_PROVIDER_ID: &str = "acme-stream";
pub const STREAM_MODEL_ID: &str = "acme-stream-chat";
pub const STREAM_PROBE_PROMPT: &str = "Reply with ACME_STREAM_OK.";
pub const STREAM_PROBE_RESPONSE: &str = "ACME_STREAM_OK";

/// Example streaming provider API documentation seeded into custom-provider eval workspaces.
pub static STREAM_API_DOCUMENTATION: LazyLock<String> = LazyLock::new(|| {
    let doc = json!({
        "name": "Acme Streaming API",
        "request": {
            "method": "POST",
            "path": "/generate",
            "headers": {
                "content-type": "application/json",
                "x-acme-key": "resolved credential"
            },
            "body": {
                "model": STREAM_MODEL_ID,
                "messages": [{ "role": "user", "content": "Hello" }],
                "stream": true
            }
        },
        "response": {
            "contentType": "application/x-ndjson",
            "events": [
                { "type": "text_delta", "text": "Hello" },
                { "type": "usage", "input_tokens": 3, "output_tokens": 2 },
                { "type": "done", "reason": "stop" }
            ]
        }
    });
    format!("{}\n", pi_js::json::stringify_pretty(&doc, "  "))
});

/// Endpoint compatibility mode served by [`AcmeServer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcmeServerMode {
    OpenAi,
    Stream,
}

fn plain_response(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    let mut resp = Response::new(Full::new(Bytes::from(message.as_bytes().to_vec())));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    resp
}

async fn handle_request(
    req: Request<Incoming>,
    mode: AcmeServerMode,
    received_valid: Arc<AtomicBool>,
) -> std::result::Result<Response<Full<Bytes>>, Infallible> {
    let expected_path = match mode {
        AcmeServerMode::OpenAi => "/v1/chat/completions",
        AcmeServerMode::Stream => "/generate",
    };
    if req.uri().path() != expected_path {
        return Ok(plain_response(StatusCode::NOT_FOUND, "Unknown endpoint"));
    }
    if req.method() != Method::POST {
        return Ok(plain_response(StatusCode::METHOD_NOT_ALLOWED, "Expected POST"));
    }
    let content_type = req
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        return Ok(plain_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Expected application/json",
        ));
    }

    let headers = req.headers().clone();
    let collected = match req.into_body().collect().await {
        Ok(body) => body.to_bytes(),
        Err(_) => return Ok(plain_response(StatusCode::BAD_REQUEST, "Invalid JSON")),
    };
    let Ok(body_str) = std::str::from_utf8(&collected) else {
        return Ok(plain_response(StatusCode::BAD_REQUEST, "Invalid JSON"));
    };
    let Ok(payload) = pi_js::json::parse(body_str) else {
        return Ok(plain_response(StatusCode::BAD_REQUEST, "Invalid JSON"));
    };
    let Some(obj) = payload.as_object() else {
        return Ok(plain_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Expected a JSON object",
        ));
    };

    let prompt = obj.get("messages").and_then(Value::as_array).and_then(|messages| {
        messages.iter().find_map(|msg| {
            let rec = msg.as_object()?;
            if rec.get("role").and_then(Value::as_str) == Some("user") {
                rec.get("content").and_then(Value::as_str).map(ToString::to_string)
            } else {
                None
            }
        })
    });
    let model = obj.get("model").and_then(Value::as_str);
    let stream = obj.get("stream").and_then(Value::as_bool);

    if mode == AcmeServerMode::Stream {
        let key = headers.get("x-acme-key").and_then(|v| v.to_str().ok());
        if key != Some("resolved-stream-key") {
            return Ok(plain_response(
                StatusCode::UNAUTHORIZED,
                "Invalid Acme Stream credential",
            ));
        }
        if model != Some(STREAM_MODEL_ID) || prompt.is_none() || stream != Some(true) {
            return Ok(plain_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid Acme Stream request",
            ));
        }
        received_valid.store(prompt.as_deref() == Some(STREAM_PROBE_PROMPT), Ordering::SeqCst);
        let ndjson = format!(
            "{}\n{}\n{}\n{}\n",
            pi_js::json::stringify(&json!({ "type": "text_delta", "text": "ACME_" })),
            pi_js::json::stringify(&json!({ "type": "text_delta", "text": "STREAM_OK" })),
            pi_js::json::stringify(&json!({ "type": "usage", "input_tokens": 4, "output_tokens": 3 })),
            pi_js::json::stringify(&json!({ "type": "done", "reason": "stop" })),
        );
        let mut resp = Response::new(Full::new(Bytes::from(ndjson)));
        *resp.status_mut() = StatusCode::OK;
        resp.headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/x-ndjson"));
        return Ok(resp);
    }

    let auth = headers.get("authorization").and_then(|v| v.to_str().ok());
    if auth != Some("Bearer resolved-acme-key") {
        return Ok(plain_response(StatusCode::UNAUTHORIZED, "Invalid Acme credential"));
    }
    if model != Some(OPENAI_MODEL_ID) || prompt.is_none() || stream != Some(true) {
        return Ok(plain_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid OpenAI-compatible request",
        ));
    }
    received_valid.store(prompt.as_deref() == Some(OPENAI_PROBE_PROMPT), Ordering::SeqCst);
    let chunk1 = pi_js::json::stringify(&json!({
        "id": "chatcmpl-acme",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": OPENAI_MODEL_ID,
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": OPENAI_PROBE_RESPONSE },
            "finish_reason": null
        }]
    }));
    let chunk2 = pi_js::json::stringify(&json!({
        "id": "chatcmpl-acme",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": OPENAI_MODEL_ID,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 3, "completion_tokens": 2 }
    }));
    let sse = format!("data: {chunk1}\n\ndata: {chunk2}\n\ndata: [DONE]\n\n");
    let mut resp = Response::new(Full::new(Bytes::from(sse)));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    resp.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(resp)
}

struct RunningServer {
    origin: String,
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

/// Local deterministic fixture server implementing Acme OpenAI-compatible or NDJSON stream endpoints.
#[derive(Clone)]
pub struct AcmeServer {
    mode: AcmeServerMode,
    received_valid: Arc<AtomicBool>,
    running: Arc<Mutex<Option<RunningServer>>>,
}

impl AcmeServer {
    /// Starts listening on `127.0.0.1:0` and records the bound local origin URL.
    pub async fn start(&self) -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|err| Error::from_io(err, "listen", None, None))?;
        let addr = listener
            .local_addr()
            .map_err(|err| Error::from_io(err, "getsockname", None, None))?;
        let origin = format!("http://127.0.0.1:{}", addr.port());
        let cancel = CancellationToken::new();
        let loop_cancel = cancel.clone();
        let mode = self.mode;
        let received_valid = Arc::clone(&self.received_valid);

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = loop_cancel.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _peer)) = accepted else {
                            break;
                        };
                        let io = TokioIo::new(stream);
                        let conn_received = Arc::clone(&received_valid);
                        let conn_cancel = loop_cancel.clone();
                        tokio::spawn(async move {
                            let svc = service_fn(move |req| {
                                handle_request(req, mode, Arc::clone(&conn_received))
                            });
                            let conn = http1::Builder::new().serve_connection(io, svc);
                            tokio::select! {
                                _ = conn => {}
                                _ = conn_cancel.cancelled() => {}
                            }
                        });
                    }
                }
            }
        });

        let mut guard = self.running.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(RunningServer { origin, cancel, handle });
        Ok(())
    }

    /// Stops the local fixture server and clears its running state.
    pub async fn stop(&self) -> Result<()> {
        let running = {
            let mut guard = self.running.lock().unwrap_or_else(|e| e.into_inner());
            guard.take()
        };
        if let Some(srv) = running {
            srv.cancel.cancel();
            let _ = srv.handle.await;
        }
        Ok(())
    }

    /// Clears the probe validation latch between cases.
    pub fn reset(&self) {
        self.received_valid.store(false, Ordering::SeqCst);
    }

    /// Returns the server's `http://127.0.0.1:<port>` origin.
    pub fn origin(&self) -> String {
        let guard = self.running.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().map(|srv| srv.origin.clone()).unwrap_or_default()
    }

    /// Returns `${origin}/v1` for OpenAI-compatible client configuration.
    pub fn base_url(&self) -> String {
        format!("{}/v1", self.origin())
    }

    /// Returns `true` when the expected probe prompt arrived with valid auth and model parameters.
    pub fn valid_request_received(&self) -> bool {
        self.received_valid.load(Ordering::SeqCst)
    }
}

/// Constructs an unstarted [`AcmeServer`] in the requested protocol mode.
pub fn create_acme_server(mode: AcmeServerMode) -> AcmeServer {
    AcmeServer {
        mode,
        received_valid: Arc::new(AtomicBool::new(false)),
        running: Arc::new(Mutex::new(None)),
    }
}

//! Port of packages/coding-agent/test/suite/mcp-oauth-server.ts

#![allow(dead_code)]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use indexmap::{IndexMap, IndexSet};
use pi_mcp::LATEST_PROTOCOL_VERSION;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Clone, Debug, Default)]
pub struct StartOAuthMcpServerOptions {
    pub iss: Option<String>,
}

struct ServerState {
    log: Vec<String>,
    registrations: Vec<Value>,
    valid_tokens: IndexSet<String>,
    refresh_tokens: IndexSet<String>,
    challenges: IndexMap<String, String>,
    issued: usize,
    origin: String,
    iss: Option<String>,
}

impl ServerState {
    fn issue_tokens(&mut self) -> Value {
        self.issued += 1;
        let access_token = format!("access-{}", self.issued);
        let refresh_token = format!("refresh-{}", self.issued);
        self.valid_tokens.insert(access_token.clone());
        self.refresh_tokens.insert(refresh_token.clone());
        json!({
            "access_token": access_token,
            "refresh_token": refresh_token,
            "token_type": "Bearer",
            "expires_in": 3600,
        })
    }
}

pub struct OAuthMcpServerHandle {
    pub url: String,
    pub origin: String,
    state: Arc<Mutex<ServerState>>,
    shutdown: Mutex<Option<watch::Sender<bool>>>,
    accept_handle: Mutex<Option<JoinHandle<()>>>,
    conn_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl OAuthMcpServerHandle {
    pub fn log(&self) -> Vec<String> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).log.clone()
    }

    pub fn registrations(&self) -> Vec<Value> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .registrations
            .clone()
    }

    pub fn expire_access_tokens(&self) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .valid_tokens
            .clear();
    }

    pub async fn close(&self) {
        if let Some(tx) = self.shutdown.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(true);
        }
        let accept = self.accept_handle.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(handle) = accept {
            let _ = handle.await;
        }
        let conns: Vec<JoinHandle<()>> =
            std::mem::take(&mut *self.conn_handles.lock().unwrap_or_else(|e| e.into_inner()));
        for handle in conns {
            handle.abort();
            let _ = handle.await;
        }
    }
}

fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    let mut resp = Response::new(Full::new(Bytes::new()));
    *resp.status_mut() = status;
    resp
}

fn json_response(status: StatusCode, body: &Value) -> Response<Full<Bytes>> {
    let text = pi_js::json::stringify(body);
    let mut resp = Response::new(Full::new(Bytes::from(text)));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    resp
}

fn handle_mcp(
    method: &Method,
    auth_header: Option<&str>,
    body_bytes: &[u8],
    state: &Arc<Mutex<ServerState>>,
) -> Response<Full<Bytes>> {
    if *method != Method::POST {
        return empty_response(if *method == Method::GET {
            StatusCode::METHOD_NOT_ALLOWED
        } else {
            StatusCode::OK
        });
    }

    let token = auth_header.and_then(|v| v.strip_prefix("Bearer "));
    {
        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
        let valid = token.is_some_and(|t| st.valid_tokens.contains(t));
        if !valid {
            st.log.push(format!("401 {}", token.unwrap_or("none")));
            let origin = st.origin.clone();
            let mut resp = empty_response(StatusCode::UNAUTHORIZED);
            if let Ok(hdr) = hyper::header::HeaderValue::from_str(&format!(
                "Bearer resource_metadata=\"{origin}/.well-known/oauth-protected-resource/mcp\""
            )) {
                resp.headers_mut().insert(hyper::header::WWW_AUTHENTICATE, hdr);
            }
            return resp;
        }
    }

    let body_text = String::from_utf8_lossy(body_bytes);
    let Ok(message) = pi_js::json::parse(&body_text) else {
        return empty_response(StatusCode::BAD_REQUEST);
    };
    let Some(msg_obj) = message.as_object() else {
        return empty_response(StatusCode::BAD_REQUEST);
    };
    let Some(id_val) = msg_obj.get("id").cloned() else {
        return empty_response(StatusCode::ACCEPTED);
    };
    let rpc_method = msg_obj.get("method").and_then(Value::as_str).unwrap_or("");

    let result = match rpc_method {
        "initialize" => json!({
            "protocolVersion": LATEST_PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "issues", "version": "1.0.0" },
        }),
        "tools/list" => json!({
            "tools": [{
                "name": "whoami",
                "inputSchema": { "type": "object", "properties": {} },
            }]
        }),
        "tools/call" => {
            let tok = token.unwrap_or("");
            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
            st.log.push(format!("call {tok}"));
            json!({
                "content": [{ "type": "text", "text": format!("token {tok}") }],
            })
        }
        _ => json!({}),
    };
    json_response(
        StatusCode::OK,
        &json!({
            "jsonrpc": "2.0",
            "id": id_val,
            "result": result,
        }),
    )
}

async fn handle_request(req: Request<Incoming>, state: Arc<Mutex<ServerState>>) -> Response<Full<Bytes>> {
    let method = req.method().clone();
    let uri_str = req.uri().to_string();
    let auth_header = req
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body_bytes = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => Bytes::new(),
    };

    let origin = state.lock().unwrap_or_else(|e| e.into_inner()).origin.clone();
    let full_url = format!("{origin}{uri_str}");
    let Ok(parsed_url) = url::Url::parse(&full_url) else {
        return empty_response(StatusCode::BAD_REQUEST);
    };

    match parsed_url.path() {
        "/mcp" => handle_mcp(&method, auth_header.as_deref(), &body_bytes, &state),
        "/.well-known/oauth-protected-resource/mcp" => json_response(
            StatusCode::OK,
            &json!({
                "resource": format!("{origin}/mcp"),
                "authorization_servers": [origin],
            }),
        ),
        "/.well-known/oauth-authorization-server" => json_response(
            StatusCode::OK,
            &json!({
                "issuer": origin,
                "authorization_endpoint": format!("{origin}/authorize"),
                "token_endpoint": format!("{origin}/token"),
                "registration_endpoint": format!("{origin}/register"),
                "response_types_supported": ["code"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
            }),
        ),
        "/register" => {
            let body_text = String::from_utf8_lossy(&body_bytes);
            let parsed = pi_js::json::parse(&body_text).unwrap_or_else(|_| Value::Object(Map::new()));
            let mut out = parsed.as_object().cloned().unwrap_or_default();
            {
                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                st.log.push("register".to_string());
                st.registrations.push(Value::Object(out.clone()));
            }
            out.insert("client_id".to_string(), Value::String("client-1".to_string()));
            json_response(StatusCode::CREATED, &Value::Object(out))
        }
        "/authorize" => {
            let q = |key: &str| {
                parsed_url
                    .query_pairs()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.into_owned())
            };
            let code_challenge = q("code_challenge").unwrap_or_default();
            let redirect_raw = q("redirect_uri").unwrap_or_default();
            let state_param = q("state").unwrap_or_default();

            let (code, iss) = {
                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                let code = format!("code-{}", st.challenges.len() + 1);
                st.challenges.insert(code.clone(), code_challenge);
                (code, st.iss.clone())
            };

            let Ok(mut redirect) = url::Url::parse(&redirect_raw) else {
                return empty_response(StatusCode::BAD_REQUEST);
            };
            redirect
                .query_pairs_mut()
                .append_pair("code", &code)
                .append_pair("state", &state_param);
            if let Some(ref iss_val) = iss {
                redirect.query_pairs_mut().append_pair("iss", iss_val);
            }
            let mut resp = empty_response(StatusCode::FOUND);
            if let Ok(loc) = hyper::header::HeaderValue::from_str(redirect.as_str()) {
                resp.headers_mut().insert(hyper::header::LOCATION, loc);
            }
            resp
        }
        "/token" => {
            let params: IndexMap<String, String> = url::form_urlencoded::parse(&body_bytes)
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let grant_type = params.get("grant_type").map(String::as_str).unwrap_or("");
            if grant_type == "authorization_code" {
                let code = params.get("code").map(String::as_str).unwrap_or("");
                let verifier = params.get("code_verifier").map(String::as_str).unwrap_or("");
                let computed =
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                let challenge = st.challenges.get(code).cloned();
                if challenge.as_deref() != Some(computed.as_str()) {
                    return json_response(StatusCode::BAD_REQUEST, &json!({ "error": "invalid_grant" }));
                }
                st.challenges.shift_remove(code);
                st.log.push("token code".to_string());
                let tokens = st.issue_tokens();
                return json_response(StatusCode::OK, &tokens);
            }
            let refresh = params.get("refresh_token").map(String::as_str).unwrap_or("");
            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
            if !st.refresh_tokens.shift_remove(refresh) {
                return json_response(StatusCode::BAD_REQUEST, &json!({ "error": "invalid_grant" }));
            }
            st.log.push("token refresh".to_string());
            let tokens = st.issue_tokens();
            json_response(StatusCode::OK, &tokens)
        }
        _ => empty_response(StatusCode::NOT_FOUND),
    }
}

pub async fn start_oauth_mcp_server(options: StartOAuthMcpServerOptions) -> std::io::Result<OAuthMcpServerHandle> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let origin = format!("http://127.0.0.1:{}", addr.port());
    let url = format!("{origin}/mcp");

    let state = Arc::new(Mutex::new(ServerState {
        log: Vec::new(),
        registrations: Vec::new(),
        valid_tokens: IndexSet::new(),
        refresh_tokens: IndexSet::new(),
        challenges: IndexMap::new(),
        issued: 0,
        origin: origin.clone(),
        iss: options.iss,
    }));

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let conn_handles: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));

    let state_accept = Arc::clone(&state);
    let conns_accept = Arc::clone(&conn_handles);
    let accept_handle = tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                biased;
                _ = shutdown_rx.changed() => break,
                res = listener.accept() => res,
            };
            let Ok((stream, _)) = accepted else {
                break;
            };
            let st = Arc::clone(&state_accept);
            let mut conn_shutdown = shutdown_rx.clone();
            let handle = tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(move |req: Request<Incoming>| {
                    let st = Arc::clone(&st);
                    async move { Ok::<_, Infallible>(handle_request(req, st).await) }
                });
                let conn = http1::Builder::new().serve_connection(io, service);
                tokio::select! {
                    _ = conn => {}
                    _ = conn_shutdown.changed() => {}
                }
            });
            conns_accept.lock().unwrap_or_else(|e| e.into_inner()).push(handle);
        }
    });

    Ok(OAuthMcpServerHandle {
        url,
        origin,
        state,
        shutdown: Mutex::new(Some(shutdown_tx)),
        accept_handle: Mutex::new(Some(accept_handle)),
        conn_handles,
    })
}

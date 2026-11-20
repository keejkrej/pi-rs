#![allow(dead_code, unused_imports)]

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use pi_js::{BoxFuture, Error, Result};
use pi_mcp::oauth::{McpOAuthServerStore, McpOAuthState, McpOAuthStateStore, MemoryOAuthStateStore};
use serde_json::{Value, json};
use url::Url;

use crate::support::helpers::{TestResponse, TestServers};

#[derive(Clone, Default)]
pub struct StartAuthServerOptions {
    pub cimd: bool,
    pub iss_parameter: bool,
}

type RefreshHook = Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>;

pub struct AuthorizationServer {
    pub server_url: String,
    origin: String,
    registrations: Arc<Mutex<Vec<Value>>>,
    authorizations: Arc<Mutex<Vec<IndexMap<String, String>>>>,
    token_requests: Arc<Mutex<Vec<IndexMap<String, String>>>>,
    stall: Arc<Mutex<IndexSet<String>>>,
    refresh_tokens: Arc<Mutex<IndexSet<String>>>,
    refresh_failure: Arc<Mutex<Option<(String, Option<RefreshHook>)>>>,
    servers: TestServers,
}

impl AuthorizationServer {
    pub async fn start(options: StartAuthServerOptions) -> Self {
        let mut servers = TestServers::new();
        let registrations = Arc::new(Mutex::new(Vec::<Value>::new()));
        let authorizations = Arc::new(Mutex::new(Vec::<IndexMap<String, String>>::new()));
        let token_requests = Arc::new(Mutex::new(Vec::<IndexMap<String, String>>::new()));
        let challenges = Arc::new(Mutex::new(IndexMap::<String, String>::new()));
        let stall = Arc::new(Mutex::new(IndexSet::<String>::new()));
        let refresh_tokens = Arc::new(Mutex::new(IndexSet::<String>::new()));
        let refresh_failure: Arc<Mutex<Option<(String, Option<RefreshHook>)>>> = Arc::new(Mutex::new(None));
        let issued = Arc::new(AtomicUsize::new(0));

        let regs_h = registrations.clone();
        let auths_h = authorizations.clone();
        let toks_h = token_requests.clone();
        let chal_h = challenges.clone();
        let stall_h = stall.clone();
        let rtok_h = refresh_tokens.clone();
        let rfail_h = refresh_failure.clone();
        let issued_h = issued.clone();

        let origin = servers
            .listen(move |req, origin| {
                let regs = regs_h.clone();
                let auths = auths_h.clone();
                let toks = toks_h.clone();
                let chals = chal_h.clone();
                let stall = stall_h.clone();
                let rtoks = rtok_h.clone();
                let rfail = rfail_h.clone();
                let issued = issued_h.clone();
                let cimd = options.cimd;
                let iss_param = options.iss_parameter;

                Box::pin(async move {
                    let full = format!("{origin}{}", req.uri);
                    let url = Url::parse(&full).expect("valid request url");
                    let path = url.path().to_string();
                    if stall.lock().unwrap().contains(&path) {
                        std::future::pending::<()>().await;
                        unreachable!();
                    }

                    let issue_tokens = || {
                        let n = issued.fetch_add(1, Ordering::SeqCst) + 1;
                        let refresh = format!("refresh-{n}");
                        rtoks.lock().unwrap().insert(refresh.clone());
                        json!({
                            "access_token": format!("access-{n}"),
                            "refresh_token": refresh,
                            "token_type": "Bearer"
                        })
                    };

                    match path.as_str() {
                        "/.well-known/oauth-protected-resource/mcp" => TestResponse::json(
                            200,
                            &json!({
                                "resource": format!("{origin}/mcp"),
                                "authorization_servers": [origin],
                            }),
                        ),
                        "/.well-known/oauth-authorization-server" => {
                            let mut meta = json!({
                                "issuer": origin,
                                "authorization_endpoint": format!("{origin}/authorize"),
                                "token_endpoint": format!("{origin}/token"),
                                "registration_endpoint": format!("{origin}/register"),
                                "response_types_supported": ["code"],
                                "code_challenge_methods_supported": ["S256"],
                                "token_endpoint_auth_methods_supported": ["none"],
                            });
                            if cimd {
                                meta["client_id_metadata_document_supported"] = Value::Bool(true);
                            }
                            if iss_param {
                                meta["authorization_response_iss_parameter_supported"] = Value::Bool(true);
                            }
                            TestResponse::json(200, &meta)
                        }
                        "/register" => {
                            let metadata = req.body_json().unwrap_or(Value::Null);
                            let mut resp = metadata.clone();
                            let count = {
                                let mut guard = regs.lock().unwrap();
                                guard.push(metadata);
                                guard.len()
                            };
                            if let Value::Object(ref mut obj) = resp {
                                obj.insert("client_id".to_string(), Value::String(format!("client-{count}")));
                            }
                            TestResponse::json(201, &resp)
                        }
                        "/authorize" => {
                            let mut query = IndexMap::new();
                            for (k, v) in url.query_pairs() {
                                query.insert(k.into_owned(), v.into_owned());
                            }
                            let count = {
                                let mut guard = auths.lock().unwrap();
                                guard.push(query.clone());
                                guard.len()
                            };
                            let code = format!("code-{count}");
                            chals
                                .lock()
                                .unwrap()
                                .insert(code.clone(), query.get("code_challenge").cloned().unwrap_or_default());
                            let redirect_raw = query.get("redirect_uri").cloned().unwrap_or_default();
                            let mut redirect = Url::parse(&redirect_raw).expect("valid redirect_uri");
                            {
                                let mut pairs = redirect.query_pairs_mut();
                                pairs.append_pair("code", &code);
                                pairs.append_pair("state", query.get("state").map(String::as_str).unwrap_or(""));
                                if iss_param {
                                    pairs.append_pair("iss", &origin);
                                }
                            }
                            TestResponse::redirect(302, redirect.as_str())
                        }
                        "/token" => {
                            let mut params = IndexMap::new();
                            for (k, v) in form_urlencoded::parse(req.body_text().as_bytes()) {
                                params.insert(k.into_owned(), v.into_owned());
                            }
                            toks.lock().unwrap().push(params.clone());
                            if params.get("grant_type").map(String::as_str) == Some("authorization_code") {
                                let code = params.get("code").cloned().unwrap_or_default();
                                let verifier = params.get("code_verifier").cloned().unwrap_or_default();
                                let challenge = pi_js::b64::encode_url(&pi_js::crypto::sha256(verifier.as_bytes()));
                                let expected = chals.lock().unwrap().get(&code).cloned();
                                if expected.as_deref() != Some(challenge.as_str()) {
                                    return TestResponse::json(400, &json!({ "error": "invalid_grant" }));
                                }
                                chals.lock().unwrap().shift_remove(&code);
                                return TestResponse::json(200, &issue_tokens());
                            }

                            let failure = rfail.lock().unwrap().take();
                            if let Some((err_code, hook)) = failure {
                                if let Some(h) = hook {
                                    h().await;
                                }
                                return TestResponse::json(400, &json!({ "error": err_code }));
                            }
                            let refresh_tok = params.get("refresh_token").cloned().unwrap_or_default();
                            if !rtoks.lock().unwrap().shift_remove(&refresh_tok) {
                                return TestResponse::json(400, &json!({ "error": "invalid_grant" }));
                            }
                            TestResponse::json(200, &issue_tokens())
                        }
                        _ => TestResponse::empty(404),
                    }
                })
            })
            .await;

        Self {
            server_url: format!("{origin}/mcp"),
            origin,
            registrations,
            authorizations,
            token_requests,
            stall,
            refresh_tokens,
            refresh_failure,
            servers,
        }
    }

    pub fn registrations(&self) -> Vec<Value> {
        self.registrations.lock().unwrap().clone()
    }

    pub fn authorizations(&self) -> Vec<IndexMap<String, String>> {
        self.authorizations.lock().unwrap().clone()
    }

    pub fn token_requests(&self) -> Vec<IndexMap<String, String>> {
        self.token_requests.lock().unwrap().clone()
    }

    pub fn add_stall(&self, path: &str) {
        self.stall.lock().unwrap().insert(path.to_string());
    }

    pub fn revoke_refresh_tokens(&self) {
        self.refresh_tokens.lock().unwrap().clear();
    }

    pub fn fail_next_refresh(&self, error: &str) {
        *self.refresh_failure.lock().unwrap() = Some((error.to_string(), None));
    }

    pub fn fail_next_refresh_with_hook<F, Fut>(&self, error: &str, on_request: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let hook: RefreshHook = Arc::new(move || Box::pin(on_request()));
        *self.refresh_failure.lock().unwrap() = Some((error.to_string(), Some(hook)));
    }

    pub async fn close(&mut self) {
        self.servers.close().await;
    }
}

#[derive(Clone, Default)]
pub struct MemoryServerStore {
    inner: MemoryOAuthStateStore,
}

impl MemoryServerStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl McpOAuthStateStore for MemoryServerStore {
    async fn load(&self) -> Result<Option<McpOAuthState>> {
        McpOAuthStateStore::load(&self.inner).await
    }

    async fn save(&self, state: McpOAuthState) -> Result<()> {
        McpOAuthStateStore::save(&self.inner, state).await
    }
}

#[async_trait::async_trait]
impl McpOAuthServerStore for MemoryServerStore {
    async fn with_refresh_lock_boxed(
        &self,
        work: Box<dyn FnOnce() -> BoxFuture<Result<Box<dyn std::any::Any + Send>>> + Send>,
    ) -> Result<Box<dyn std::any::Any + Send>> {
        work().await
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedSave {
    pub locked: bool,
    pub access_token: Option<String>,
}

type LockErrorFactory = Arc<dyn Fn() -> Error + Send + Sync>;

#[derive(Clone)]
pub struct RecordingStore {
    memory: MemoryOAuthStateStore,
    saves: Arc<Mutex<Vec<RecordedSave>>>,
    lock_runs: Arc<Mutex<Vec<tokio::sync::oneshot::Receiver<()>>>>,
    locked: Arc<AtomicBool>,
    held: Arc<Mutex<Option<tokio::sync::watch::Receiver<bool>>>>,
    lock_error: Option<LockErrorFactory>,
}

impl RecordingStore {
    pub fn new(lock_error: Option<LockErrorFactory>) -> Self {
        Self {
            memory: MemoryOAuthStateStore::new(),
            saves: Arc::new(Mutex::new(Vec::new())),
            lock_runs: Arc::new(Mutex::new(Vec::new())),
            locked: Arc::new(AtomicBool::new(false)),
            held: Arc::new(Mutex::new(None)),
            lock_error,
        }
    }

    pub fn saves(&self) -> Vec<RecordedSave> {
        self.saves.lock().unwrap().clone()
    }

    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    pub fn hold(&self) -> impl FnOnce() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        *self.held.lock().unwrap() = Some(rx);
        move || {
            let _ = tx.send(true);
        }
    }

    pub async fn wait_lock_runs(&self) {
        let rxs = std::mem::take(&mut *self.lock_runs.lock().unwrap());
        for rx in rxs {
            let _ = rx.await;
        }
    }
}

#[async_trait::async_trait]
impl McpOAuthStateStore for RecordingStore {
    async fn load(&self) -> Result<Option<McpOAuthState>> {
        McpOAuthStateStore::load(&self.memory).await
    }

    async fn save(&self, state: McpOAuthState) -> Result<()> {
        let locked = self.locked.load(Ordering::SeqCst);
        let access_token = state.tokens.as_ref().map(|t| t.access_token.clone());
        self.saves.lock().unwrap().push(RecordedSave { locked, access_token });
        McpOAuthStateStore::save(&self.memory, state).await
    }
}

#[async_trait::async_trait]
impl McpOAuthServerStore for RecordingStore {
    async fn with_refresh_lock_boxed(
        &self,
        work: Box<dyn FnOnce() -> BoxFuture<Result<Box<dyn std::any::Any + Send>>> + Send>,
    ) -> Result<Box<dyn std::any::Any + Send>> {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        self.lock_runs.lock().unwrap().push(done_rx);
        struct NotifyDone(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for NotifyDone {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.send(());
                }
            }
        }
        let _done_guard = NotifyDone(Some(done_tx));

        let held_rx = self.held.lock().unwrap().clone();
        if let Some(mut rx) = held_rx {
            while !*rx.borrow() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        }
        if let Some(ref factory) = self.lock_error {
            return Err(factory());
        }

        struct LockReset(Arc<AtomicBool>);
        impl Drop for LockReset {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        self.locked.store(true, Ordering::SeqCst);
        let _reset = LockReset(self.locked.clone());
        work().await
    }
}

pub async fn authorize_in_browser(authorization_url: &str) -> String {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest client");
    let resp = client.get(authorization_url).send().await.expect("authorize request");
    resp.headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

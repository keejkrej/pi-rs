//! Port of packages/coding-agent/test/llama-extension.test.ts (+ b3c2e4a6b, ea0f49a36)

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{HeaderMap, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use indexmap::IndexMap;
use pi_ai::auth::types::{
    ApiKeyAuthInput, ApiKeyCredential, AuthContext, AuthEvent, AuthInteraction, AuthPrompt, Credential,
    ProviderAuthInteraction,
};
use pi_ai::models::RefreshModelsContext;
use pi_ai::models_store::ModelsStoreEntry;
use pi_ai::types::{
    AnyModel, ClassifierAnswer, ClassifierContext, ClassifierOptions, ClassifierQuestion, ModelThinkingLevel,
    ProviderRequestOptions, ThinkingFormat,
};
use pi_coding_agent::extensions::llama::client::{
    LlamaClient, LlamaModelArchitecture, LlamaModelInfo, LlamaModelMeta, LlamaModelStatus, LlamaModelStatusInfo,
    LlamaProgress, normalize_llama_server_url,
};
use pi_coding_agent::extensions::llama::create_llama_extension;
use pi_coding_agent::extensions::llama::huggingface::{
    HuggingFaceClient, HuggingFaceGated, HuggingFaceModel, HuggingFaceModelDetails, HuggingFaceQuantization,
    find_hugging_face_token,
};
use pi_coding_agent::extensions::llama::provider::{
    DEFAULT_LLAMA_SERVER_URL, LLAMA_PROVIDER_ID, SetCatalogOptions, create_llama_provider,
};
use pi_js::BoxFuture;
use pi_js::abort::AbortController;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

struct StreamBody {
    rx: mpsc::UnboundedReceiver<Bytes>,
}

impl Body for StreamBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.rx).poll_recv(cx) {
            Poll::Ready(Some(chunk)) => Poll::Ready(Some(Ok(Frame::data(chunk)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

struct CapturedRequest {
    method: String,
    url: String,
    headers: HeaderMap,
    body: String,
}

type HandlerFn = Arc<dyn Fn(CapturedRequest) -> BoxFuture<Response<BoxBody<Bytes, Infallible>>> + Send + Sync>;

struct TestServer {
    url: String,
    shutdown: Option<watch::Sender<bool>>,
    accept_handle: Option<JoinHandle<()>>,
    conn_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl TestServer {
    async fn start<F>(handler: F) -> Self
    where
        F: Fn(CapturedRequest) -> BoxFuture<Response<BoxBody<Bytes, Infallible>>> + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr: SocketAddr = listener.local_addr().expect("addr");
        let url = format!("http://127.0.0.1:{}", addr.port());
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let conn_handles: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let handler: HandlerFn = Arc::new(handler);
        let conns_clone = Arc::clone(&conn_handles);

        let accept_handle = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    biased;
                    _ = shutdown_rx.changed() => break,
                    res = listener.accept() => res,
                };
                let Ok((stream, _)) = accepted else { break };
                let handler = Arc::clone(&handler);
                let mut conn_shutdown = shutdown_rx.clone();
                let h = tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req: Request<Incoming>| {
                        let handler = Arc::clone(&handler);
                        async move {
                            let method = req.method().as_str().to_string();
                            let url = req.uri().to_string();
                            let headers = req.headers().clone();
                            let body_bytes = req
                                .into_body()
                                .collect()
                                .await
                                .map(|b| b.to_bytes())
                                .unwrap_or_default();
                            let body = String::from_utf8_lossy(&body_bytes).into_owned();
                            let resp = handler(CapturedRequest {
                                method,
                                url,
                                headers,
                                body,
                            })
                            .await;
                            Ok::<_, Infallible>(resp)
                        }
                    });
                    let conn = http1::Builder::new().serve_connection(io, service);
                    tokio::select! {
                        _ = conn => {}
                        _ = conn_shutdown.changed() => {}
                    }
                });
                conns_clone.lock().unwrap_or_else(|e| e.into_inner()).push(h);
            }
        });

        Self {
            url,
            shutdown: Some(shutdown_tx),
            accept_handle: Some(accept_handle),
            conn_handles,
        }
    }

    async fn close(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(true);
        }
        if let Some(h) = self.accept_handle.take() {
            let _ = h.await;
        }
        let handles = std::mem::take(&mut *self.conn_handles.lock().unwrap_or_else(|e| e.into_inner()));
        for h in handles {
            h.abort();
            let _ = h.await;
        }
    }
}

fn json_ok(val: Value) -> Response<BoxBody<Bytes, Infallible>> {
    let text = pi_js::json::stringify(&val);
    let mut resp = Response::new(Full::new(Bytes::from(text)).boxed());
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    resp
}

fn empty_404() -> Response<BoxBody<Bytes, Infallible>> {
    let mut resp = Response::new(Full::new(Bytes::new()).boxed());
    *resp.status_mut() = StatusCode::NOT_FOUND;
    resp
}

fn sse_ok(rx: mpsc::UnboundedReceiver<Bytes>) -> Response<BoxBody<Bytes, Infallible>> {
    let mut resp = Response::new(StreamBody { rx }.boxed());
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("text/event-stream"),
    );
    resp
}

struct EmptyAuthContext;

#[async_trait]
impl AuthContext for EmptyAuthContext {
    async fn env(&self, _name: &str) -> Option<String> {
        None
    }

    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

struct ScriptedInteraction {
    answers: Mutex<VecDeque<String>>,
}

#[async_trait]
impl AuthInteraction for ScriptedInteraction {
    async fn prompt(&self, _prompt: AuthPrompt) -> pi_js::Result<String> {
        self.answers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| pi_js::Error::msg("no more answers"))
    }

    fn notify(&self, _event: AuthEvent) {}
}

#[tokio::test]
async fn exposes_default_url_and_builds_extension_factory() {
    assert_eq!(LLAMA_PROVIDER_ID, "llama.cpp");
    assert_eq!(DEFAULT_LLAMA_SERVER_URL, "http://127.0.0.1:9931");

    let dir = tempfile::tempdir().expect("tempdir");
    let rt = pi_coding_agent::core::extensions::create_extension_runtime();
    let bus = pi_coding_agent::core::event_bus::create_event_bus();
    let ext = pi_coding_agent::core::extensions::load_extension_from_factory(
        create_llama_extension(),
        &dir.path().to_string_lossy(),
        bus,
        rt.clone(),
        Some("builtin:llama.cpp"),
    )
    .await
    .expect("load llama extension");

    assert!(ext.commands.lock().unwrap().contains_key("llama"));
    let native = rt.inner.pending_native_provider_registrations.lock().unwrap();
    assert!(native.iter().any(|p| p.provider.id() == LLAMA_PROVIDER_ID));
}

#[test]
fn normalizes_management_and_inference_urls() {
    assert_eq!(
        normalize_llama_server_url("http://127.0.0.1:9931/v1/").expect("ok"),
        "http://127.0.0.1:9931"
    );
    assert_eq!(
        normalize_llama_server_url("https://example.com/prefix/v1").expect("ok"),
        "https://example.com/prefix"
    );
    let err = normalize_llama_server_url("file:///tmp/llama").expect_err("should fail");
    assert!(err.message().contains("http or https"));
}

// #11745 / ea0f49a36
#[tokio::test]
async fn names_the_server_and_cause_when_it_cannot_be_reached() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let url = format!("http://127.0.0.1:{port}");
    let client = LlamaClient::new(&url, None).expect("client");
    let err = client
        .list(Default::default())
        .await
        .expect_err("should fail unreachable");
    assert!(
        err.message()
            .contains(&format!("Could not reach llama.cpp server at {url}:")),
        "unexpected error: {}",
        err.message()
    );
}

#[test]
fn exposes_loaded_and_sleeping_models_with_router_metadata() {
    let controller = create_llama_provider();
    controller
        .set_catalog(
            &[
                LlamaModelInfo {
                    id: "loaded".to_string(),
                    aliases: None,
                    status: LlamaModelStatusInfo {
                        value: LlamaModelStatus::Loaded,
                        args: Some(vec![
                            "llama-server".to_string(),
                            "--n-gpu-layers".to_string(),
                            "999".to_string(),
                        ]),
                        failed: None,
                        exit_code: None,
                        progress: None,
                    },
                    architecture: Some(LlamaModelArchitecture {
                        input_modalities: Some(vec!["text".to_string(), "image".to_string()]),
                        output_modalities: None,
                    }),
                    source: None,
                    meta: Some(LlamaModelMeta {
                        n_ctx: Some(65536),
                        n_ctx_train: Some(131072),
                        size: None,
                        ftype: None,
                    }),
                },
                LlamaModelInfo {
                    id: "sleeping".to_string(),
                    aliases: None,
                    status: LlamaModelStatusInfo {
                        value: LlamaModelStatus::Sleeping,
                        args: None,
                        failed: None,
                        exit_code: None,
                        progress: None,
                    },
                    architecture: None,
                    source: None,
                    meta: None,
                },
                LlamaModelInfo {
                    id: "unloaded".to_string(),
                    aliases: None,
                    status: LlamaModelStatusInfo {
                        value: LlamaModelStatus::Unloaded,
                        args: None,
                        failed: None,
                        exit_code: None,
                        progress: None,
                    },
                    architecture: None,
                    source: None,
                    meta: None,
                },
                LlamaModelInfo {
                    id: "loading".to_string(),
                    aliases: None,
                    status: LlamaModelStatusInfo {
                        value: LlamaModelStatus::Loading,
                        args: None,
                        failed: None,
                        exit_code: None,
                        progress: None,
                    },
                    architecture: None,
                    source: None,
                    meta: None,
                },
            ],
            "http://localhost:9931",
            SetCatalogOptions::default(),
        )
        .expect("set_catalog");

    let models = controller.provider.get_models();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "loaded");
    assert_eq!(models[0].base_url, "http://localhost:9931/v1");
    assert_eq!(models[0].context_window, 65536);
    assert_eq!(models[0].max_tokens, 65536);
    assert_eq!(models[1].id, "sleeping");
    assert_eq!(models[1].base_url, "http://localhost:9931/v1");
}

// #9528
#[tokio::test]
async fn discovers_chat_template_thinking_support_for_loaded_models() {
    let props_requests = Arc::new(Mutex::new(0usize));
    let props_req = Arc::clone(&props_requests);
    let srv = TestServer::start(move |req| {
        let props_req = Arc::clone(&props_req);
        Box::pin(async move {
            if req.url == "/models" {
                return json_ok(json!({
                    "data": [{ "id": "qwen", "status": { "value": "loaded" }, "meta": { "n_ctx": 32768 } }]
                }));
            }
            if let Ok(parsed) = url::Url::parse(&format!("http://localhost{}", req.url))
                && parsed.path() == "/props"
            {
                *props_req.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                let model_q = parsed
                    .query_pairs()
                    .find(|(k, _)| k == "model")
                    .map(|(_, v)| v.into_owned());
                let auto_q = parsed
                    .query_pairs()
                    .find(|(k, _)| k == "autoload")
                    .map(|(_, v)| v.into_owned());
                assert_eq!(model_q.as_deref(), Some("qwen"));
                assert_eq!(auto_q.as_deref(), Some("false"));
                return json_ok(json!({ "chat_template": "{% if enable_thinking %}think{% endif %}" }));
            }
            empty_404()
        })
    })
    .await;

    let controller = create_llama_provider();
    let cred = Credential::ApiKey(ApiKeyCredential {
        key: Some("local".to_string()),
        env: Some(IndexMap::from([("LLAMA_BASE_URL".to_string(), srv.url.clone())])),
    });
    let ctrl = AbortController::new();
    controller
        .provider
        .refresh_models(RefreshModelsContext {
            credential: Some(cred),
            stored: None,
            publish: Arc::new(|pub_data| {
                Box::pin(async move {
                    if let Some(upd) = pub_data.update {
                        upd();
                    }
                    Ok(true)
                })
            }),
            allow_network: true,
            force: None,
            signal: ctrl.signal(),
        })
        .expect("refresh fut")
        .await
        .expect("refresh ok");

    assert_eq!(*props_requests.lock().unwrap_or_else(|e| e.into_inner()), 1);
    let models = controller.provider.get_models();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "qwen");
    assert!(models[0].reasoning);
    let tmap = models[0].thinking_level_map.as_ref().expect("thinking_level_map");
    assert_eq!(
        tmap.get(&ModelThinkingLevel::Off).and_then(|v| v.as_deref()),
        Some("off")
    );
    assert_eq!(tmap.get(&ModelThinkingLevel::Minimal).and_then(|v| v.as_ref()), None);
    assert_eq!(
        tmap.get(&ModelThinkingLevel::Medium).and_then(|v| v.as_deref()),
        Some("medium")
    );
    assert_eq!(
        models[0]
            .compat
            .as_ref()
            .and_then(|c| c.as_openai_completions())
            .and_then(|c| c.thinking_format),
        Some(ThinkingFormat::QwenChatTemplate)
    );
    srv.close().await;
}

#[tokio::test]
async fn persists_and_restores_selectable_models_for_cache_only_startup_refreshes() {
    let srv = TestServer::start(|req| {
        Box::pin(async move {
            if req.url == "/models" {
                return json_ok(json!({
                    "data": [
                        { "id": "loaded", "status": { "value": "loaded" }, "meta": { "n_ctx": 32768 } },
                        { "id": "sleeping", "status": { "value": "sleeping" }, "meta": { "n_ctx": 32768 } },
                        { "id": "unloaded", "status": { "value": "unloaded" } }
                    ]
                }));
            }
            if req.url == "/props?model=loaded&autoload=false" {
                return json_ok(json!({}));
            }
            empty_404()
        })
    })
    .await;

    let cached: Arc<Mutex<Option<ModelsStoreEntry>>> = Arc::new(Mutex::new(None));
    let publish: pi_ai::models::PublishModels = {
        let cached = Arc::clone(&cached);
        Arc::new(move |p: pi_ai::models::ModelsPublication| {
            let cached = Arc::clone(&cached);
            Box::pin(async move {
                match p.persist {
                    Some(None) => *cached.lock().unwrap_or_else(|e| e.into_inner()) = None,
                    Some(Some(entry)) => {
                        *cached.lock().unwrap_or_else(|e| e.into_inner()) = Some(entry);
                    }
                    None => {}
                }
                if let Some(upd) = p.update {
                    upd();
                }
                Ok(true)
            }) as BoxFuture<pi_js::Result<bool>>
        })
    };

    let cred = Credential::ApiKey(ApiKeyCredential {
        key: Some("local".to_string()),
        env: Some(IndexMap::from([("LLAMA_BASE_URL".to_string(), srv.url.clone())])),
    });
    let first = create_llama_provider();
    first
        .provider
        .refresh_models(RefreshModelsContext {
            credential: Some(cred.clone()),
            stored: None,
            publish: Arc::clone(&publish),
            allow_network: true,
            force: None,
            signal: AbortController::new().signal(),
        })
        .expect("refresh")
        .await
        .expect("ok");

    let ids: Vec<String> = first.provider.get_models().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, vec!["loaded", "sleeping"]);

    let stored = cached.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stored_pairs: Vec<(String, String)> = stored
        .as_ref()
        .expect("stored")
        .models
        .iter()
        .map(|m| match m {
            AnyModel::Chat(c) => (c.id.clone(), c.api.clone()),
            AnyModel::Classifier(c) => (c.id.clone(), c.api.clone()),
            AnyModel::Image(i) => (i.id.clone(), i.api.clone()),
            AnyModel::Unknown(_) => (String::new(), String::new()),
        })
        .collect();
    assert_eq!(
        stored_pairs,
        vec![
            ("loaded".to_string(), "openai-completions".to_string()),
            ("sleeping".to_string(), "openai-completions".to_string()),
            ("loaded".to_string(), "llama-cpp-classify".to_string()),
            ("sleeping".to_string(), "llama-cpp-classify".to_string()),
        ]
    );

    let second = create_llama_provider();
    second
        .provider
        .refresh_models(RefreshModelsContext {
            credential: Some(cred),
            stored,
            publish,
            allow_network: false,
            force: None,
            signal: AbortController::new().signal(),
        })
        .expect("refresh")
        .await
        .expect("ok");

    let restored = second.provider.get_models();
    assert_eq!(restored.len(), 2);
    assert_eq!(restored[0].base_url, format!("{}/v1", srv.url));
    assert_eq!(restored[0].context_window, 32768);
    let classifiers: Vec<_> = second
        .provider
        .get_all_models()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| match m {
            AnyModel::Classifier(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(classifiers.len(), 2);
    assert_eq!(classifiers[0].api, "llama-cpp-classify");
    assert_eq!(classifiers[0].base_url, srv.url);
    srv.close().await;
}

#[tokio::test]
async fn preserves_cached_llama_cpp_context_for_unloaded_autoload_presets() {
    let loaded_flag = Arc::new(Mutex::new(true));
    let unloaded_args: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
    let lf = Arc::clone(&loaded_flag);
    let ua = Arc::clone(&unloaded_args);
    let srv = TestServer::start(move |req| {
        let lf = Arc::clone(&lf);
        let ua = Arc::clone(&ua);
        Box::pin(async move {
            if req.url == "/models" {
                let is_loaded = *lf.lock().unwrap_or_else(|e| e.into_inner());
                let entry = if is_loaded {
                    json!({
                        "id": "qwen",
                        "status": { "value": "loaded" },
                        "source": "preset",
                        "meta": { "n_ctx": 65536, "n_ctx_train": 128000 }
                    })
                } else {
                    let args = ua.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let mut st = json!({ "value": "unloaded" });
                    if let Some(a) = args {
                        st.as_object_mut().expect("obj").insert("args".to_string(), json!(a));
                    }
                    json!({
                        "id": "qwen",
                        "status": st,
                        "source": "preset",
                        "meta": { "n_ctx_train": 128000 }
                    })
                };
                return json_ok(json!({ "data": [entry] }));
            }
            if req.url == "/props?model=qwen&autoload=false" {
                return json_ok(json!({}));
            }
            if req.url == "/props" {
                return json_ok(json!({ "role": "router", "models_autoload": true }));
            }
            empty_404()
        })
    })
    .await;

    let cached: Arc<Mutex<Option<ModelsStoreEntry>>> = Arc::new(Mutex::new(None));
    let publish: pi_ai::models::PublishModels = {
        let cached = Arc::clone(&cached);
        Arc::new(move |p: pi_ai::models::ModelsPublication| {
            let cached = Arc::clone(&cached);
            Box::pin(async move {
                match p.persist {
                    Some(None) => *cached.lock().unwrap_or_else(|e| e.into_inner()) = None,
                    Some(Some(entry)) => {
                        *cached.lock().unwrap_or_else(|e| e.into_inner()) = Some(entry);
                    }
                    None => {}
                }
                if let Some(upd) = p.update {
                    upd();
                }
                Ok(true)
            }) as BoxFuture<pi_js::Result<bool>>
        })
    };
    let cred = Credential::ApiKey(ApiKeyCredential {
        key: Some("local".to_string()),
        env: Some(IndexMap::from([("LLAMA_BASE_URL".to_string(), srv.url.clone())])),
    });
    let ctx_windows = || {
        cached
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|e| {
                e.models
                    .iter()
                    .map(|m| match m {
                        AnyModel::Chat(c) => c.context_window,
                        AnyModel::Classifier(c) => c.context_window,
                        AnyModel::Image(_) | AnyModel::Unknown(_) => 0,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    let first = create_llama_provider();
    first
        .provider
        .refresh_models(RefreshModelsContext {
            credential: Some(cred.clone()),
            stored: None,
            publish: Arc::clone(&publish),
            allow_network: true,
            force: None,
            signal: AbortController::new().signal(),
        })
        .expect("refresh")
        .await
        .expect("ok");
    assert_eq!(ctx_windows(), vec![65536, 65536]);

    *loaded_flag.lock().unwrap_or_else(|e| e.into_inner()) = false;
    let stored = cached.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let second = create_llama_provider();
    second
        .provider
        .refresh_models(RefreshModelsContext {
            credential: Some(cred.clone()),
            stored,
            publish: Arc::clone(&publish),
            allow_network: true,
            force: None,
            signal: AbortController::new().signal(),
        })
        .expect("refresh")
        .await
        .expect("ok");
    assert_eq!(second.provider.get_models()[0].context_window, 65536);
    assert_eq!(ctx_windows(), vec![65536, 65536]);

    *unloaded_args.lock().unwrap_or_else(|e| e.into_inner()) = Some(vec![
        "llama-server".to_string(),
        "--ctx-size".to_string(),
        "32768".to_string(),
    ]);
    let stored2 = cached.lock().unwrap_or_else(|e| e.into_inner()).clone();
    second
        .provider
        .refresh_models(RefreshModelsContext {
            credential: Some(cred),
            stored: stored2,
            publish,
            allow_network: true,
            force: None,
            signal: AbortController::new().signal(),
        })
        .expect("refresh")
        .await
        .expect("ok");
    assert_eq!(ctx_windows(), vec![32768, 32768]);
    srv.close().await;
}

#[tokio::test]
async fn exposes_unloaded_presets_only_when_router_autoload_is_enabled() {
    let props_requests = Arc::new(Mutex::new(0usize));
    let pr = Arc::clone(&props_requests);
    let srv = TestServer::start(move |req| {
        let pr = Arc::clone(&pr);
        Box::pin(async move {
            let auth = req
                .headers
                .get(hyper::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            assert_eq!(auth, Some("Bearer local"));
            if req.url == "/models" {
                return json_ok(json!({
                    "data": [
                        { "id": "preset", "status": { "value": "unloaded" }, "source": "preset", "meta": { "n_ctx": 65536 } },
                        { "id": "failed-preset", "status": { "value": "unloaded", "failed": true }, "source": "preset" },
                        { "id": "cache", "status": { "value": "unloaded" }, "source": "cache" },
                        { "id": "models-dir", "status": { "value": "unloaded" }, "source": "models_dir" }
                    ]
                }));
            }
            if req.url == "/props" {
                *pr.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                return json_ok(json!({ "role": "router", "models_autoload": true }));
            }
            empty_404()
        })
    })
    .await;

    let controller = create_llama_provider();
    let cred = Credential::ApiKey(ApiKeyCredential {
        key: Some("local".to_string()),
        env: Some(IndexMap::from([("LLAMA_BASE_URL".to_string(), srv.url.clone())])),
    });
    controller
        .provider
        .refresh_models(RefreshModelsContext {
            credential: Some(cred),
            stored: None,
            publish: Arc::new(|p| {
                Box::pin(async move {
                    if let Some(u) = p.update {
                        u();
                    }
                    Ok(true)
                })
            }),
            allow_network: true,
            force: None,
            signal: AbortController::new().signal(),
        })
        .expect("refresh")
        .await
        .expect("ok");

    assert_eq!(*props_requests.lock().unwrap_or_else(|e| e.into_inner()), 1);
    let ids: Vec<_> = controller.provider.get_models().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, vec!["preset"]);
    srv.close().await;
}

#[tokio::test]
async fn classifies_with_selectable_models_through_llama_server() {
    let paths: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let p_clone = Arc::clone(&paths);
    let srv = TestServer::start(move |req| {
        let p_clone = Arc::clone(&p_clone);
        Box::pin(async move {
            p_clone.lock().unwrap_or_else(|e| e.into_inner()).push(req.url.clone());
            let payload = pi_js::json::parse(&req.body).expect("json body");
            assert_eq!(payload.get("model").and_then(Value::as_str), Some("qwen"));
            match req.url.as_str() {
                "/tokenize" => {
                    let content = payload.get("content").and_then(Value::as_str).unwrap_or("");
                    let tokens: Vec<u32> = content.chars().map(|c| c as u32).collect();
                    json_ok(json!({ "tokens": tokens }))
                }
                "/apply-template" => json_ok(json!({ "prompt": "<|im_start|>assistant\n" })),
                "/completion" => json_ok(json!({
                    "completion_probabilities": [{
                        "top_logprobs": [
                            { "id": 66, "token": "B", "logprob": -0.1 },
                            { "id": 65, "token": "A", "logprob": -2.4 }
                        ]
                    }]
                })),
                _ => empty_404(),
            }
        })
    })
    .await;

    let controller = create_llama_provider();
    controller
        .set_catalog(
            &[LlamaModelInfo {
                id: "qwen".to_string(),
                aliases: None,
                status: LlamaModelStatusInfo {
                    value: LlamaModelStatus::Loaded,
                    args: None,
                    failed: None,
                    exit_code: None,
                    progress: None,
                },
                architecture: None,
                source: None,
                meta: None,
            }],
            &srv.url,
            SetCatalogOptions::default(),
        )
        .expect("set_catalog");
    let mut classifier = controller
        .provider
        .get_all_models()
        .unwrap_or_default()
        .into_iter()
        .find_map(|m| match m {
            AnyModel::Classifier(c) => Some(c),
            _ => None,
        })
        .expect("classifier model");
    classifier.base_url = format!("{}/v1", srv.url);

    let mut state = IndexMap::new();
    state.insert(
        "message".to_string(),
        Value::String("The build is red again.".to_string()),
    );
    let mut criteria = IndexMap::new();
    criteria.insert("billing".to_string(), String::new());
    criteria.insert("ci".to_string(), String::new());
    let mut questions = IndexMap::new();
    questions.insert(
        "kind".to_string(),
        ClassifierQuestion::Choice {
            instructions: "What is this about?".to_string(),
            criteria,
        },
    );
    let result = controller
        .provider
        .classify(
            &classifier,
            &ClassifierContext { state, questions },
            Some(ClassifierOptions {
                base: ProviderRequestOptions {
                    api_key: Some("local".to_string()),
                    ..ProviderRequestOptions::default()
                },
                temperature: None,
            }),
        )
        .expect("classify fut")
        .await;

    assert_eq!(result.error_message, None);
    assert!(
        matches!(
            result.answers.get("kind"),
            Some(ClassifierAnswer::Choice { choice, .. }) if choice == "ci"
        ),
        "answers: {:?}",
        result.answers
    );
    assert!(
        paths
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&"/completion".to_string())
    );
    srv.close().await;
}

#[tokio::test]
async fn hides_unloaded_presets_when_router_autoload_is_disabled() {
    let srv = TestServer::start(|req| {
        Box::pin(async move {
            if req.url == "/models" {
                return json_ok(json!({
                    "data": [{ "id": "preset", "status": { "value": "unloaded" }, "source": "preset" }]
                }));
            }
            if req.url == "/props" {
                return json_ok(json!({ "role": "router", "models_autoload": false }));
            }
            empty_404()
        })
    })
    .await;

    let controller = create_llama_provider();
    let cred = Credential::ApiKey(ApiKeyCredential {
        key: Some("local".to_string()),
        env: Some(IndexMap::from([("LLAMA_BASE_URL".to_string(), srv.url.clone())])),
    });
    controller
        .provider
        .refresh_models(RefreshModelsContext {
            credential: Some(cred),
            stored: None,
            publish: Arc::new(|p| {
                Box::pin(async move {
                    if let Some(u) = p.update {
                        u();
                    }
                    Ok(true)
                })
            }),
            allow_network: true,
            force: None,
            signal: AbortController::new().signal(),
        })
        .expect("refresh")
        .await
        .expect("ok");

    assert!(controller.provider.get_models().is_empty());
    srv.close().await;
}

#[tokio::test]
async fn stays_dormant_until_configured_and_stores_url_plus_optional_key() {
    let controller = create_llama_provider();
    let auth = controller.provider.auth().api_key.as_ref().expect("api_key auth");
    let ctx: Arc<dyn AuthContext> = Arc::new(EmptyAuthContext);
    let signal = AbortController::new().signal();

    assert!(
        auth.check(ApiKeyAuthInput {
            ctx: Arc::clone(&ctx),
            credential: None,
            signal: signal.clone(),
        })
        .await
        .expect("check")
        .is_none()
    );
    assert!(
        auth.resolve(ApiKeyAuthInput {
            ctx: Arc::clone(&ctx),
            credential: None,
            signal: signal.clone(),
        })
        .await
        .expect("resolve")
        .is_none()
    );

    let srv = TestServer::start(|req| {
        Box::pin(async move {
            let hdr = req
                .headers
                .get(hyper::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            assert_eq!(hdr, Some("Bearer secret"));
            json_ok(json!({ "data": [] }))
        })
    })
    .await;

    let interaction = Arc::new(ScriptedInteraction {
        answers: Mutex::new(VecDeque::from(vec![srv.url.clone(), "secret".to_string()])),
    });
    let cred = auth
        .login(ProviderAuthInteraction::new(interaction, signal.clone()))
        .await
        .expect("login");
    assert_eq!(
        cred,
        ApiKeyCredential {
            key: Some("secret".to_string()),
            env: Some(IndexMap::from([("LLAMA_BASE_URL".to_string(), srv.url.clone())])),
        }
    );

    let resolved = auth
        .resolve(ApiKeyAuthInput {
            ctx,
            credential: Some(cred),
            signal,
        })
        .await
        .expect("resolve")
        .expect("some auth");
    assert_eq!(resolved.auth.api_key.as_deref(), Some("secret"));
    assert_eq!(
        resolved.auth.base_url.as_deref(),
        Some(format!("{}/v1", srv.url).as_str())
    );
    assert_eq!(resolved.source.as_deref(), Some("stored credential"));
    srv.close().await;
}

#[tokio::test]
async fn searches_hugging_face_and_reads_quantizations_plus_access_requirements() {
    let srv = TestServer::start(|req| {
        Box::pin(async move {
            let auth = req
                .headers
                .get(hyper::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            assert_eq!(auth, Some("Bearer hf-secret"));
            if req.url.starts_with("/api/models?") {
                let parsed = url::Url::parse(&format!("http://localhost{}", req.url)).expect("url");
                let param = |k: &str| {
                    parsed
                        .query_pairs()
                        .find(|(key, _)| key == k)
                        .map(|(_, v)| v.into_owned())
                };
                assert_eq!(param("search").as_deref(), Some("qwen coder"));
                assert_eq!(param("filter").as_deref(), Some("gguf"));
                assert_eq!(param("sort").as_deref(), Some("downloads"));
                return json_ok(json!([{ "id": "owner/model-GGUF", "downloads": 1200 }]));
            }
            if req.url == "/api/models/owner/model-GGUF?blobs=true" {
                return json_ok(json!({
                    "id": "owner/model-GGUF",
                    "gated": "manual",
                    "siblings": [
                        { "rfilename": "model-Q5_K_M.gguf", "size": 6000 },
                        { "rfilename": "model-Q4_K_M-00001-of-00002.gguf", "size": 2000 },
                        { "rfilename": "model-Q4_K_M-00002-of-00002.gguf", "size": 3000 },
                        { "rfilename": "mmproj-F16.gguf", "size": 1000 }
                    ]
                }));
            }
            empty_404()
        })
    })
    .await;

    let client = HuggingFaceClient::new(Some("hf-secret".to_string()), Some(&srv.url));
    assert_eq!(
        client.search("qwen coder", None).await.expect("search"),
        vec![HuggingFaceModel {
            id: "owner/model-GGUF".to_string(),
            downloads: 1200,
        }]
    );
    assert_eq!(
        client.details("owner/model-GGUF", None).await.expect("details"),
        HuggingFaceModelDetails {
            id: "owner/model-GGUF".to_string(),
            gated: HuggingFaceGated::Manual,
            quantizations: vec![
                HuggingFaceQuantization {
                    name: "Q4_K_M".to_string(),
                    size: Some(5000),
                },
                HuggingFaceQuantization {
                    name: "Q5_K_M".to_string(),
                    size: Some(6000),
                },
            ],
        }
    );
    let env = IndexMap::from([("HF_TOKEN".to_string(), " hf-secret ".to_string())]);
    assert_eq!(find_hugging_face_token(Some(&env)).await.as_deref(), Some("hf-secret"));
    srv.close().await;
}

#[tokio::test]
async fn loads_with_sse_progress_and_waits_for_loaded_catalog_state() {
    let status = Arc::new(Mutex::new("unloaded".to_string()));
    let streams: Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>> = Arc::new(Mutex::new(Vec::new()));

    let st_clone = Arc::clone(&status);
    let str_clone = Arc::clone(&streams);
    let srv = TestServer::start(move |req| {
        let status = Arc::clone(&st_clone);
        let streams = Arc::clone(&str_clone);
        Box::pin(async move {
            if req.url == "/models/sse" {
                let (tx, rx) = mpsc::unbounded_channel();
                streams.lock().unwrap_or_else(|e| e.into_inner()).push(tx);
                return sse_ok(rx);
            }
            if req.url == "/models/load" && req.method == "POST" {
                *status.lock().unwrap_or_else(|e| e.into_inner()) = "loading".to_string();
                let status_bg = Arc::clone(&status);
                let streams_bg = Arc::clone(&streams);
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    let ev1 = pi_js::json::stringify(&json!({
                        "model": "test-model",
                        "event": "status_change",
                        "data": {
                            "status": "loading",
                            "progress": { "stages": ["text_model", "mmproj_model"], "current": "text_model", "value": 0.5 }
                        }
                    }));
                    let msg1 = Bytes::from(format!("data: {ev1}\n\n"));
                    for tx in &*streams_bg.lock().unwrap_or_else(|e| e.into_inner()) {
                        let _ = tx.send(msg1.clone());
                    }
                    *status_bg.lock().unwrap_or_else(|e| e.into_inner()) = "loaded".to_string();
                    let ev2 = pi_js::json::stringify(&json!({
                        "model": "test-model",
                        "event": "status_change",
                        "data": { "status": "loaded" }
                    }));
                    let msg2 = Bytes::from(format!("data: {ev2}\n\n"));
                    for tx in &*streams_bg.lock().unwrap_or_else(|e| e.into_inner()) {
                        let _ = tx.send(msg2.clone());
                    }
                });
                return json_ok(json!({ "success": true }));
            }
            if req.url == "/models" {
                let s = status.lock().unwrap_or_else(|e| e.into_inner()).clone();
                return json_ok(json!({ "data": [{ "id": "test-model", "status": { "value": s } }] }));
            }
            empty_404()
        })
    })
    .await;

    let progress: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let prog_cb = Arc::clone(&progress);
    let model = LlamaClient::new(&srv.url, None)
        .expect("client")
        .load_and_wait(
            "test-model",
            Arc::new(move |entry| {
                prog_cb.lock().unwrap_or_else(|e| e.into_inner()).push(entry.message);
            }),
            None,
        )
        .await
        .expect("load_and_wait");

    assert_eq!(model.status.value, LlamaModelStatus::Loaded);
    assert!(
        progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&"Loading text model".to_string())
    );
    srv.close().await;
}

#[tokio::test]
async fn downloads_with_byte_progress_and_returns_refreshed_catalog() {
    let status = Arc::new(Mutex::new("missing".to_string()));
    let streams: Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>> = Arc::new(Mutex::new(Vec::new()));

    let st_clone = Arc::clone(&status);
    let str_clone = Arc::clone(&streams);
    let srv = TestServer::start(move |req| {
        let status = Arc::clone(&st_clone);
        let streams = Arc::clone(&str_clone);
        Box::pin(async move {
            if req.url == "/models/sse" {
                let (tx, rx) = mpsc::unbounded_channel();
                streams.lock().unwrap_or_else(|e| e.into_inner()).push(tx);
                return sse_ok(rx);
            }
            if req.url == "/models" && req.method == "POST" {
                *status.lock().unwrap_or_else(|e| e.into_inner()) = "downloading".to_string();
                let status_bg = Arc::clone(&status);
                let streams_bg = Arc::clone(&streams);
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    let ev1 = pi_js::json::stringify(&json!({
                        "model": "owner/repo:Q4_K_M",
                        "event": "download_progress",
                        "data": { "progress": { "https://example/model.gguf": { "done": 512, "total": 1024 } } }
                    }));
                    let msg1 = Bytes::from(format!("data: {ev1}\n\n"));
                    for tx in &*streams_bg.lock().unwrap_or_else(|e| e.into_inner()) {
                        let _ = tx.send(msg1.clone());
                    }
                    *status_bg.lock().unwrap_or_else(|e| e.into_inner()) = "unloaded".to_string();
                    let ev2 = pi_js::json::stringify(&json!({
                        "model": "owner/repo:Q4_K_M",
                        "event": "download_finished",
                        "data": {}
                    }));
                    let msg2 = Bytes::from(format!("data: {ev2}\n\n"));
                    for tx in &*streams_bg.lock().unwrap_or_else(|e| e.into_inner()) {
                        let _ = tx.send(msg2.clone());
                    }
                });
                return json_ok(json!({ "success": true }));
            }
            if req.url.starts_with("/models") {
                let s = status.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let data = if s == "missing" {
                    json!([])
                } else {
                    json!([{ "id": "owner/repo:Q4_K_M", "status": { "value": s } }])
                };
                return json_ok(json!({ "data": data }));
            }
            empty_404()
        })
    })
    .await;

    let progress: Arc<Mutex<Vec<LlamaProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let prog_cb = Arc::clone(&progress);
    let models = LlamaClient::new(&srv.url, None)
        .expect("client")
        .download_and_wait(
            "owner/repo:Q4_K_M",
            Arc::new(move |entry| {
                prog_cb.lock().unwrap_or_else(|e| e.into_inner()).push(entry);
            }),
            None,
        )
        .await
        .expect("download_and_wait");

    assert_eq!(
        models,
        vec![LlamaModelInfo {
            id: "owner/repo:Q4_K_M".to_string(),
            aliases: None,
            status: LlamaModelStatusInfo {
                value: LlamaModelStatus::Unloaded,
                args: None,
                failed: None,
                exit_code: None,
                progress: None,
            },
            architecture: None,
            source: None,
            meta: None,
        }]
    );
    assert!(
        progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&LlamaProgress {
                message: "Downloading model".to_string(),
                ratio: Some(0.5),
                detail: Some("512 B / 1.00 KiB".to_string()),
            })
    );
    srv.close().await;
}

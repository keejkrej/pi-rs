//! Port of packages/coding-agent/test/remote-catalog-provider.test.ts

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use pi_ai::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthResult, Credential, ModelAuth, ProviderAuth};
use pi_ai::models::{
    CreateModelsOptions, CreateProviderApi, CreateProviderOptions, ModelsPublication, Provider, RefreshModelsContext,
    create_models, create_provider,
};
use pi_ai::models_store::{InMemoryModelsStore, ModelsStore};
use pi_ai::types::{
    AnyModel, ApiStreamOptions, Model, ModelCost, ModelInput, ModelType, ProviderStreams, SimpleStreamFunction,
    SimpleStreamOptions, StreamFunction, TranscriptContext,
};
use pi_coding_agent::config::VERSION;
use pi_coding_agent::core::remote_catalog_provider::{REMOTE_CATALOG_MODEL_TYPES, with_remote_catalog};
use pi_js::abort::AbortController;
use pi_js::fetch::Request;
use pi_js::fetch::testing::{mock_fetch, response as fetch_response};
use pi_js::json::stringify;
use pi_js::time::parse_date;
use pi_js::{BoxFuture, Result};
use serde_json::json;

struct StaticTestApiKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for StaticTestApiKeyAuth {
    fn name(&self) -> &str {
        "Test"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth::default(),
            env: None,
            source: None,
        }))
    }
}

fn chat_model(id: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "openai-completions".to_owned(),
        provider: "test-provider".to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn unused_stream_fn() -> StreamFunction {
    Arc::new(
        |_model: &Model, _ctx: &TranscriptContext, _opts: Option<ApiStreamOptions>| {
            panic!("stream not used in remote catalog test");
        },
    )
}

fn unused_simple_stream_fn() -> SimpleStreamFunction {
    Arc::new(
        |_model: &Model, _ctx: &TranscriptContext, _opts: Option<SimpleStreamOptions>| {
            panic!("stream_simple not used in remote catalog test");
        },
    )
}

fn test_provider(local_generated_at: Option<i64>) -> Arc<dyn Provider + Send + Sync> {
    let base = create_provider(CreateProviderOptions {
        id: "test-provider".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(StaticTestApiKeyAuth)),
            oauth: None,
        },
        models: vec![AnyModel::Chat(chat_model("static"))],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(ProviderStreams {
            stream: unused_stream_fn(),
            stream_simple: unused_simple_stream_fn(),
            fetch_deferred: None,
            cancel_deferred: None,
        })),
        images: None,
        classifiers: None,
    })
    .unwrap();
    with_remote_catalog(base, Some("https://pi.dev"), local_generated_at)
}

async fn refresh_provider(
    provider: &Arc<dyn Provider + Send + Sync>,
    store: &Arc<InMemoryModelsStore>,
    allow_network: bool,
    force: bool,
) -> Result<()> {
    let store_ref = store.clone();
    let pid = provider.id().to_owned();
    let publish: pi_ai::models::PublishModels = Arc::new(move |pub_: ModelsPublication| {
        let store_ref = store_ref.clone();
        let pid = pid.clone();
        Box::pin(async move {
            match pub_.persist {
                Some(None) => store_ref.delete(&pid, None).await?,
                Some(Some(entry)) => store_ref.write(&pid, entry, None).await?,
                None => {}
            }
            if let Some(update) = pub_.update {
                update();
            }
            Ok(true)
        })
    });

    let stored = store.read(provider.id(), None).await?;
    let signal = AbortController::new().signal();
    if let Some(fut) = provider.refresh_models(RefreshModelsContext {
        credential: Some(Credential::ApiKey(Default::default())),
        stored,
        publish,
        allow_network,
        force: Some(force),
        signal,
    }) {
        fut.await?;
    }
    Ok(())
}

#[tokio::test]
async fn parses_keyed_catalogs_sends_version_headers_observes_ttl_and_forced_refreshes() {
    let recorded_requests: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded_clone = recorded_requests.clone();
    let _guard = mock_fetch(move |req| {
        recorded_clone.lock().unwrap().push(req);
        let body = stringify(&json!({ "dynamic": chat_model("dynamic") }));
        Box::pin(async move { Ok(fetch_response(200, &[("content-type", "application/json")], body)) })
    });

    let provider = test_provider(None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, true, false).await.unwrap();
    refresh_provider(&provider, &store, true, false).await.unwrap();
    refresh_provider(&provider, &store, true, true).await.unwrap();

    let ids: Vec<String> = provider.get_models().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, vec!["static", "dynamic"]);
    let stored = store.read(provider.id(), None).await.unwrap().unwrap();
    assert_eq!(
        stored.models.iter().map(AnyModel::id).collect::<Vec<_>>(),
        vec!["dynamic"]
    );

    let calls = recorded_requests.lock().unwrap();
    assert_eq!(calls.len(), 2);
    let ua = calls[0].headers.get("User-Agent").unwrap_or_default();
    assert!(ua.contains(&format!("pi/{}", *VERSION)));
    assert!(
        calls[0].url.contains("/api/models/providers/test-provider"),
        "unexpected URL: {}",
        calls[0].url
    );
    let expected_types = REMOTE_CATALOG_MODEL_TYPES
        .iter()
        .map(|t| match t {
            ModelType::Chat => "chat",
            ModelType::Image => "image",
            ModelType::Classifier => "classifier",
        })
        .collect::<Vec<_>>()
        .join(",");
    assert!(
        calls[0].url.contains(&format!("types={}", expected_types))
            || calls[0]
                .url
                .contains(&format!("types={}", expected_types.replace(',', "%2C")))
    );
}

#[tokio::test]
async fn overlays_image_and_classifier_models_and_drops_unknown_model_types() {
    let _guard = mock_fetch(move |_req| {
        let body = stringify(&json!({
            "chat": {
                "type": "chat",
                "id": "chat",
                "name": "chat",
                "api": "openai-completions",
                "provider": "test-provider",
                "baseUrl": "https://example.test/v1",
                "reasoning": false,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 1000,
                "maxTokens": 100
            },
            "flux": {
                "type": "image",
                "id": "flux",
                "name": "FLUX",
                "api": "openrouter-images",
                "provider": "test-provider",
                "baseUrl": "https://example.test/v1",
                "input": ["text"],
                "output": ["image"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
            },
            "jev": {
                "type": "classifier",
                "id": "jev",
                "name": "Jev",
                "api": "typesafe-system-one",
                "provider": "test-provider",
                "baseUrl": "https://example.test/v1",
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 64000
            },
            "clip": {
                "type": "video",
                "id": "clip",
                "name": "clip",
                "api": "openai-completions",
                "provider": "test-provider",
                "baseUrl": "https://example.test/v1",
                "reasoning": false,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 1000,
                "maxTokens": 100
            }
        }));
        Box::pin(async move { Ok(fetch_response(200, &[("content-type", "application/json")], body)) })
    });

    let provider = test_provider(None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, true, false).await.unwrap();

    let models = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: Some(store.clone()),
        auth_context: None,
    }));
    models.set_provider(provider.clone());

    let all_ids: Vec<String> = models
        .get_all_models(Some("test-provider"))
        .into_iter()
        .map(|m| m.id().to_owned())
        .collect();
    assert_eq!(all_ids, vec!["static", "chat", "flux", "jev"]);
    assert!(matches!(
        models.get_model_of_type(ModelType::Image, "test-provider", "flux"),
        Some(AnyModel::Image(_))
    ));
    assert!(matches!(
        models.get_model_of_type(ModelType::Classifier, "test-provider", "jev"),
        Some(AnyModel::Classifier(_))
    ));
    assert!(models.get_model("test-provider", "flux").is_none());

    let stored = store.read(provider.id(), None).await.unwrap().unwrap();
    assert_eq!(
        stored.models.iter().map(AnyModel::id).collect::<Vec<_>>(),
        vec!["chat", "flux", "jev"]
    );
}

#[tokio::test]
async fn prefers_the_newer_of_the_generated_and_remote_catalogs() {
    let local_generated_at = parse_date("2026-07-23T10:00:00.000Z").unwrap() as i64;
    let queue = Arc::new(Mutex::new(VecDeque::from(vec![
        fetch_response(
            200,
            &[
                ("content-type", "application/json"),
                ("last-modified", "Thu, 23 Jul 2026 09:59:00 GMT"),
            ],
            stringify(&json!({ "old": chat_model("old") })),
        ),
        fetch_response(
            200,
            &[
                ("content-type", "application/json"),
                ("last-modified", "Thu, 23 Jul 2026 10:01:00 GMT"),
            ],
            stringify(&json!({ "newer": chat_model("newer") })),
        ),
    ])));

    let q_clone = queue.clone();
    let _guard = mock_fetch(move |_req| {
        let resp = q_clone.lock().unwrap().pop_front().unwrap();
        Box::pin(async move { Ok(resp) })
    });

    let provider = test_provider(Some(local_generated_at));
    let store = Arc::new(InMemoryModelsStore::new());

    refresh_provider(&provider, &store, true, false).await.unwrap();
    assert_eq!(
        provider.get_models().into_iter().map(|m| m.id).collect::<Vec<_>>(),
        vec!["static"]
    );

    refresh_provider(&provider, &store, true, true).await.unwrap();
    assert_eq!(
        provider.get_models().into_iter().map(|m| m.id).collect::<Vec<_>>(),
        vec!["static", "newer"]
    );
    let stored = store.read(provider.id(), None).await.unwrap().unwrap();
    assert_eq!(
        stored.last_modified,
        Some(parse_date("Thu, 23 Jul 2026 10:01:00 GMT").unwrap() as i64)
    );
}

#[tokio::test]
async fn revalidates_stored_catalog_with_etag_and_keeps_overlay_on_304() {
    let queue = Arc::new(Mutex::new(VecDeque::from(vec![
        fetch_response(
            200,
            &[("content-type", "application/json"), ("etag", "\"catalog-1\"")],
            stringify(&json!({ "dynamic": chat_model("dynamic") })),
        ),
        fetch_response(304, &[("etag", "\"catalog-1\"")], ""),
    ])));
    let recorded: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
    let q_clone = queue.clone();
    let r_clone = recorded.clone();
    let _guard = mock_fetch(move |req| {
        r_clone.lock().unwrap().push(req);
        let resp = q_clone.lock().unwrap().pop_front().unwrap();
        Box::pin(async move { Ok(resp) })
    });

    let provider = test_provider(None);
    let store = Arc::new(InMemoryModelsStore::new());

    refresh_provider(&provider, &store, true, false).await.unwrap();
    {
        let calls = recorded.lock().unwrap();
        assert!(calls[0].headers.get("if-none-match").is_none());
    }
    let stored1 = store.read(provider.id(), None).await.unwrap().unwrap();
    assert_eq!(stored1.etag.as_deref(), Some("\"catalog-1\""));
    let checked_at = stored1.checked_at.unwrap_or(0);

    refresh_provider(&provider, &store, true, true).await.unwrap();
    {
        let calls = recorded.lock().unwrap();
        assert_eq!(calls[1].headers.get("if-none-match").as_deref(), Some("\"catalog-1\""));
    }
    assert_eq!(
        provider.get_models().into_iter().map(|m| m.id).collect::<Vec<_>>(),
        vec!["static", "dynamic"]
    );
    let stored2 = store.read(provider.id(), None).await.unwrap().unwrap();
    assert_eq!(
        stored2.models.iter().map(AnyModel::id).collect::<Vec<_>>(),
        vec!["dynamic"]
    );
    assert_eq!(stored2.etag.as_deref(), Some("\"catalog-1\""));
    assert!(stored2.checked_at.unwrap_or(0) >= checked_at);
}

#[tokio::test]
async fn drops_stale_etag_when_overlay_becomes_unavailable() {
    let queue = Arc::new(Mutex::new(VecDeque::from(vec![
        fetch_response(
            200,
            &[("content-type", "application/json"), ("etag", "\"catalog-1\"")],
            stringify(&json!({ "dynamic": chat_model("dynamic") })),
        ),
        fetch_response(501, &[], "not implemented"),
    ])));
    let q_clone = queue.clone();
    let _guard = mock_fetch(move |_req| {
        let resp = q_clone.lock().unwrap().pop_front().unwrap();
        Box::pin(async move { Ok(resp) })
    });

    let provider = test_provider(None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, true, false).await.unwrap();
    refresh_provider(&provider, &store, true, true).await.unwrap();

    let stored = store.read(provider.id(), None).await.unwrap().unwrap();
    assert!(stored.etag.is_none());
}

#[tokio::test]
async fn keeps_etag_and_overlay_after_transient_failure() {
    let queue = Arc::new(Mutex::new(VecDeque::from(vec![
        fetch_response(
            200,
            &[("content-type", "application/json"), ("etag", "\"catalog-1\"")],
            stringify(&json!({ "dynamic": chat_model("dynamic") })),
        ),
        fetch_response(429, &[], "rate limited"),
        fetch_response(429, &[], "rate limited"),
        fetch_response(429, &[], "rate limited"),
        fetch_response(304, &[("etag", "\"catalog-1\"")], ""),
    ])));
    let recorded: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
    let q_clone = queue.clone();
    let r_clone = recorded.clone();
    let _guard = mock_fetch(move |req| {
        r_clone.lock().unwrap().push(req);
        let resp = q_clone.lock().unwrap().pop_front().unwrap();
        Box::pin(async move { Ok(resp) })
    });

    let provider = test_provider(None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, true, false).await.unwrap();

    let err = refresh_provider(&provider, &store, true, true).await.unwrap_err();
    assert!(err.to_string().contains("429"));

    let stored = store.read(provider.id(), None).await.unwrap().unwrap();
    assert_eq!(stored.etag.as_deref(), Some("\"catalog-1\""));
    assert_eq!(
        stored.models.iter().map(AnyModel::id).collect::<Vec<_>>(),
        vec!["dynamic"]
    );

    refresh_provider(&provider, &store, true, true).await.unwrap();
    assert_eq!(
        provider.get_models().into_iter().map(|m| m.id).collect::<Vec<_>>(),
        vec!["static", "dynamic"]
    );
}

#[tokio::test]
async fn lets_newer_catalog_request_bypass_stalled_older_request_without_stale_publication() {
    let call_count = Arc::new(Mutex::new(0usize));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));

    let count_clone = call_count.clone();
    let started_clone = started_tx.clone();
    let release_clone = release_rx.clone();
    let _guard = mock_fetch(move |_req| -> BoxFuture<Result<_>> {
        let mut idx = count_clone.lock().unwrap();
        *idx += 1;
        let current = *idx;
        let started_clone = started_clone.clone();
        let release_clone = release_clone.clone();
        Box::pin(async move {
            if current == 1 {
                if let Some(tx) = started_clone.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release_clone.lock().await.take() {
                    let _ = rx.await;
                }
                return Ok(fetch_response(
                    200,
                    &[("content-type", "application/json")],
                    stringify(&json!({ "older": chat_model("older") })),
                ));
            }
            Ok(fetch_response(
                200,
                &[("content-type", "application/json")],
                stringify(&json!({ "newer": chat_model("newer") })),
            ))
        })
    });

    let provider = test_provider(None);
    let store = Arc::new(InMemoryModelsStore::new());
    let models = Arc::new(create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: Some(store.clone()),
        auth_context: None,
    })));
    models.set_provider(provider.clone());

    let m1 = models.clone();
    let pid1 = provider.id().to_owned();
    let first_task = tokio::spawn(async move {
        m1.refresh(Some(pi_ai::models::ModelsRefreshOptions {
            allow_network: Some(true),
            providers: Some(vec![pid1]),
            force: Some(true),
            signal: None,
        }))
        .await
    });
    let _ = started_rx.await;

    models
        .refresh(Some(pi_ai::models::ModelsRefreshOptions {
            allow_network: Some(true),
            providers: Some(vec![provider.id().to_owned()]),
            force: Some(true),
            signal: None,
        }))
        .await
        .unwrap();
    let _ = release_tx.send(());
    let _ = first_task.await;

    assert_eq!(
        provider.get_models().into_iter().map(|m| m.id).collect::<Vec<_>>(),
        vec!["static", "newer"]
    );
    let stored = store.read(provider.id(), None).await.unwrap().unwrap();
    assert_eq!(
        stored.models.iter().map(AnyModel::id).collect::<Vec<_>>(),
        vec!["newer"]
    );
}

#[tokio::test]
async fn treats_unimplemented_catalog_routes_as_unavailable_overlay() {
    let _guard = mock_fetch(|_req| Box::pin(async move { Ok(fetch_response(501, &[], "not implemented")) }));

    let provider = test_provider(None);
    let store = Arc::new(InMemoryModelsStore::new());

    refresh_provider(&provider, &store, true, false).await.unwrap();
    assert_eq!(
        provider.get_models().into_iter().map(|m| m.id).collect::<Vec<_>>(),
        vec!["static"]
    );
    let stored = store.read(provider.id(), None).await.unwrap().unwrap();
    assert!(stored.models.is_empty());
    assert!(stored.checked_at.is_some());
}

//! Port of packages/coding-agent/test/sdk-stream-options.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Model, ModelCost, ModelInput, ModelPromptCache, ProviderHeaders,
    SimpleStreamOptions, StopReason, TextContent, Usage, UserMessage, UserMessageContent,
};
use pi_ai::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use pi_ai::{ApiKeyCredential, Credential, CredentialStore};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::cache_warmer::CacheWarmingRunState;
use pi_coding_agent::core::extensions::types::{
    BeforeProviderHeadersEvent, ExtensionApi, InlineExtension, ProviderConfig, ProviderStreamEvent,
};
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::{
    CacheWarmingMode, ProviderRetrySettings, RetrySettings, Settings, SettingsManager,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::model_runtime_test_utils::{create_model_registry, get_model_runtime};

fn create_model(api: &str) -> Model {
    let mut headers = IndexMap::new();
    headers.insert("x-model".to_string(), "model".to_string());
    Model {
        r#type: None,
        id: "capture-model".to_string(),
        name: "Capture Model".to_string(),
        api: api.to_string(),
        provider: "capture-provider".to_string(),
        base_url: "https://capture.invalid/v1".to_string(),
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
        context_window: 128_000,
        max_tokens: 4_096,
        headers: Some(headers),
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn create_done_message(api: &str, prompt_tokens: i64) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new("ok"))],
        api: api.to_string(),
        provider: "capture-provider".to_string(),
        model: "capture-model".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            cache_read: prompt_tokens,
            total_tokens: prompt_tokens,
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
        duration_ms: None,
    }
}

fn create_done_stream(api: &str, prompt_tokens: i64) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    stream.end(Some(create_done_message(api, prompt_tokens)));
    stream
}

async fn capture_stream_options(
    api: &str,
    settings: Settings,
    request_options: SimpleStreamOptions,
    extension_factory: Option<InlineExtension>,
    provider_event: Option<Value>,
) -> Option<SimpleStreamOptions> {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-sdk-stream-options-"])).unwrap();
    let cwd = pi_js::path::join(&[&temp_dir, "project"]);
    let agent_dir = pi_js::path::join(&[&temp_dir, "agent"]);
    pi_js::fs::mkdir(&cwd, true, None).unwrap();
    pi_js::fs::mkdir(&agent_dir, true, None).unwrap();

    let model = create_model(api);
    let settings_manager = SettingsManager::in_memory(Some(settings), None);
    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        settings_manager: Some(settings_manager.clone()),
        extension_factories: extension_factory
            .map(|f| vec![pi_coding_agent::core::resource_loader::InlineExtension::Core(f)])
            .unwrap_or_default(),
        ..Default::default()
    }));
    resource_loader.reload(None).await.unwrap();

    let auth_storage = Arc::new(AuthStorage::create(Some(&pi_js::path::join(&[
        &agent_dir,
        "auth.json",
    ]))));
    auth_storage
        .modify(
            &model.provider,
            Box::new(|_prev| {
                Box::pin(async move {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("test-api-key".to_string()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();
    let model_registry = create_model_registry(auth_storage, Some(&pi_js::path::join(&[&agent_dir, "models.json"])))
        .await
        .unwrap();
    let captured: Arc<Mutex<Option<SimpleStreamOptions>>> = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);

    let mut provider_headers = IndexMap::new();
    provider_headers.insert("x-provider".to_string(), "provider".to_string());
    let api_owned = api.to_string();
    let ev_opt = provider_event.clone();

    model_registry
        .register_provider(
            &model.provider,
            ProviderConfig {
                api: Some(api.to_string()),
                headers: Some(provider_headers),
                stream_simple: Some(Arc::new(move |request_model, _ctx, provider_opts| {
                    *captured_ref.lock().unwrap_or_else(PoisonError::into_inner) = provider_opts.clone();
                    if ev_opt.is_none() {
                        return create_done_stream(&api_owned, 0);
                    }
                    let stream = create_assistant_message_event_stream();
                    let stream_clone = stream.clone();
                    let ev_val = ev_opt.clone().unwrap();
                    let api_done = api_owned.clone();
                    tokio::spawn(async move {
                        if let Some(cb) = provider_opts.and_then(|o| o.base.on_provider_stream_event) {
                            let _ = cb(&ev_val, &request_model).await;
                        }
                        stream_clone.end(Some(create_done_message(&api_done, 0)));
                    });
                    stream
                })),
                ..Default::default()
            }
            .into(),
        )
        .unwrap();

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(agent_dir),
        model: Some(model.clone()),
        model_runtime: Some(get_model_runtime(&model_registry)),
        settings_manager: Some(settings_manager),
        session_manager: Some(SessionManager::in_memory(Some(&cwd), None, None).unwrap()),
        resource_loader: Some(resource_loader),
        ..Default::default()
    }))
    .await
    .unwrap();
    let session = created.session;

    if provider_event.is_none() {
        let stream_fn = session.agent().stream_fn().unwrap();
        let stream = stream_fn(
            model.clone(),
            Context {
                system_prompt: None,
                messages: vec![],
                tools: None,
            },
            Some(request_options),
        )
        .await
        .unwrap();
        let _ = stream.result().await;
    } else {
        session.prompt("test", None).await.unwrap();
    }

    let result = captured.lock().unwrap_or_else(PoisonError::into_inner).clone();
    session.dispose();
    model_registry.unregister_provider(&model.provider);
    let _ = pi_js::fs::rm(&temp_dir, true, true);
    result
}

async fn create_cache_warming_session<F>(
    populate: Option<F>,
) -> (
    pi_coding_agent::core::agent_session::AgentSession,
    Arc<std::sync::atomic::AtomicUsize>,
    Box<dyn FnOnce() + Send>,
)
where
    F: FnOnce(&SessionManager, &Model),
{
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-sdk-cache-warm-"])).unwrap();
    let cwd = pi_js::path::join(&[&temp_dir, "project"]);
    let agent_dir = pi_js::path::join(&[&temp_dir, "agent"]);
    pi_js::fs::mkdir(&cwd, true, None).unwrap();
    pi_js::fs::mkdir(&agent_dir, true, None).unwrap();

    let mut model = create_model("anthropic-messages");
    model.cost = ModelCost {
        input: 10.0,
        output: 50.0,
        cache_read: 0.25,
        cache_write: 12.5,
        tiers: None,
    };
    model.prompt_cache = Some(ModelPromptCache {
        short: Some(300),
        long: None,
    });

    let auth_storage = Arc::new(AuthStorage::create(Some(&pi_js::path::join(&[
        &agent_dir,
        "auth.json",
    ]))));
    auth_storage
        .modify(
            &model.provider,
            Box::new(|_prev| {
                Box::pin(async move {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("test-api-key".to_string()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();

    let model_registry = create_model_registry(auth_storage, Some(&pi_js::path::join(&[&agent_dir, "models.json"])))
        .await
        .unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let api_owned = model.api.clone();
    model_registry
        .register_provider(
            &model.provider,
            ProviderConfig {
                api: Some(model.api.clone()),
                stream_simple: Some(Arc::new(move |_m, _ctx, _opts| {
                    calls_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    create_done_stream(&api_owned, 100_000)
                })),
                ..Default::default()
            }
            .into(),
        )
        .unwrap();

    let session_manager = SessionManager::in_memory(Some(&cwd), None, None).unwrap();
    if let Some(f) = populate {
        f(&session_manager, &model);
    }

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(cwd),
        agent_dir: Some(agent_dir),
        model: Some(model.clone()),
        model_runtime: Some(get_model_runtime(&model_registry)),
        settings_manager: Some(SettingsManager::in_memory(
            Some(Settings {
                cache_warming: Some(CacheWarmingMode::Idle),
                ..Default::default()
            }),
            None,
        )),
        session_manager: Some(session_manager),
        ..Default::default()
    }))
    .await
    .unwrap();
    let session = created.session;
    let session_cleanup = session.clone();
    let provider_name = model.provider.clone();
    let dispose = Box::new(move || {
        session_cleanup.dispose();
        model_registry.unregister_provider(&provider_name);
        let _ = pi_js::fs::rm(&temp_dir, true, true);
    });
    (session, calls, dispose)
}

#[tokio::test]
async fn schedules_cache_warming_after_a_completed_session_request() {
    let (session, _calls, dispose) = create_cache_warming_session::<fn(&SessionManager, &Model)>(None).await;
    session.prompt("test", None).await.unwrap();

    let status = session.cache_warming_status().unwrap();
    assert!(status.next_warm_at.is_some_and(|ts| ts > pi_js::time::now_ms()));

    // Equivalent shallow copies remain current, but removing the request prefix does not.
    let msgs = session.agent().state().messages;
    session.agent().replace_messages(msgs.clone());
    let model = session.agent().state().model;
    session.agent().set_model(model);
    assert!(
        session
            .cache_warming_status()
            .and_then(|s| s.next_warm_at)
            .is_some_and(|ts| ts > pi_js::time::now_ms())
    );

    if !msgs.is_empty() {
        session.agent().replace_messages(msgs[1..].to_vec());
    }
    assert_eq!(
        session.cache_warming_status().and_then(|s| s.reason),
        Some("conversation context changed".to_string())
    );

    dispose();
}

#[tokio::test]
async fn waits_for_the_next_request_instead_of_restoring_cache_warming() {
    let (session, calls, dispose) = create_cache_warming_session(Some(|manager: &SessionManager, model: &Model| {
        manager.append_model_change(&model.provider, &model.id).unwrap();
        manager.append_thinking_level_change("off").unwrap();
        manager
            .append_message(
                pi_ai::types::Message::User(UserMessage {
                    content: UserMessageContent::Text("test".to_string()),
                    timestamp: pi_js::time::now_ms(),
                })
                .into(),
            )
            .unwrap();
        let assistant = create_done_message(&model.api, 100_000);
        let usage = assistant.usage;
        manager
            .append_message(pi_ai::types::Message::Assistant(assistant).into())
            .unwrap();
        manager
            .append_usage("cache_warm", &model.provider, &model.id, usage, None)
            .unwrap();
    }))
    .await;

    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let status = session.cache_warming_status().unwrap();
    assert_eq!(status.state, CacheWarmingRunState::Inactive);
    assert_eq!(status.reason.as_deref(), Some("waiting for first request"));

    dispose();
}

#[tokio::test]
async fn forwards_http_idle_timeout_ms_as_timeout_ms_for_openai_codex() {
    let options = capture_stream_options(
        "openai-codex-responses",
        Settings {
            http_idle_timeout_ms: Some(json!(1234)),
            ..Default::default()
        },
        SimpleStreamOptions::default(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(options.base.timeout_ms, Some(1234));
}

#[tokio::test]
async fn defaults_timeout_ms_from_http_idle_timeout_ms_for_all_providers() {
    let options = capture_stream_options(
        "openai-completions",
        Settings {
            http_idle_timeout_ms: Some(json!(1234)),
            ..Default::default()
        },
        SimpleStreamOptions::default(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(options.base.timeout_ms, Some(1234));
}

#[tokio::test]
async fn lets_request_timeout_ms_override_http_idle_timeout_ms_for_openai_codex() {
    let mut req = SimpleStreamOptions::default();
    req.base.timeout_ms = Some(0);
    let options = capture_stream_options(
        "openai-codex-responses",
        Settings {
            http_idle_timeout_ms: Some(json!(1234)),
            ..Default::default()
        },
        req,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(options.base.timeout_ms, Some(0));
}

#[tokio::test]
async fn forwards_websocket_connect_timeout_ms_from_settings() {
    let options = capture_stream_options(
        "openai-codex-responses",
        Settings {
            websocket_connect_timeout_ms: Some(json!(1234)),
            ..Default::default()
        },
        SimpleStreamOptions::default(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(options.base.websocket_connect_timeout_ms, Some(1234));
}

#[tokio::test]
async fn lets_request_websocket_connect_timeout_ms_override_settings() {
    let mut req = SimpleStreamOptions::default();
    req.base.websocket_connect_timeout_ms = Some(0);
    let options = capture_stream_options(
        "openai-codex-responses",
        Settings {
            websocket_connect_timeout_ms: Some(json!(1234)),
            ..Default::default()
        },
        req,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(options.base.websocket_connect_timeout_ms, Some(0));
}

#[tokio::test]
async fn forwards_provider_retry_settings() {
    let options = capture_stream_options(
        "openai-completions",
        Settings {
            retry: Some(RetrySettings {
                provider: Some(ProviderRetrySettings {
                    max_retries: Some(2),
                    max_retry_delay_ms: Some(3000),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        SimpleStreamOptions::default(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(options.base.max_retries, Some(2));
    assert_eq!(options.base.max_retry_delay_ms, Some(3000));
}

#[tokio::test]
async fn forwards_provider_stream_events_to_extensions() {
    let provider_event = json!({ "openrouter_metadata": { "strategy": "direct" } });
    let captured_events: Arc<Mutex<Vec<ProviderStreamEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_ref = Arc::clone(&captured_events);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let captured = Arc::clone(&captured_ref);
        async move {
            pi.on::<ProviderStreamEvent>(move |ev, _ctx| {
                let captured_inner = Arc::clone(&captured);
                Box::pin(async move {
                    captured_inner.lock().unwrap_or_else(PoisonError::into_inner).push(ev);
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let options = capture_stream_options(
        "openai-completions",
        Settings::default(),
        SimpleStreamOptions::default(),
        Some(ext),
        Some(provider_event.clone()),
    )
    .await
    .unwrap();

    assert!(options.base.on_provider_stream_event.is_some());
    assert_eq!(
        *captured_events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![ProviderStreamEvent {
            data: provider_event,
            provider: "capture-provider".to_string(),
            api: "openai-completions".to_string(),
            model: "capture-model".to_string(),
        }]
    );
}

#[tokio::test]
async fn runs_before_provider_headers_on_assembled_headers() {
    let mut req_headers: ProviderHeaders = IndexMap::new();
    req_headers.insert("x-explicit".to_string(), Some("explicit".to_string()));
    let mut req = SimpleStreamOptions::default();
    req.base.headers = Some(req_headers);

    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<BeforeProviderHeadersEvent>(|ev, _ctx| {
            Box::pin(async move {
                let mut h = ev.headers.lock().unwrap_or_else(PoisonError::into_inner);
                let val = [
                    h.get("x-provider").and_then(|v| v.as_deref()).unwrap_or(""),
                    h.get("x-model").and_then(|v| v.as_deref()).unwrap_or(""),
                    h.get("x-explicit").and_then(|v| v.as_deref()).unwrap_or(""),
                ]
                .join(":");
                h.insert("x-hook".to_string(), Some(val));
                Ok(None)
            })
        });
        Ok(())
    }));

    let options = capture_stream_options("openai-completions", Settings::default(), req, Some(ext), None)
        .await
        .unwrap();
    let headers = options.base.headers.unwrap();
    assert_eq!(headers.get("x-provider").and_then(|v| v.as_deref()), Some("provider"));
    assert_eq!(headers.get("x-model").and_then(|v| v.as_deref()), Some("model"));
    assert_eq!(headers.get("x-explicit").and_then(|v| v.as_deref()), Some("explicit"));
    assert_eq!(
        headers.get("x-hook").and_then(|v| v.as_deref()),
        Some("provider:model:explicit")
    );
}

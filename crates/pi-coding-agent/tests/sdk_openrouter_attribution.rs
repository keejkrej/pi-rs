//! Port of packages/coding-agent/test/sdk-openrouter-attribution.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Model, ModelCost, ModelInput, ProviderHeaders,
    SimpleStreamOptions, StopReason, TextContent, Usage,
};
use pi_ai::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{ApiKeyCredential, Credential};
use pi_coding_agent::core::auth_storage::{AuthStorage, AuthStorageData};
use pi_coding_agent::core::extensions::types::ProviderConfig;
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pretty_assertions::assert_eq;
use serial_test::serial;
use support::model_runtime_test_utils::{create_in_memory_model_registry, get_model_runtime};

fn make_model(provider: &str, base_url: &str, id: Option<&str>) -> Model {
    Model {
        r#type: None,
        id: id
            .map(str::to_string)
            .unwrap_or_else(|| format!("{provider}-test-model")),
        name: format!("{provider} Test Model"),
        api: "openai-completions".to_string(),
        provider: provider.to_string(),
        base_url: base_url.to_string(),
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
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn create_done_stream() -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let msg = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new("ok"))],
        api: "openai-completions".to_string(),
        provider: "capture-provider".to_string(),
        model: "capture-model".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    };
    stream.end(Some(msg));
    stream
}

#[derive(Default)]
struct CaptureOpts {
    telemetry_enabled: Option<bool>,
    provider_headers: Option<IndexMap<String, String>>,
    request_headers: Option<ProviderHeaders>,
    session_id: Option<String>,
}

async fn capture_headers(model: Model, opts: CaptureOpts) -> Option<ProviderHeaders> {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[
        &pi_js::env::tmp_dir(),
        "pi-sdk-attribution-test-",
    ]))
    .unwrap();
    let cwd = pi_js::path::join(&[&temp_dir, "project"]);
    let agent_dir = pi_js::path::join(&[&temp_dir, "agent"]);
    pi_js::fs::mkdir(&cwd, true, None).unwrap();
    pi_js::fs::mkdir(&agent_dir, true, None).unwrap();

    let settings_manager = SettingsManager::create(&cwd, Some(&agent_dir), None);
    if opts.telemetry_enabled == Some(false) {
        settings_manager.set_enable_install_telemetry(false);
    }

    let mut auth_data: AuthStorageData = IndexMap::new();
    auth_data.insert(
        model.provider.clone(),
        Credential::ApiKey(ApiKeyCredential {
            key: Some("test-api-key".to_string()),
            env: None,
        }),
    );
    let auth_storage = Arc::new(AuthStorage::in_memory(Some(auth_data)));
    let model_registry = create_in_memory_model_registry(auth_storage).await.unwrap();
    let captured_options: Arc<Mutex<Option<SimpleStreamOptions>>> = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured_options);

    model_registry
        .register_provider(
            &model.provider,
            ProviderConfig {
                api: Some(model.api.clone()),
                headers: opts.provider_headers,
                stream_simple: Some(Arc::new(move |_m, _ctx, provider_opts| {
                    *captured_ref.lock().unwrap_or_else(PoisonError::into_inner) = provider_opts;
                    create_done_stream()
                })),
                ..Default::default()
            }
            .into(),
        )
        .unwrap();

    let model_runtime = get_model_runtime(&model_registry);
    let session_manager = SessionManager::in_memory(Some(&cwd), None, None).unwrap();
    if let Some(ref sid) = opts.session_id {
        session_manager
            .new_session(Some(pi_coding_agent::core::session_manager::NewSessionOptions {
                id: Some(sid.clone()),
                ..Default::default()
            }))
            .unwrap();
    }

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(cwd),
        agent_dir: Some(agent_dir),
        model: Some(model.clone()),
        model_runtime: Some(model_runtime),
        settings_manager: Some(settings_manager),
        session_manager: Some(session_manager),
        ..Default::default()
    }))
    .await
    .unwrap();

    let session = created.session;
    let mut stream_opts = SimpleStreamOptions::default();
    stream_opts.base.session_id = Some(session.session_id());
    if let Some(req_headers) = opts.request_headers {
        stream_opts.base.headers = Some(req_headers);
    }
    let stream_fn = session.agent().stream_fn().unwrap();
    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![],
        tools: None,
    });
    let _ = ctx;
    let stream = stream_fn(
        model.clone(),
        Context {
            system_prompt: None,
            messages: vec![],
            tools: None,
        },
        Some(stream_opts),
    )
    .await
    .unwrap();
    let _ = stream.result().await;
    let result = captured_options
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|o| o.base.headers.clone());

    session.dispose();
    model_registry.unregister_provider(&model.provider);
    let _ = pi_js::fs::rm(&temp_dir, true, true);
    result
}

#[tokio::test]
#[serial]
async fn adds_default_attribution_headers_for_openrouter_models() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let headers = capture_headers(
        make_model("openrouter", "https://openrouter.ai/api/v1", None),
        CaptureOpts::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        headers.get("HTTP-Referer").and_then(|v| v.as_deref()),
        Some("https://pi.dev")
    );
    assert_eq!(headers.get("X-OpenRouter-Title").and_then(|v| v.as_deref()), Some("pi"));
    assert_eq!(
        headers.get("X-OpenRouter-Categories").and_then(|v| v.as_deref()),
        Some("cli-agent")
    );
}

#[tokio::test]
#[serial]
async fn does_not_add_attribution_headers_when_telemetry_is_disabled() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let headers = capture_headers(
        make_model("openrouter", "https://openrouter.ai/api/v1", None),
        CaptureOpts {
            telemetry_enabled: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_default();
    assert_eq!(headers.get("HTTP-Referer").and_then(|v| v.as_deref()), None);
    assert_eq!(headers.get("X-OpenRouter-Title").and_then(|v| v.as_deref()), None);
    assert_eq!(headers.get("X-OpenRouter-Categories").and_then(|v| v.as_deref()), None);
}

#[tokio::test]
#[serial]
async fn adds_attribution_headers_for_custom_providers_routed_through_openrouter() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let headers = capture_headers(
        make_model("custom-openrouter", "https://openrouter.ai/api/v1", None),
        CaptureOpts::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        headers.get("HTTP-Referer").and_then(|v| v.as_deref()),
        Some("https://pi.dev")
    );
    assert_eq!(headers.get("X-OpenRouter-Title").and_then(|v| v.as_deref()), Some("pi"));
    assert_eq!(
        headers.get("X-OpenRouter-Categories").and_then(|v| v.as_deref()),
        Some("cli-agent")
    );
}

#[tokio::test]
#[serial]
async fn preserves_legacy_openrouter_base_url_substring_attribution_matching() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let headers = capture_headers(
        make_model("custom-openrouter", "not-a-url-openrouter.ai", None),
        CaptureOpts::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        headers.get("HTTP-Referer").and_then(|v| v.as_deref()),
        Some("https://pi.dev")
    );
    assert_eq!(headers.get("X-OpenRouter-Title").and_then(|v| v.as_deref()), Some("pi"));
    assert_eq!(
        headers.get("X-OpenRouter-Categories").and_then(|v| v.as_deref()),
        Some("cli-agent")
    );
}

#[tokio::test]
#[serial]
async fn lets_provider_and_request_headers_override_the_defaults() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let mut provider_headers = IndexMap::new();
    provider_headers.insert("HTTP-Referer".to_string(), "https://provider.example".to_string());
    provider_headers.insert("X-OpenRouter-Categories".to_string(), "provider-category".to_string());
    let mut request_headers: ProviderHeaders = IndexMap::new();
    request_headers.insert("X-OpenRouter-Title".to_string(), Some("request-title".to_string()));

    let headers = capture_headers(
        make_model("openrouter", "https://openrouter.ai/api/v1", None),
        CaptureOpts {
            provider_headers: Some(provider_headers),
            request_headers: Some(request_headers),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        headers.get("HTTP-Referer").and_then(|v| v.as_deref()),
        Some("https://provider.example")
    );
    assert_eq!(
        headers.get("X-OpenRouter-Title").and_then(|v| v.as_deref()),
        Some("request-title")
    );
    assert_eq!(
        headers.get("X-OpenRouter-Categories").and_then(|v| v.as_deref()),
        Some("provider-category")
    );
}

#[tokio::test]
#[serial]
async fn adds_default_attribution_headers_for_direct_nvidia_nim_endpoints() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let headers = capture_headers(
        make_model("custom-nim", "https://integrate.api.nvidia.com/v1", None),
        CaptureOpts::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        headers.get("X-BILLING-INVOKE-ORIGIN").and_then(|v| v.as_deref()),
        Some("Pi")
    );
}

#[tokio::test]
#[serial]
async fn adds_default_attribution_headers_for_the_nvidia_provider() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let headers = capture_headers(
        make_model("nvidia", "https://example.test/v1", None),
        CaptureOpts::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        headers.get("X-BILLING-INVOKE-ORIGIN").and_then(|v| v.as_deref()),
        Some("Pi")
    );
}

#[tokio::test]
#[serial]
async fn does_not_add_nvidia_nim_attribution_headers_when_telemetry_is_disabled() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let headers = capture_headers(
        make_model("nvidia", "https://integrate.api.nvidia.com/v1", None),
        CaptureOpts {
            telemetry_enabled: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_default();
    assert_eq!(headers.get("X-BILLING-INVOKE-ORIGIN").and_then(|v| v.as_deref()), None);
}

#[tokio::test]
#[serial]
async fn lets_provider_and_request_headers_override_nvidia_nim_defaults() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let mut provider_headers = IndexMap::new();
    provider_headers.insert("X-BILLING-INVOKE-ORIGIN".to_string(), "Provider".to_string());
    let mut request_headers: ProviderHeaders = IndexMap::new();
    request_headers.insert("X-BILLING-INVOKE-ORIGIN".to_string(), Some("Request".to_string()));

    let headers = capture_headers(
        make_model("nvidia", "https://integrate.api.nvidia.com/v1", None),
        CaptureOpts {
            provider_headers: Some(provider_headers),
            request_headers: Some(request_headers),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        headers.get("X-BILLING-INVOKE-ORIGIN").and_then(|v| v.as_deref()),
        Some("Request")
    );
}

#[tokio::test]
#[serial]
async fn does_not_add_nvidia_nim_attribution_headers_for_nvidia_models_routed_through_openrouter() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let headers = capture_headers(
        make_model(
            "openrouter",
            "https://openrouter.ai/api/v1",
            Some("nvidia/nemotron-3-super-120b-a12b"),
        ),
        CaptureOpts::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        headers.get("HTTP-Referer").and_then(|v| v.as_deref()),
        Some("https://pi.dev")
    );
    assert_eq!(headers.get("X-BILLING-INVOKE-ORIGIN").and_then(|v| v.as_deref()), None);
}

#[tokio::test]
#[serial]
async fn does_not_add_nvidia_nim_attribution_headers_for_nvidia_models_routed_through_vercel_ai_gateway() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let headers = capture_headers(
        make_model(
            "vercel-ai-gateway",
            "https://ai-gateway.vercel.sh/v1",
            Some("nvidia/nemotron-3-super-120b-a12b"),
        ),
        CaptureOpts::default(),
    )
    .await
    .unwrap_or_default();
    assert_eq!(headers.get("X-BILLING-INVOKE-ORIGIN").and_then(|v| v.as_deref()), None);
}

#[tokio::test]
#[serial]
async fn adds_opencode_session_headers() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let headers = capture_headers(
        make_model("opencode", "https://opencode.ai/zen/v1", None),
        CaptureOpts {
            session_id: Some("opencode-session".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        headers.get("x-opencode-session").and_then(|v| v.as_deref()),
        Some("opencode-session")
    );
    assert_eq!(headers.get("x-opencode-client").and_then(|v| v.as_deref()), Some("pi"));
}

#[tokio::test]
#[serial]
async fn lets_configured_opencode_headers_override_the_defaults() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let mut provider_headers = IndexMap::new();
    provider_headers.insert("x-opencode-session".to_string(), "configured-session".to_string());
    provider_headers.insert("x-opencode-client".to_string(), "configured-client".to_string());

    let headers = capture_headers(
        make_model("opencode", "https://opencode.ai/zen/v1", None),
        CaptureOpts {
            session_id: Some("opencode-session".to_string()),
            provider_headers: Some(provider_headers),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        headers.get("x-opencode-session").and_then(|v| v.as_deref()),
        Some("configured-session")
    );
    assert_eq!(
        headers.get("x-opencode-client").and_then(|v| v.as_deref()),
        Some("configured-client")
    );
}

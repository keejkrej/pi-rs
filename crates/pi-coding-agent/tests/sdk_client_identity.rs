//! Port of packages/coding-agent/test/sdk-client-identity.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Model, ModelCost, SimpleStreamOptions, StopReason, TextContent,
    Usage,
};
use pi_ai::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use pi_ai::utils::identity::{
    ClientIdentity, ClientIdentityTarget, UserAgentMode, get_attribution_headers, pi_attribution,
};
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{ApiKeyCredential, Credential, ModelInput};
use pi_coding_agent::config::VERSION;
use pi_coding_agent::core::auth_storage::{AuthStorage, AuthStorageData};
use pi_coding_agent::core::extensions::types::ProviderConfig;
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::{NewSessionOptions, SessionManager};
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
        max_tokens: 4096,
        headers: None,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
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
        duration_ms: None,
        timestamp: 0,
    };
    stream.end(Some(msg));
    stream
}

#[derive(Default)]
struct CaptureOpts {
    telemetry_enabled: Option<bool>,
    provider_headers: Option<IndexMap<String, String>>,
    session_id: Option<String>,
}

async fn capture_options(model: Model, opts: CaptureOpts) -> Option<SimpleStreamOptions> {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-sdk-identity-test-"])).unwrap();
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
            .new_session(Some(NewSessionOptions {
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
    let result = captured_options.lock().unwrap_or_else(PoisonError::into_inner).clone();

    session.dispose();
    model_registry.unregister_provider(&model.provider);
    let _ = pi_js::fs::rm(&temp_dir, true, true);
    result
}

#[tokio::test]
#[serial]
async fn identifies_pi_with_detailed_user_agent_and_attribution_as_cli_agent() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let model = make_model("openrouter", "https://openrouter.ai/api/v1", None);
    let options = capture_options(model.clone(), CaptureOpts::default()).await.unwrap();

    assert_eq!(
        options.base.identity,
        Some(ClientIdentity {
            user_agent: Some(UserAgentMode::DETAILED),
            version: Some((*VERSION).to_owned()),
            attribution: Some(Some(pi_attribution())),
        })
    );
    let headers = get_attribution_headers(
        ClientIdentityTarget {
            provider: &model.provider,
            base_url: &model.base_url,
        },
        options.base.identity.as_ref(),
    );
    assert_eq!(
        headers,
        IndexMap::from([
            ("HTTP-Referer".to_owned(), "https://pi.dev".to_owned()),
            ("X-OpenRouter-Title".to_owned(), "pi".to_owned()),
            ("X-OpenRouter-Categories".to_owned(), "cli-agent".to_owned()),
        ])
    );
}

#[tokio::test]
#[serial]
async fn turns_attribution_off_when_telemetry_is_disabled() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let model = make_model("nvidia", "https://integrate.api.nvidia.com/v1", None);
    let options = capture_options(
        model.clone(),
        CaptureOpts {
            telemetry_enabled: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(
        options.base.identity,
        Some(ClientIdentity {
            user_agent: Some(UserAgentMode::DETAILED),
            version: Some((*VERSION).to_owned()),
            attribution: Some(None),
        })
    );
    assert!(
        get_attribution_headers(
            ClientIdentityTarget {
                provider: &model.provider,
                base_url: &model.base_url,
            },
            options.base.identity.as_ref()
        )
        .is_empty()
    );
}

#[tokio::test]
#[serial]
async fn turns_attribution_off_with_pi_telemetry_0() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", Some("0"));
    let model = make_model("openrouter", "https://openrouter.ai/api/v1", None);
    let options = capture_options(model, CaptureOpts::default()).await.unwrap();

    assert_eq!(options.base.identity.and_then(|id| id.attribution), Some(None));
}

#[tokio::test]
#[serial]
async fn adds_opencode_session_headers_for_custom_opencode_providers() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let model = make_model("custom-opencode", "https://opencode.ai/zen/v1", None);
    let options = capture_options(
        model,
        CaptureOpts {
            session_id: Some("opencode-session".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(
        options
            .base
            .headers
            .as_ref()
            .and_then(|h| h.get("x-opencode-session"))
            .and_then(|v| v.as_deref()),
        Some("opencode-session")
    );
}

#[tokio::test]
#[serial]
async fn lets_configured_opencode_headers_override_the_session_header() {
    let _env = pi_js::env::testing::EnvGuard::set("PI_TELEMETRY", None);
    let model = make_model("opencode", "https://opencode.ai/zen/v1", None);
    let options = capture_options(
        model,
        CaptureOpts {
            session_id: Some("opencode-session".to_string()),
            provider_headers: Some(IndexMap::from([(
                "x-opencode-session".to_string(),
                "configured-session".to_string(),
            )])),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(
        options
            .base
            .headers
            .as_ref()
            .and_then(|h| h.get("x-opencode-session"))
            .and_then(|v| v.as_deref()),
        Some("configured-session")
    );
}

//! Port of packages/ai/test/providers.test.ts

use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use indexmap::IndexMap;
use tokio::sync::Notify;

use pi_js::abort::AbortController;
use pi_js::json::JsonValue;
use pi_js::{Error, Result};

use pi_ai::api::lazy::{LazyApiCapabilities, lazy_api};
use pi_ai::auth::helpers::env_api_key_auth;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthContext, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind,
    AuthResult, ModelAuth, ProviderAuth, ProviderAuthInteraction,
};
use pi_ai::compat::{get_model as get_compat_model, get_models as get_compat_models};
use pi_ai::models::{
    CreateModelsOptions, CreateProviderApi, CreateProviderOptions, ModelsApi, ModelsDeferredCancelOptions,
    ModelsDeferredFetchOptions, ModelsRefreshOptions, ModelsSimpleStreamOptions, create_models, create_provider,
    get_supported_thinking_levels,
};
use pi_ai::models_store::{InMemoryModelsStore, ModelsStore};
use pi_ai::providers::all::{
    builtin_models, builtin_providers, get_all_builtin_models, get_builtin_classifier_model,
    get_builtin_classifier_models, get_builtin_image_model, get_builtin_image_models, get_builtin_model,
    get_builtin_models, get_builtin_providers,
};
use pi_ai::providers::amazon_bedrock::amazon_bedrock_provider;
use pi_ai::providers::anthropic::anthropic_provider;
use pi_ai::providers::cloudflare_ai_gateway::cloudflare_ai_gateway_provider;
use pi_ai::providers::cloudflare_workers_ai::cloudflare_workers_ai_provider;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxDeferredOptions, FauxResponseStep, RegisterFauxProviderOptions,
    faux_assistant_message, faux_provider,
};
use pi_ai::providers::google_vertex::google_vertex_provider;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessageEvent, Context, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle,
    DeferredOption, DeferredWindow, Message, Model, ModelCompat, ModelCost, ModelImageInputLimits,
    ModelImageResizeOptions, ModelInputLimits, ModelModality, ModelThinkingLevel, ProviderEnv, ProviderHeaders,
    ProviderRequestOptions, ProviderStreams, SimpleStreamOptions, StopReason, StreamOptions, UserMessage,
    UserMessageContent,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_ai::utils::transcript::normalize_context;

struct FakeAuthContext {
    env: ProviderEnv,
    files: Vec<String>,
}

impl FakeAuthContext {
    fn new(env: &[(&str, &str)], files: &[&str]) -> Arc<dyn AuthContext> {
        let mut map = IndexMap::new();
        for &(k, v) in env {
            map.insert(k.to_owned(), v.to_owned());
        }
        Arc::new(Self {
            env: map,
            files: files.iter().map(|s| (*s).to_owned()).collect(),
        })
    }
}

#[async_trait]
impl AuthContext for FakeAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    async fn file_exists(&self, path: &str) -> bool {
        self.files.iter().any(|entry| entry == path)
    }
}

struct RecordingInteraction {
    answers: Mutex<VecDeque<String>>,
    events: Mutex<Vec<AuthEvent>>,
    expect_secret_prompt: bool,
}

impl RecordingInteraction {
    fn new(answers: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.iter().map(|s| (*s).to_owned()).collect()),
            events: Mutex::new(Vec::new()),
            expect_secret_prompt: false,
        })
    }

    fn secret(answers: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.iter().map(|s| (*s).to_owned()).collect()),
            events: Mutex::new(Vec::new()),
            expect_secret_prompt: true,
        })
    }

    fn events(&self) -> Vec<AuthEvent> {
        self.events.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[async_trait]
impl AuthInteraction for RecordingInteraction {
    async fn prompt(&self, prompt: AuthPrompt) -> Result<String> {
        if self.expect_secret_prompt {
            assert!(
                matches!(prompt.kind, AuthPromptKind::Secret { .. }),
                "expected secret prompt, got {:?}",
                prompt.kind
            );
        }
        self.answers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front()
            .ok_or_else(|| Error::msg("no more scripted answers"))
    }

    fn notify(&self, event: AuthEvent) {
        self.events.lock().unwrap_or_else(|p| p.into_inner()).push(event);
    }
}

fn user_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    }
}

const DEFAULT_IMAGE_RESIZE: ModelImageResizeOptions = ModelImageResizeOptions {
    max_width: Some(2000),
    max_height: Some(2000),
    max_bytes: Some(4_718_592), // 4.5 * 1024 * 1024
    jpeg_quality: Some(80.0),
};

fn compat_bool(compat: Option<&ModelCompat>, key: &str) -> Option<bool> {
    let compat = compat?;
    match (compat, key) {
        (ModelCompat::OpenAICompletions(c), "supportsStrictMode") => c.supports_strict_mode,
        (ModelCompat::OpenAIResponses(c), "supportsStrictMode") => c.supports_strict_mode,
        (ModelCompat::Bedrock(c), "supportsStrictMode") => c.supports_strict_mode,
        (ModelCompat::OpenAICompletions(c), "supportsOpenAIGrammarTools") => c.supports_open_ai_grammar_tools,
        (ModelCompat::OpenAIResponses(c), "supportsOpenAIGrammarTools") => c.supports_open_ai_grammar_tools,
        (ModelCompat::AnthropicMessages(c), "supportsStrictTools") => c.supports_strict_tools,
        (ModelCompat::OpenAICompletions(c), "supportsMidConvoSystemMessages") => c.supports_mid_convo_system_messages,
        (ModelCompat::OpenAIResponses(c), "supportsMidConvoSystemMessages") => c.supports_mid_convo_system_messages,
        (ModelCompat::AnthropicMessages(c), "supportsMidConvoSystemMessages") => c.supports_mid_convo_system_messages,
        (ModelCompat::MistralConversations(c), "supportsMidConvoSystemMessages") => {
            c.supports_mid_convo_system_messages
        }
        (ModelCompat::OpenAIResponses(c), "supportsAdditionalTools") => c.supports_additional_tools,
        (ModelCompat::OpenAIResponses(c), "supportsToolSearch") => c.supports_tool_search,
        (ModelCompat::AnthropicMessages(c), "supportsMidConvoToolChanges") => c.supports_mid_convo_tool_changes,
        (ModelCompat::OpenAICompletions(c), "supportsMidConvoToolAdditions") => c.supports_mid_convo_tool_additions,
        (ModelCompat::Other(JsonValue::Object(obj)), k) => obj.get(k).and_then(|v| v.as_bool()),
        _ => None,
    }
}

fn header_entry(key: &str, value: Option<&str>) -> (String, Option<String>) {
    (key.to_owned(), value.map(str::to_owned))
}

#[test]
fn builtin_models_registers_every_builtin_provider_with_models() {
    let models = builtin_models(None);
    let providers = models.get_providers();
    assert_eq!(providers.len(), builtin_providers().len());
    assert!(providers.iter().any(|p| p.id() == "anthropic"));

    let anthropic = models
        .get_model("anthropic", "claude-haiku-4-5")
        .expect("anthropic/claude-haiku-4-5 exists");
    assert_eq!(anthropic.api, "anthropic-messages");

    let all = models.get_models(None);
    assert!(all.len() > 500, "expected > 500 builtin models, got {}", all.len());

    for provider in &providers {
        let list = models.get_all_models(Some(provider.id()));
        assert!(!list.is_empty(), "provider {} has no models", provider.id());
        assert!(
            list.iter().all(|m| m.provider() == provider.id()),
            "provider {} contains mismatched model provider",
            provider.id()
        );
    }

    let radius_balanced = get_builtin_model("radius", "balanced").expect("radius/balanced exists");
    assert_eq!(radius_balanced.api, "pi-messages");
    assert_eq!(radius_balanced.provider, "radius");
}

#[test]
fn returns_empty_results_for_unknown_provider_ids() {
    let unknown_provider = "not-a-provider";
    let unknown_model = "x";

    assert!(get_builtin_model(unknown_provider, unknown_model).is_none());
    assert!(get_builtin_image_model(unknown_provider, unknown_model).is_none());
    assert!(get_builtin_classifier_model(unknown_provider, unknown_model).is_none());
    assert!(get_builtin_models(unknown_provider).is_empty());
    assert!(get_builtin_image_models(unknown_provider).is_empty());
    assert!(get_builtin_classifier_models(unknown_provider).is_empty());
    assert!(get_all_builtin_models(unknown_provider).is_empty());
    assert!(get_compat_model(unknown_provider, unknown_model).is_none());
    assert!(get_compat_models(unknown_provider).is_empty());
}

#[test]
fn stores_native_constrained_sampling_capabilities_in_model_metadata() {
    let gpt4o = get_builtin_model("openai", "gpt-4o").expect("openai/gpt-4o exists");
    assert_eq!(compat_bool(gpt4o.compat.as_ref(), "supportsStrictMode"), Some(true));
    assert_eq!(compat_bool(gpt4o.compat.as_ref(), "supportsOpenAIGrammarTools"), None);

    let gpt54 = get_builtin_model("openai", "gpt-5.4").expect("openai/gpt-5.4 exists");
    assert_eq!(compat_bool(gpt54.compat.as_ref(), "supportsStrictMode"), Some(true));
    assert_eq!(
        compat_bool(gpt54.compat.as_ref(), "supportsOpenAIGrammarTools"),
        Some(true)
    );

    let haiku = get_builtin_model("anthropic", "claude-haiku-4-5").expect("anthropic/claude-haiku-4-5 exists");
    assert_eq!(compat_bool(haiku.compat.as_ref(), "supportsStrictTools"), Some(true));
}

#[test]
fn keeps_the_conservative_resize_profile_on_every_vision_model() {
    let vision_models: Vec<Model> = get_builtin_providers()
        .into_iter()
        .flat_map(get_builtin_models)
        .filter(|model| model.input.contains(&ModelModality::Image))
        .collect();
    assert!(!vision_models.is_empty());
    for model in &vision_models {
        let resize = model
            .input_limits
            .as_ref()
            .and_then(|limits| limits.images.as_ref())
            .and_then(|images| images.resize);
        assert_eq!(
            resize,
            Some(DEFAULT_IMAGE_RESIZE),
            "model {}/{} missing DEFAULT_IMAGE_RESIZE",
            model.provider,
            model.id
        );
    }
}

#[test]
fn records_known_direct_provider_image_request_limits() {
    let haiku = get_builtin_model("anthropic", "claude-haiku-4-5").expect("haiku");
    let haiku_limits = haiku.input_limits.expect("haiku limits");
    assert_eq!(haiku_limits.max_request_bytes, Some(32 * 1024 * 1024));
    assert_eq!(haiku_limits.images.and_then(|i| i.max_per_request), Some(100));

    let opus5 = get_builtin_model("anthropic", "claude-opus-5").expect("opus5");
    assert_eq!(
        opus5
            .input_limits
            .and_then(|l| l.images)
            .and_then(|i| i.max_per_request),
        Some(600)
    );

    let bedrock_haiku =
        get_builtin_model("amazon-bedrock", "anthropic.claude-haiku-4-5-20251001-v1:0").expect("bedrock haiku");
    assert_eq!(
        bedrock_haiku
            .input_limits
            .and_then(|l| l.images)
            .and_then(|i| i.max_per_message),
        Some(20)
    );

    let gpt4o = get_builtin_model("openai", "gpt-4o").expect("gpt4o");
    let gpt4o_limits = gpt4o.input_limits.expect("gpt4o limits");
    assert_eq!(gpt4o_limits.max_request_bytes, Some(512 * 1024 * 1024));
    assert_eq!(gpt4o_limits.images.and_then(|i| i.max_per_request), Some(1500));

    let flash = get_builtin_model("google", "gemini-2.5-flash").expect("flash");
    let flash_limits = flash.input_limits.expect("flash limits");
    assert_eq!(flash_limits.max_request_bytes, Some(20 * 1024 * 1024));
    assert_eq!(flash_limits.images.and_then(|i| i.max_per_request), Some(3600));
}

#[test]
fn does_not_infer_image_limits_from_gateway_api_compatibility() {
    let openrouter_vision = get_builtin_models("openrouter")
        .into_iter()
        .find(|model| model.input.contains(&ModelModality::Image))
        .expect("openrouter vision model exists");
    assert_eq!(
        openrouter_vision.input_limits,
        Some(ModelInputLimits {
            max_request_bytes: None,
            images: Some(ModelImageInputLimits {
                resize: Some(DEFAULT_IMAGE_RESIZE),
                max_per_message: None,
                max_per_request: None,
            }),
        })
    );
}

#[test]
fn uses_models_dev_effort_levels_for_google_thinking_models() {
    // Regression test for https://github.com/earendil-works/pi/issues/9455
    for provider in ["google", "google-vertex"] {
        let m36 = get_builtin_model(provider, "gemini-3.6-flash").expect("gemini-3.6-flash");
        assert!(get_supported_thinking_levels(&m36).contains(&ModelThinkingLevel::Minimal));

        let m38 = get_builtin_model(provider, "gemini-3.8-flash").expect("gemini-3.8-flash");
        assert_eq!(
            get_supported_thinking_levels(&m38),
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
            ]
        );

        let m31 = get_builtin_model(provider, "gemini-3.1-pro-preview").expect("gemini-3.1-pro-preview");
        assert_eq!(
            get_supported_thinking_levels(&m31),
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
            ]
        );
    }

    let opencode_38 = get_builtin_model("opencode", "gemini-3.8-flash").expect("opencode/gemini-3.8-flash");
    assert_eq!(
        get_supported_thinking_levels(&opencode_38),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
        ]
    );

    let gemma = get_builtin_model("google", "gemma-4-31b-it").expect("google/gemma-4-31b-it");
    assert_eq!(
        get_supported_thinking_levels(&gemma),
        vec![ModelThinkingLevel::Minimal, ModelThinkingLevel::High]
    );
}

#[test]
fn enables_mid_conversation_system_messages_only_for_verified_models() {
    let models = builtin_models(None);
    let supported = [
        ("moonshotai", "kimi-k2.6"),
        ("moonshotai", "kimi-k2.7-code"),
        ("moonshotai", "kimi-k2.7-code-highspeed"),
        ("moonshotai", "kimi-k3"),
        ("moonshotai-cn", "kimi-k2.6"),
        ("moonshotai-cn", "kimi-k2.7-code"),
        ("moonshotai-cn", "kimi-k2.7-code-highspeed"),
        ("moonshotai-cn", "kimi-k3"),
        ("fireworks", "accounts/fireworks/models/kimi-k3"),
        ("fireworks", "accounts/fireworks/routers/kimi-k3-fast"),
        ("openai", "gpt-5.4"),
        ("openai", "gpt-5.5"),
        ("openai", "gpt-6-astra"),
        ("openai-codex", "gpt-5.5"),
        ("anthropic", "claude-opus-5"),
        ("opencode", "gpt-5.4"),
        ("opencode", "gpt-5.6-terra"),
        ("opencode-go", "gpt-5.6-luna"),
        ("opencode", "claude-opus-4-8"),
        ("opencode", "claude-opus-5"),
        ("opencode", "kimi-k3"),
        ("opencode-go", "kimi-k3"),
        ("github-copilot", "gpt-5.6-terra"),
        ("github-copilot", "claude-opus-5"),
        ("github-copilot", "claude-opus-4.8"),
        ("github-copilot", "kimi-k3"),
        ("deepseek", "deepseek-v4-pro"),
        ("openrouter", "openai/gpt-5.6-terra"),
    ];
    let unsupported = [
        ("fireworks", "accounts/fireworks/models/nemotron-3-ultra-nvfp4"),
        ("openai", "gpt-4.1"),
        ("openai", "gpt-5.2"),
        ("anthropic", "claude-sonnet-4-5"),
        ("google", "gemini-2.5-pro"),
        ("opencode", "gpt-5.2"),
        ("opencode", "claude-sonnet-4-5"),
        ("github-copilot", "claude-sonnet-4.6"),
        ("deepseek", "deepseek-flash"),
        ("openrouter", "anthropic/claude-opus-5"),
        ("openrouter", "moonshotai/kimi-k3"),
        ("openrouter", "openai/gpt-5.6-terra:batch"),
    ];

    for (provider, model_id) in supported {
        let model = models
            .get_model(provider, model_id)
            .unwrap_or_else(|| panic!("missing supported model {provider}/{model_id}"));
        assert_eq!(
            compat_bool(model.compat.as_ref(), "supportsMidConvoSystemMessages"),
            Some(true),
            "{provider}/{model_id} should have supportsMidConvoSystemMessages: true"
        );
    }

    for (provider, model_id) in unsupported {
        let model = models
            .get_model(provider, model_id)
            .unwrap_or_else(|| panic!("missing unsupported model {provider}/{model_id}"));
        assert_eq!(
            compat_bool(model.compat.as_ref(), "supportsMidConvoSystemMessages"),
            None,
            "{provider}/{model_id} should not have supportsMidConvoSystemMessages"
        );
    }
}

#[test]
fn routes_proxied_tool_changes_through_verified_transports_only() {
    let models = builtin_models(None);
    for (provider, model_id) in [("opencode", "gpt-5.6-terra"), ("github-copilot", "gpt-5.6-terra")] {
        // Proxies pass `additional_tools` through to OpenAI but are not verified for tool search.
        let model = models.get_model(provider, model_id).expect("proxy model exists");
        assert_eq!(
            compat_bool(model.compat.as_ref(), "supportsAdditionalTools"),
            Some(true),
            "{provider}/{model_id}"
        );
        assert_eq!(
            compat_bool(model.compat.as_ref(), "supportsToolSearch"),
            None,
            "{provider}/{model_id}"
        );
    }

    // Proxied Anthropic endpoints reject `tool_addition`/`tool_removal` blocks.
    for provider in ["opencode", "github-copilot"] {
        let model = models.get_model(provider, "claude-opus-5").expect("claude-opus-5");
        assert_eq!(
            compat_bool(model.compat.as_ref(), "supportsMidConvoToolChanges"),
            None,
            "{provider}"
        );
    }
    let anthropic_opus5 = models
        .get_model("anthropic", "claude-opus-5")
        .expect("anthropic/claude-opus-5");
    assert_eq!(
        compat_bool(anthropic_opus5.compat.as_ref(), "supportsMidConvoToolChanges"),
        Some(true)
    );

    // Kimi-style tool-bearing system messages survive Moonshot and OpenCode but not Copilot.
    for provider in ["moonshotai", "moonshotai-cn", "opencode", "opencode-go"] {
        let kimi = models.get_model(provider, "kimi-k3").expect("kimi-k3");
        assert_eq!(
            compat_bool(kimi.compat.as_ref(), "supportsMidConvoToolAdditions"),
            Some(true),
            "{provider}"
        );
    }
    for provider in ["moonshotai", "moonshotai-cn"] {
        for model_id in ["kimi-k2.6", "kimi-k2.7-code", "kimi-k2.7-code-highspeed"] {
            let model = models.get_model(provider, model_id).expect("kimi variant");
            assert_eq!(
                compat_bool(model.compat.as_ref(), "supportsMidConvoToolAdditions"),
                None,
                "{provider}/{model_id}"
            );
        }
    }
    let copilot_kimi = models
        .get_model("github-copilot", "kimi-k3")
        .expect("github-copilot/kimi-k3");
    assert_eq!(
        compat_bool(copilot_kimi.compat.as_ref(), "supportsMidConvoToolAdditions"),
        None
    );
    let openrouter_gpt = models
        .get_model("openrouter", "openai/gpt-5.6-terra")
        .expect("openrouter/openai/gpt-5.6-terra");
    assert_eq!(
        compat_bool(openrouter_gpt.compat.as_ref(), "supportsMidConvoToolAdditions"),
        None
    );
}

#[test]
fn uses_official_kimi_k3_pricing_for_moonshot_providers() {
    let models = builtin_models(None);
    for provider in ["moonshotai", "moonshotai-cn"] {
        let model = models.get_model(provider, "kimi-k3").expect("kimi-k3");
        assert_eq!(
            model.cost,
            ModelCost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 0.0,
                tiers: None,
            }
        );
    }
}

#[test]
fn uses_api_equivalent_implied_pricing_for_kimi_coding_subscription_models() {
    let models = builtin_models(None);
    let expected_costs = [
        (
            "k3",
            ModelCost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 0.0,
                tiers: None,
            },
        ),
        (
            "kimi-for-coding-highspeed",
            ModelCost {
                input: 1.9,
                output: 8.0,
                cache_read: 0.38,
                cache_write: 0.0,
                tiers: None,
            },
        ),
    ];

    for (model_id, cost) in expected_costs {
        let model = models
            .get_model("kimi-coding", model_id)
            .unwrap_or_else(|| panic!("missing kimi-coding/{model_id}"));
        assert_eq!(model.cost, cost);
    }
}

#[tokio::test]
async fn resolves_anthropic_bearer_auth_from_env_with_auth_token_precedence() {
    let models = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: None,
        auth_context: Some(FakeAuthContext::new(
            &[
                ("ANTHROPIC_AUTH_TOKEN", "auth-token"),
                ("ANTHROPIC_OAUTH_TOKEN", "oauth-token"),
                ("ANTHROPIC_API_KEY", "api-key"),
            ],
            &[],
        )),
    }));
    models.set_provider(anthropic_provider());

    let result = models
        .get_auth_provider_id("anthropic", None)
        .await
        .expect("get_auth ok")
        .expect("anthropic auth configured");
    assert_eq!(
        result.auth,
        ModelAuth {
            api_key: None,
            headers: Some(IndexMap::from([header_entry(
                "Authorization",
                Some("Bearer auth-token")
            )])),
            base_url: None,
        }
    );
    assert_eq!(result.source.as_deref(), Some("ANTHROPIC_AUTH_TOKEN"));
}

#[tokio::test]
async fn preserves_anthropic_oauth_token_precedence_over_the_api_key() {
    let models = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: None,
        auth_context: Some(FakeAuthContext::new(
            &[("ANTHROPIC_API_KEY", "key"), ("ANTHROPIC_OAUTH_TOKEN", "oauth-token")],
            &[],
        )),
    }));
    models.set_provider(anthropic_provider());

    let result = models
        .get_auth_provider_id("anthropic", None)
        .await
        .expect("get_auth ok")
        .expect("anthropic auth configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("oauth-token"));
    assert_eq!(result.source.as_deref(), Some("ANTHROPIC_OAUTH_TOKEN"));
}

#[tokio::test]
async fn runs_provider_owned_bedrock_bearer_token_and_aws_profile_login_flows() {
    let bedrock = amazon_bedrock_provider();
    let auth = bedrock.auth().api_key.as_ref().expect("bedrock api_key auth");
    let signal = AbortController::new().signal();

    let bearer_interaction = RecordingInteraction::new(&["bearer-token", "bedrock-token"]);
    let bearer_cred = auth
        .login(ProviderAuthInteraction::new(bearer_interaction, signal.clone()))
        .await
        .expect("bearer login ok");
    assert_eq!(
        bearer_cred,
        ApiKeyCredential {
            key: Some("bedrock-token".to_owned()),
            env: None,
        }
    );

    let profile_interaction = RecordingInteraction::new(&["aws-profile", "work"]);
    let profile_cred = auth
        .login(ProviderAuthInteraction::new(
            profile_interaction.clone(),
            signal.clone(),
        ))
        .await
        .expect("profile login ok");
    assert_eq!(
        profile_cred,
        ApiKeyCredential {
            key: None,
            env: Some(IndexMap::from([("AWS_PROFILE".to_owned(), "work".to_owned())])),
        }
    );

    let events = profile_interaction.events();
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                AuthEvent::Info { links: Some(links), .. }
                    if links.iter().any(|l| l.label.as_deref() == Some("AWS credential provider chain"))
            )
        }),
        "expected Info event with AWS credential provider chain link, got {events:?}"
    );

    let resolved = auth
        .resolve(ApiKeyAuthInput {
            ctx: FakeAuthContext::new(&[], &[]),
            credential: Some(ApiKeyCredential {
                key: None,
                env: Some(IndexMap::from([("AWS_PROFILE".to_owned(), "work".to_owned())])),
            }),
            signal,
        })
        .await
        .expect("resolve ok")
        .expect("resolve Some");
    assert_eq!(resolved.auth, ModelAuth::default());
    assert_eq!(
        resolved.env,
        Some(IndexMap::from([("AWS_PROFILE".to_owned(), "work".to_owned())]))
    );
}

#[tokio::test]
async fn reports_bedrock_as_configured_from_ambient_aws_credentials_without_an_api_key() {
    let models = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: None,
        auth_context: Some(FakeAuthContext::new(&[("AWS_PROFILE", "dev")], &[])),
    }));
    models.set_provider(amazon_bedrock_provider());
    let model = &models.get_models(Some("amazon-bedrock"))[0];

    let result = models
        .get_auth_provider_id(&model.provider, None)
        .await
        .expect("get_auth ok")
        .expect("configured");
    assert_eq!(result.auth, ModelAuth::default());
    assert_eq!(result.source.as_deref(), Some("AWS_PROFILE"));

    let unconfigured = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: None,
        auth_context: Some(FakeAuthContext::new(&[], &[])),
    }));
    unconfigured.set_provider(amazon_bedrock_provider());
    assert!(
        unconfigured
            .get_auth_provider_id(&model.provider, None)
            .await
            .expect("get_auth ok")
            .is_none()
    );
}

#[tokio::test]
async fn requires_cloudflare_workers_ai_account_config_and_returns_scoped_env() {
    let missing_account = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: None,
        auth_context: Some(FakeAuthContext::new(&[("CLOUDFLARE_API_KEY", "cf-key")], &[])),
    }));
    missing_account.set_provider(cloudflare_workers_ai_provider());
    let model = &missing_account.get_models(Some("cloudflare-workers-ai"))[0];
    assert!(
        missing_account
            .get_auth_provider_id(&model.provider, None)
            .await
            .expect("get_auth ok")
            .is_none()
    );

    let configured = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: None,
        auth_context: Some(FakeAuthContext::new(
            &[
                ("CLOUDFLARE_API_KEY", "cf-key"),
                ("CLOUDFLARE_ACCOUNT_ID", "account-id"),
            ],
            &[],
        )),
    }));
    configured.set_provider(cloudflare_workers_ai_provider());
    let result = configured
        .get_auth_provider_id(&model.provider, None)
        .await
        .expect("get_auth ok")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("cf-key"));
    assert_eq!(result.auth.headers, None);
    assert_eq!(
        result.env,
        Some(IndexMap::from([(
            "CLOUDFLARE_ACCOUNT_ID".to_owned(),
            "account-id".to_owned(),
        )]))
    );
}

#[tokio::test]
async fn requires_cloudflare_ai_gateway_account_and_gateway_config_and_returns_scoped_env_headers() {
    let missing_gateway = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: None,
        auth_context: Some(FakeAuthContext::new(
            &[
                ("CLOUDFLARE_API_KEY", "cf-key"),
                ("CLOUDFLARE_ACCOUNT_ID", "account-id"),
            ],
            &[],
        )),
    }));
    missing_gateway.set_provider(cloudflare_ai_gateway_provider());
    let model = &missing_gateway.get_models(Some("cloudflare-ai-gateway"))[0];
    assert!(
        missing_gateway
            .get_auth_provider_id(&model.provider, None)
            .await
            .expect("get_auth ok")
            .is_none()
    );

    let configured = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: None,
        auth_context: Some(FakeAuthContext::new(
            &[
                ("CLOUDFLARE_API_KEY", "cf-key"),
                ("CLOUDFLARE_ACCOUNT_ID", "account-id"),
                ("CLOUDFLARE_GATEWAY_ID", "gateway-id"),
            ],
            &[],
        )),
    }));
    configured.set_provider(cloudflare_ai_gateway_provider());
    let result = configured
        .get_auth_provider_id(&model.provider, None)
        .await
        .expect("get_auth ok")
        .expect("configured");
    assert_eq!(
        result.auth,
        ModelAuth {
            api_key: None,
            headers: Some(IndexMap::from([
                header_entry("cf-aig-authorization", Some("Bearer cf-key")),
                header_entry("Authorization", None),
                header_entry("x-api-key", None),
            ])),
            base_url: None,
        }
    );
    assert_eq!(
        result.env,
        Some(IndexMap::from([
            ("CLOUDFLARE_ACCOUNT_ID".to_owned(), "account-id".to_owned()),
            ("CLOUDFLARE_GATEWAY_ID".to_owned(), "gateway-id".to_owned()),
        ]))
    );
}

#[tokio::test]
async fn runs_provider_owned_vertex_api_key_and_adc_login_flows() {
    let vertex = google_vertex_provider();
    let auth = vertex.auth().api_key.as_ref().expect("vertex api_key auth");
    let signal = AbortController::new().signal();

    let key_interaction = RecordingInteraction::new(&["api-key", "vertex-key"]);
    let key_cred = auth
        .login(ProviderAuthInteraction::new(key_interaction, signal.clone()))
        .await
        .expect("vertex key login ok");
    assert_eq!(
        key_cred,
        ApiKeyCredential {
            key: Some("vertex-key".to_owned()),
            env: None,
        }
    );

    let adc_interaction = RecordingInteraction::new(&["adc", "project-id", "us-central1"]);
    let adc_cred = auth
        .login(ProviderAuthInteraction::new(adc_interaction.clone(), signal.clone()))
        .await
        .expect("adc login ok");
    assert_eq!(
        adc_cred,
        ApiKeyCredential {
            key: None,
            env: Some(IndexMap::from([
                ("GOOGLE_CLOUD_PROJECT".to_owned(), "project-id".to_owned()),
                ("GOOGLE_CLOUD_LOCATION".to_owned(), "us-central1".to_owned()),
            ])),
        }
    );

    let events = adc_interaction.events();
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                AuthEvent::Info { links: Some(links), .. }
                    if links.iter().any(|l| l.label.as_deref() == Some("Application Default Credentials"))
            )
        }),
        "expected Info event with Application Default Credentials link, got {events:?}"
    );

    let resolved = auth
        .resolve(ApiKeyAuthInput {
            ctx: FakeAuthContext::new(&[], &["~/.config/gcloud/application_default_credentials.json"]),
            credential: Some(ApiKeyCredential {
                key: None,
                env: Some(IndexMap::from([
                    ("GOOGLE_CLOUD_PROJECT".to_owned(), "project-id".to_owned()),
                    ("GOOGLE_CLOUD_LOCATION".to_owned(), "us-central1".to_owned()),
                ])),
            }),
            signal,
        })
        .await
        .expect("resolve ok")
        .expect("resolve Some");
    assert_eq!(resolved.auth, ModelAuth::default());
    assert_eq!(
        resolved.env,
        Some(IndexMap::from([
            ("GOOGLE_CLOUD_PROJECT".to_owned(), "project-id".to_owned()),
            ("GOOGLE_CLOUD_LOCATION".to_owned(), "us-central1".to_owned()),
        ]))
    );
}

#[tokio::test]
async fn resolves_vertex_via_adc_file_plus_project_and_location() {
    let adc = "~/.config/gcloud/application_default_credentials.json";
    let configured = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: None,
        auth_context: Some(FakeAuthContext::new(
            &[
                ("GOOGLE_CLOUD_PROJECT", "proj"),
                ("GOOGLE_CLOUD_LOCATION", "us-central1"),
            ],
            &[adc],
        )),
    }));
    configured.set_provider(google_vertex_provider());
    let model = &configured.get_models(Some("google-vertex"))[0];

    let result = configured
        .get_auth_provider_id(&model.provider, None)
        .await
        .expect("get_auth ok")
        .expect("configured");
    assert_eq!(result.auth, ModelAuth::default());
    assert!(
        result
            .source
            .as_deref()
            .is_some_and(|s| s.contains("application default")),
        "expected source containing 'application default', got {:?}",
        result.source
    );

    // ADC without project/location is not configured
    let partial = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: None,
        auth_context: Some(FakeAuthContext::new(&[("GOOGLE_CLOUD_PROJECT", "proj")], &[adc])),
    }));
    partial.set_provider(google_vertex_provider());
    assert!(
        partial
            .get_auth_provider_id(&model.provider, None)
            .await
            .expect("get_auth ok")
            .is_none()
    );

    // explicit key wins over ADC
    let keyed = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: None,
        auth_context: Some(FakeAuthContext::new(&[("GOOGLE_CLOUD_API_KEY", "vertex-key")], &[])),
    }));
    keyed.set_provider(google_vertex_provider());
    let keyed_result = keyed
        .get_auth_provider_id(&model.provider, None)
        .await
        .expect("get_auth ok")
        .expect("configured");
    assert_eq!(keyed_result.auth.api_key.as_deref(), Some("vertex-key"));
}

#[tokio::test]
async fn env_api_key_auth_prefers_stored_credential_and_falls_back_through_env_vars() {
    let auth = env_api_key_auth("Test key", &["FIRST_KEY", "SECOND_KEY"]);
    let signal = AbortController::new().signal();

    let stored = auth
        .resolve(ApiKeyAuthInput {
            ctx: FakeAuthContext::new(&[("FIRST_KEY", "env")], &[]),
            credential: Some(ApiKeyCredential {
                key: Some("stored".to_owned()),
                env: None,
            }),
            signal: signal.clone(),
        })
        .await
        .expect("resolve ok")
        .expect("stored resolved");
    assert_eq!(stored.auth.api_key.as_deref(), Some("stored"));
    assert_eq!(stored.source.as_deref(), Some("stored credential"));

    let second = auth
        .resolve(ApiKeyAuthInput {
            ctx: FakeAuthContext::new(&[("SECOND_KEY", "second")], &[]),
            credential: None,
            signal: signal.clone(),
        })
        .await
        .expect("resolve ok")
        .expect("second resolved");
    assert_eq!(second.auth.api_key.as_deref(), Some("second"));
    assert_eq!(second.source.as_deref(), Some("SECOND_KEY"));

    assert!(
        auth.resolve(ApiKeyAuthInput {
            ctx: FakeAuthContext::new(&[], &[]),
            credential: None,
            signal,
        })
        .await
        .expect("resolve ok")
        .is_none()
    );
}

#[tokio::test]
async fn env_api_key_auth_login_prompts_for_secret_and_returns_api_key_credential() {
    let auth = env_api_key_auth("Test key", &["TEST_KEY"]);
    let signal = AbortController::new().signal();
    let interaction = RecordingInteraction::secret(&["entered-key"]);
    let credential = auth
        .login(ProviderAuthInteraction::new(interaction, signal))
        .await
        .expect("login ok");
    assert_eq!(
        credential,
        ApiKeyCredential {
            key: Some("entered-key".to_owned()),
            env: None,
        }
    );
}

struct StubNoopAuth(&'static str);

#[async_trait]
impl ApiKeyAuth for StubNoopAuth {
    fn name(&self) -> &str {
        self.0
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth::default(),
            env: None,
            source: None,
        }))
    }
}

struct FixedAuth {
    name: &'static str,
    result: AuthResult,
}

#[async_trait]
impl ApiKeyAuth for FixedAuth {
    fn name(&self) -> &str {
        self.name
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(Some(self.result.clone()))
    }
}

fn recording_streams(label: &'static str, calls: Arc<Mutex<Vec<String>>>) -> ProviderStreams {
    let calls_stream = Arc::clone(&calls);
    let respond_stream: pi_ai::types::StreamFunction = Arc::new(move |model, _context, _options| {
        calls_stream
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(format!("{label}:{}", model.id));
        let stream = AssistantMessageEventStream::new();
        let message = faux_assistant_message(FauxAssistantMessageContent::Text("ok".to_owned()), None);
        stream.push(AssistantMessageEvent::Start {
            partial: message.clone(),
        });
        stream.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: message.clone(),
        });
        stream.end(Some(message));
        stream
    });
    let calls_simple = calls;
    let respond_simple: pi_ai::types::SimpleStreamFunction = Arc::new(move |model, _context, _options| {
        calls_simple
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(format!("{label}:{}", model.id));
        let stream = AssistantMessageEventStream::new();
        let message = faux_assistant_message(FauxAssistantMessageContent::Text("ok".to_owned()), None);
        stream.push(AssistantMessageEvent::Start {
            partial: message.clone(),
        });
        stream.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: message.clone(),
        });
        stream.end(Some(message));
        stream
    });
    ProviderStreams {
        stream: respond_stream,
        stream_simple: respond_simple,
        fetch_deferred: None,
        cancel_deferred: None,
    }
}

fn test_model(api: &str, id: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: api.to_owned(),
        provider: "mixed".to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelModality::Text],
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 10000,
        max_tokens: 1000,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

#[tokio::test]
async fn lazily_exposes_only_declared_deferred_capabilities() {
    let loads = Arc::new(AtomicUsize::new(0));
    let loads_clone = Arc::clone(&loads);
    let ctx_captured = normalize_context(&user_context());

    let api = lazy_api(
        Arc::new(move || {
            let loads_inner = Arc::clone(&loads_clone);
            let ctx_inner = ctx_captured.clone();
            Box::pin(async move {
                loads_inner.fetch_add(1, Ordering::SeqCst);
                let mut streams = recording_streams("deferred", Arc::new(Mutex::new(Vec::new())));
                let stream_simple = Arc::clone(&streams.stream_simple);
                streams.fetch_deferred = Some(Arc::new(move |model, _handle, _options| {
                    (stream_simple)(model, &ctx_inner, None)
                }));
                Ok(streams)
            })
        }),
        Some(LazyApiCapabilities {
            fetch_deferred: true,
            cancel_deferred: false,
        }),
    );

    let model = test_model("api-a", "model-a");
    let handle = DeferredHandle {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
        api: model.api.clone(),
        id: "response-1".to_owned(),
        expires_at: None,
        poll_after_ms: None,
        data: None,
    };

    assert_eq!(loads.load(Ordering::SeqCst), 0);
    assert!(api.cancel_deferred.is_none());
    let fetch = api.fetch_deferred.as_ref().expect("fetch_deferred present");
    let res = fetch(&model, &handle, None).result().await;
    assert_eq!(res.stop_reason, StopReason::Stop);
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dispatches_on_model_api_for_mixed_api_providers() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider = create_provider(CreateProviderOptions {
        id: "mixed".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(StubNoopAuth("Test"))),
            oauth: None,
        },
        models: vec![
            pi_ai::types::AnyModel::Chat(test_model("api-a", "model-a")),
            pi_ai::types::AnyModel::Chat(test_model("api-b", "model-b")),
        ],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::ByApi(IndexMap::from([
            ("api-a".to_owned(), recording_streams("a", Arc::clone(&calls))),
            ("api-b".to_owned(), recording_streams("b", Arc::clone(&calls))),
        ]))),
        images: None,
        classifiers: None,
    })
    .expect("create_provider ok");
    let models = create_models(None);
    models.set_provider(provider);

    models
        .complete_simple(&test_model("api-a", "model-a"), &user_context(), None)
        .await;
    models
        .complete_simple(&test_model("api-b", "model-b"), &user_context(), None)
        .await;
    let recorded = calls.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(recorded, vec!["a:model-a", "b:model-b"]);
}

#[tokio::test]
async fn merges_provider_resolved_env_into_stream_options() {
    let captured_env: Arc<Mutex<Option<ProviderEnv>>> = Arc::new(Mutex::new(None));
    let captured_api_key: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut env_model = test_model("api-a", "model-a");
    env_model.provider = "env-provider".to_owned();

    let inner = recording_streams("a", Arc::new(Mutex::new(Vec::new())));
    let inner_stream = Arc::clone(&inner.stream);
    let inner_simple = Arc::clone(&inner.stream_simple);
    let env_for_stream = Arc::clone(&captured_env);
    let key_for_stream = Arc::clone(&captured_api_key);
    let env_for_simple = Arc::clone(&captured_env);
    let key_for_simple = Arc::clone(&captured_api_key);

    let provider = create_provider(CreateProviderOptions {
        id: "env-provider".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(FixedAuth {
                name: "Test",
                result: AuthResult {
                    auth: ModelAuth {
                        api_key: Some("provider-key".to_owned()),
                        headers: None,
                        base_url: None,
                    },
                    env: Some(IndexMap::from([
                        ("PROVIDER_ONLY".to_owned(), "provider".to_owned()),
                        ("SHARED".to_owned(), "provider".to_owned()),
                    ])),
                    source: None,
                },
            })),
            oauth: None,
        },
        models: vec![pi_ai::types::AnyModel::Chat(env_model.clone())],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(ProviderStreams {
            stream: Arc::new(move |model, context, options| {
                *env_for_stream.lock().unwrap_or_else(|p| p.into_inner()) =
                    options.as_ref().and_then(|o| o.base.env.clone());
                *key_for_stream.lock().unwrap_or_else(|p| p.into_inner()) =
                    options.as_ref().and_then(|o| o.base.api_key.clone());
                (inner_stream)(model, context, options)
            }),
            stream_simple: Arc::new(move |model, context, options| {
                *env_for_simple.lock().unwrap_or_else(|p| p.into_inner()) =
                    options.as_ref().and_then(|o| o.base.env.clone());
                *key_for_simple.lock().unwrap_or_else(|p| p.into_inner()) =
                    options.as_ref().and_then(|o| o.base.api_key.clone());
                (inner_simple)(model, context, options)
            }),
            fetch_deferred: None,
            cancel_deferred: None,
        })),
        images: None,
        classifiers: None,
    })
    .expect("create_provider ok");

    let models = create_models(None);
    models.set_provider(provider);

    let mut req_opts = SimpleStreamOptions::default();
    req_opts.base.api_key = Some("request-key".to_owned());
    req_opts.base.env = Some(IndexMap::from([
        ("REQUEST_ONLY".to_owned(), "request".to_owned()),
        ("SHARED".to_owned(), "request".to_owned()),
    ]));
    models
        .complete_simple(
            &env_model,
            &user_context(),
            Some(ModelsSimpleStreamOptions {
                base: req_opts,
                transform_headers: None,
            }),
        )
        .await;

    assert_eq!(
        captured_api_key.lock().unwrap_or_else(|p| p.into_inner()).as_deref(),
        Some("request-key")
    );
    assert_eq!(
        *captured_env.lock().unwrap_or_else(|p| p.into_inner()),
        Some(IndexMap::from([
            ("PROVIDER_ONLY".to_owned(), "provider".to_owned()),
            ("SHARED".to_owned(), "request".to_owned()),
            ("REQUEST_ONLY".to_owned(), "request".to_owned()),
        ]))
    );
}

#[tokio::test]
async fn applies_resolved_request_options_to_deferred_fetch_and_cancellation() {
    let fetched_model: Arc<Mutex<Option<Model>>> = Arc::new(Mutex::new(None));
    let fetched_options: Arc<Mutex<Option<DeferredFetchOptions>>> = Arc::new(Mutex::new(None));
    let cancelled_options: Arc<Mutex<Option<DeferredCancelOptions>>> = Arc::new(Mutex::new(None));

    let mut deferred_model = test_model("api-a", "model-a");
    deferred_model.provider = "deferred-provider".to_owned();

    let mut streams = recording_streams("deferred", Arc::new(Mutex::new(Vec::new())));
    let stream_simple = Arc::clone(&streams.stream_simple);
    let fm_clone = Arc::clone(&fetched_model);
    let fo_clone = Arc::clone(&fetched_options);
    let ctx_inner = normalize_context(&user_context());
    streams.fetch_deferred = Some(Arc::new(move |model, _handle, options| {
        *fm_clone.lock().unwrap_or_else(|p| p.into_inner()) = Some(model.clone());
        *fo_clone.lock().unwrap_or_else(|p| p.into_inner()) = options;
        (stream_simple)(model, &ctx_inner, None)
    }));
    let co_clone = Arc::clone(&cancelled_options);
    streams.cancel_deferred = Some(Arc::new(move |_model, _handle, options| {
        let co = Arc::clone(&co_clone);
        Box::pin(async move {
            *co.lock().unwrap_or_else(|p| p.into_inner()) = options;
            Ok(())
        })
    }));

    let provider = create_provider(CreateProviderOptions {
        id: "deferred-provider".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(FixedAuth {
                name: "Test",
                result: AuthResult {
                    auth: ModelAuth {
                        api_key: Some("provider-key".to_owned()),
                        headers: Some(IndexMap::from([
                            header_entry("Authorization", Some("Bearer provider")),
                            header_entry("X-Shared", Some("provider")),
                        ])),
                        base_url: Some("https://resolved.test/v1".to_owned()),
                    },
                    env: Some(IndexMap::from([
                        ("PROVIDER_ONLY".to_owned(), "provider".to_owned()),
                        ("SHARED".to_owned(), "provider".to_owned()),
                    ])),
                    source: None,
                },
            })),
            oauth: None,
        },
        models: vec![pi_ai::types::AnyModel::Chat(deferred_model.clone())],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(streams)),
        images: None,
        classifiers: None,
    })
    .expect("create_provider ok");

    let models = create_models(None);
    models.set_provider(provider);

    let handle = DeferredHandle {
        provider: deferred_model.provider.clone(),
        model_id: deferred_model.id.clone(),
        api: deferred_model.api.clone(),
        id: "response-1".to_owned(),
        expires_at: None,
        poll_after_ms: None,
        data: None,
    };

    models
        .fetch_deferred(
            &deferred_model,
            &handle,
            Some(ModelsDeferredFetchOptions {
                base: DeferredFetchOptions {
                    base: ProviderRequestOptions {
                        signal: None,
                        telemetry_context: None,
                        api_key: Some("request-key".to_owned()),
                        fetch: None,
                        env: Some(IndexMap::from([
                            ("REQUEST_ONLY".to_owned(), "request".to_owned()),
                            ("SHARED".to_owned(), "request".to_owned()),
                        ])),
                        on_payload: None,
                        on_response: None,
                        headers: Some(IndexMap::from([
                            header_entry("X-Request", Some("request")),
                            header_entry("x-shared", Some("request")),
                        ])),
                        timeout_ms: Some(100),
                        max_retries: None,
                        max_retry_delay_ms: None,
                    },
                    wait: Some(50),
                },
                transform_headers: Some(Arc::new(|mut headers| {
                    Box::pin(async move {
                        headers.insert("X-Transformed".to_owned(), Some("yes".to_owned()));
                        Ok(headers)
                    })
                })),
            }),
        )
        .await;

    models
        .cancel_deferred(
            &deferred_model,
            &handle,
            Some(ModelsDeferredCancelOptions {
                base: DeferredCancelOptions {
                    timeout_ms: Some(200),
                    ..Default::default()
                },
                transform_headers: Some(Arc::new(|mut headers| {
                    Box::pin(async move {
                        headers.insert("X-Cancel".to_owned(), Some("yes".to_owned()));
                        Ok(headers)
                    })
                })),
            }),
        )
        .await
        .expect("cancel_deferred ok");

    let fm = fetched_model
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .expect("fetched model");
    assert_eq!(fm.base_url, "https://resolved.test/v1");

    let fo = fetched_options
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .expect("fetched options");
    assert_eq!(fo.wait, Some(50));
    assert_eq!(fo.base.timeout_ms, Some(100));
    assert_eq!(fo.base.api_key.as_deref(), Some("request-key"));
    assert_eq!(
        fo.base.headers,
        Some(IndexMap::from([
            header_entry("Authorization", Some("Bearer provider")),
            header_entry("X-Request", Some("request")),
            header_entry("x-shared", Some("request")),
            header_entry("X-Transformed", Some("yes")),
        ]))
    );
    assert_eq!(
        fo.base.env,
        Some(IndexMap::from([
            ("PROVIDER_ONLY".to_owned(), "provider".to_owned()),
            ("SHARED".to_owned(), "request".to_owned()),
            ("REQUEST_ONLY".to_owned(), "request".to_owned()),
        ]))
    );

    let co = cancelled_options
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .expect("cancelled options");
    assert_eq!(co.timeout_ms, Some(200));
    assert_eq!(co.api_key.as_deref(), Some("provider-key"));
    assert_eq!(
        co.headers,
        Some(IndexMap::from([
            header_entry("Authorization", Some("Bearer provider")),
            header_entry("X-Shared", Some("provider")),
            header_entry("X-Cancel", Some("yes")),
        ]))
    );
    assert_eq!(
        co.env,
        Some(IndexMap::from([
            ("PROVIDER_ONLY".to_owned(), "provider".to_owned()),
            ("SHARED".to_owned(), "provider".to_owned()),
        ]))
    );
}

#[tokio::test]
async fn produces_a_stream_error_for_a_model_whose_api_has_no_implementation() {
    let provider = create_provider(CreateProviderOptions {
        id: "mixed".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(StubNoopAuth("Test"))),
            oauth: None,
        },
        models: vec![pi_ai::types::AnyModel::Chat(test_model("api-a", "model-a"))],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::ByApi(IndexMap::from([(
            "api-a".to_owned(),
            recording_streams("a", Arc::new(Mutex::new(Vec::new()))),
        )]))),
        images: None,
        classifiers: None,
    })
    .expect("create_provider ok");

    let transcript = normalize_context(&user_context());
    let result = provider
        .stream_simple(&test_model("api-ghost", "model-x"), &transcript, None)
        .result()
        .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|msg| msg.contains("no API implementation")),
        "unexpected error_message: {:?}",
        result.error_message
    );
}

#[tokio::test]
async fn lets_a_newer_dynamic_refresh_bypass_and_supersede_older_network_work() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let first_started = Arc::new(Notify::new());
    let first_blocked = Arc::new(Notify::new());
    let fetches_fn = Arc::clone(&fetches);
    let first_started_fn = Arc::clone(&first_started);
    let first_blocked_fn = Arc::clone(&first_blocked);

    let provider = create_provider(CreateProviderOptions {
        id: "dynamic".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(StubNoopAuth("Test"))),
            oauth: None,
        },
        models: Vec::new(),
        fetch_models: Some(Arc::new(move |_ctx| {
            let fetches_inner = Arc::clone(&fetches_fn);
            let started = Arc::clone(&first_started_fn);
            let blocked = Arc::clone(&first_blocked_fn);
            Box::pin(async move {
                let current = fetches_inner.fetch_add(1, Ordering::SeqCst) + 1;
                if current == 1 {
                    started.notify_one();
                    blocked.notified().await;
                }
                Ok(vec![pi_ai::types::AnyModel::Chat(test_model(
                    "api-a",
                    &format!("listed-{current}"),
                ))])
            })
        })),
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(recording_streams(
            "a",
            Arc::new(Mutex::new(Vec::new())),
        ))),
        images: None,
        classifiers: None,
    })
    .expect("create_provider ok");

    let store = Arc::new(InMemoryModelsStore::new());
    let models = create_models(Some(CreateModelsOptions {
        credentials: None,
        models_store: Some(store.clone()),
        auth_context: None,
    }));
    models.set_provider(provider.clone());
    assert!(provider.get_models().is_empty());

    let first_fut = models.refresh(Some(ModelsRefreshOptions {
        allow_network: None,
        providers: Some(vec!["dynamic".to_owned()]),
        force: None,
        signal: None,
    }));
    tokio::pin!(first_fut);

    let notified_first = first_started.notified();
    tokio::select! {
        _ = notified_first => {}
        _ = &mut first_fut => unreachable!("first refresh should block until notified"),
    }

    let second = models
        .refresh(Some(ModelsRefreshOptions {
            allow_network: None,
            providers: Some(vec!["dynamic".to_owned()]),
            force: None,
            signal: None,
        }))
        .await
        .expect("second refresh ok");
    assert!(!second.aborted);

    let first = first_fut.await.expect("first refresh ok");
    assert!(!first.aborted);
    assert_eq!(fetches.load(Ordering::SeqCst), 2);
    assert_eq!(
        provider.get_models().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["listed-2"]
    );
    let stored = store
        .read("dynamic", None)
        .await
        .expect("store read ok")
        .expect("entry present");
    assert_eq!(
        stored.models.iter().map(|m| m.id()).collect::<Vec<_>>(),
        vec!["listed-2"]
    );

    first_blocked.notify_one();
    tokio::task::yield_now().await;
    assert_eq!(
        provider.get_models().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["listed-2"]
    );
    let stored_after = store
        .read("dynamic", None)
        .await
        .expect("store read ok")
        .expect("entry present");
    assert_eq!(
        stored_after.models.iter().map(|m| m.id()).collect::<Vec<_>>(),
        vec!["listed-2"]
    );
}

#[tokio::test]
async fn faux_provider_streams_queued_responses_through_a_models_collection() {
    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("hello from faux".to_owned()),
        None,
    ))]);

    let model = models.get_models(Some(faux.provider().id())).swap_remove(0);
    let result = models.complete_simple(&model, &user_context(), None).await;
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert!(matches!(
        result.content.as_slice(),
        [AssistantContentBlock::Text(t)] if t.text == "hello from faux"
    ));
    assert_eq!(faux.state().call_count(), 1);
}

#[tokio::test]
async fn faux_provider_submits_polls_and_redeems_deferred_responses() {
    let faux = faux_provider(Some(RegisterFauxProviderOptions {
        deferred: Some(FauxDeferredOptions {
            pending_fetches: Some(1),
            poll_after_ms: Some(25),
        }),
        ..Default::default()
    }));
    let models = create_models(None);
    models.set_provider(faux.provider());
    faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_owned()),
        None,
    ))]);
    let model = faux.get_model();

    let mut opts = SimpleStreamOptions::default();
    opts.deferred = Some(DeferredOption::Window {
        window: Some(DeferredWindow::H1),
    });
    let mut submission = models.stream_simple(
        &model,
        &user_context(),
        Some(ModelsSimpleStreamOptions {
            base: opts,
            transform_headers: None,
        }),
    );

    let mut event_types = Vec::new();
    while let Some(event) = submission.next().await {
        let kind = match event {
            AssistantMessageEvent::Start { .. } => "start",
            AssistantMessageEvent::Done { .. } => "done",
            _ => "other",
        };
        event_types.push(kind);
    }
    let deferred = submission.result().await;
    assert_eq!(event_types, vec!["start", "done"]);
    assert_eq!(deferred.stop_reason, StopReason::Deferred);
    assert!(deferred.content.is_empty());

    let handle = deferred
        .deferred
        .clone()
        .expect("Faux response did not include a deferred handle");
    assert_eq!(handle.provider, model.provider);
    assert_eq!(handle.model_id, model.id);
    assert_eq!(handle.api, model.api);
    assert!(!handle.id.is_empty());
    assert_eq!(handle.poll_after_ms, Some(25));

    let pending = models.fetch_deferred(&model, &handle, None).await;
    assert_eq!(pending.stop_reason, StopReason::Deferred);
    assert_eq!(pending.deferred.as_ref(), Some(&handle));

    let ready = models
        .fetch_deferred(
            &model,
            &handle,
            Some(ModelsDeferredFetchOptions {
                base: DeferredFetchOptions {
                    base: ProviderRequestOptions::default(),
                    wait: Some(0),
                },
                transform_headers: None,
            }),
        )
        .await;
    assert_eq!(ready.stop_reason, StopReason::Stop);
    assert!(matches!(
        ready.content.as_slice(),
        [AssistantContentBlock::Text(t)] if t.text == "ready"
    ));
    assert!(ready.usage.total_tokens > 0);
    assert_eq!(faux.state().call_count(), 1);
    assert_eq!(faux.state().deferred_fetch_count(), 2);
}

#[tokio::test]
async fn faux_provider_records_cancellation_and_returns_deferred_fetch_failures_in_band() {
    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    faux.set_responses(vec![
        FauxResponseStep::Factory(Arc::new(|_, _, _, _| {
            Box::pin(async { Err(Error::msg("deferred failed")) })
        })),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("cancelled".to_owned()),
            None,
        )),
    ]);
    let model = faux.get_model();

    let mut deferred_opts = SimpleStreamOptions::default();
    deferred_opts.deferred = Some(DeferredOption::Bool(true));

    let failed_submission = models
        .complete_simple(
            &model,
            &user_context(),
            Some(ModelsSimpleStreamOptions {
                base: deferred_opts.clone(),
                transform_headers: None,
            }),
        )
        .await;
    let failed_handle = failed_submission
        .deferred
        .expect("Faux response did not include a deferred handle");
    let failed = models.fetch_deferred(&model, &failed_handle, None).await;
    assert_eq!(failed.stop_reason, StopReason::Error);
    assert_eq!(failed.error_message.as_deref(), Some("deferred failed"));

    let cancelled_submission = models
        .complete_simple(
            &model,
            &user_context(),
            Some(ModelsSimpleStreamOptions {
                base: deferred_opts,
                transform_headers: None,
            }),
        )
        .await;
    let cancelled_handle = cancelled_submission
        .deferred
        .expect("Faux response did not include a deferred handle");
    models
        .cancel_deferred(&model, &cancelled_handle, None)
        .await
        .expect("cancel_deferred ok");
    assert_eq!(faux.state().cancelled_deferred(), vec![cancelled_handle.clone()]);
    let cancelled = models.fetch_deferred(&model, &cancelled_handle, None).await;
    assert_eq!(cancelled.stop_reason, StopReason::Error);
    assert!(
        cancelled
            .error_message
            .as_deref()
            .is_some_and(|msg| msg.contains("was cancelled")),
        "unexpected error_message: {:?}",
        cancelled.error_message
    );
}

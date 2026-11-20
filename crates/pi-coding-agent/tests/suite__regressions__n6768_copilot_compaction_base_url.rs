//! Port of `packages/coding-agent/test/suite/regressions/6768-copilot-compaction-base-url.test.ts`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent_core::{AgentMessage, StreamFn};
use pi_ai::{
    ApiStreamOptions, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    Credential, LoginOptions, Message, Model, ModelAuth, ModelCost, ModelInput, OAuthAuth, OAuthCredential, Provider,
    ProviderAuth, ProviderAuthInteraction, SimpleStreamOptions, StopReason, TranscriptContext, Usage, UserMessage,
    UserMessageContent, faux_text,
};
use pi_coding_agent::core::auth_storage::{AuthStorage, AuthStorageData};
use pi_coding_agent::core::compaction::{CompactOptions, CompactionSettings, compact, prepare_compaction};
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::session_manager::SessionManager;

const INDIVIDUAL_BASE_URL: &str = "https://api.individual.githubcopilot.com";
const ENTERPRISE_BASE_URL: &str = "https://api.enterprise.githubcopilot.com";

fn base_model(base_url: &str) -> Model {
    Model {
        r#type: None,
        id: "gpt-4o".to_owned(),
        name: "Copilot GPT-4o".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "github-copilot".to_owned(),
        base_url: base_url.to_owned(),
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
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

#[tokio::test]
async fn compaction_forwards_resolved_enterprise_base_url_through_stream_wrapper() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    sm.append_message(AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text("message to compact".to_owned()),
        timestamp: 1000,
    })))
    .unwrap();
    sm.append_message(AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text("assistant response to compact"))],
        api: "openai-completions".to_owned(),
        provider: "github-copilot".to_owned(),
        model: "gpt-4o".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 100,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 100,
            cost: Default::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1500,
        duration_ms: None,
    })))
    .unwrap();

    let prep = prepare_compaction(
        &sm.get_branch(None),
        &CompactionSettings {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 1,
        },
    )
    .unwrap();

    let seen_base_url: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let seen_ref = Arc::clone(&seen_base_url);

    // Simulates ModelRuntime::stream_simple resolving the OAuth enterprise baseUrl over the catalog baseUrl.
    let stream_fn: StreamFn = Arc::new(move |mut model, _ctx, _opts| {
        let seen = Arc::clone(&seen_ref);
        Box::pin(async move {
            if model.provider == "github-copilot" && model.base_url == INDIVIDUAL_BASE_URL {
                model.base_url = ENTERPRISE_BASE_URL.to_owned();
            }
            *seen.lock().unwrap() = Some(model.base_url.clone());

            let stream = AssistantMessageEventStream::new();
            stream.push(AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: AssistantMessage {
                    content: vec![AssistantContentBlock::Text(faux_text("summary"))],
                    api: model.api,
                    provider: model.provider,
                    model: model.id,
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
                    timestamp: 0,
                    duration_ms: None,
                },
            });
            Ok(stream)
        })
    });

    let res = compact(
        &prep,
        &base_model(INDIVIDUAL_BASE_URL),
        Some(CompactOptions {
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert!(res.summary.contains("summary"));
    assert_eq!(seen_base_url.lock().unwrap().as_deref(), Some(ENTERPRISE_BASE_URL));
}

struct EnterpriseCopilotOAuth;

#[async_trait]
impl OAuthAuth for EnterpriseCopilotOAuth {
    fn name(&self) -> &str {
        "GitHub Copilot OAuth"
    }

    async fn login(
        &self,
        _interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> pi_js::error::Result<OAuthCredential> {
        Err(pi_js::error::Error::msg("not used"))
    }

    async fn refresh(
        &self,
        credential: OAuthCredential,
        _signal: pi_js::abort::AbortSignal,
    ) -> pi_js::error::Result<OAuthCredential> {
        Ok(credential)
    }

    async fn to_auth(&self, credential: OAuthCredential) -> pi_js::error::Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: Some(ENTERPRISE_BASE_URL.to_owned()),
        })
    }
}

struct MockEnterpriseCopilotProvider {
    auth: ProviderAuth,
    seen_base_url: Arc<Mutex<Option<String>>>,
}

impl Provider for MockEnterpriseCopilotProvider {
    fn id(&self) -> &str {
        "github-copilot"
    }

    fn name(&self) -> &str {
        "GitHub Copilot"
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        vec![base_model(INDIVIDUAL_BASE_URL)]
    }

    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        *self.seen_base_url.lock().unwrap() = Some(model.base_url.clone());
        AssistantMessageEventStream::new()
    }

    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        *self.seen_base_url.lock().unwrap() = Some(model.base_url.clone());
        let stream = AssistantMessageEventStream::new();
        stream.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: AssistantMessage {
                content: vec![AssistantContentBlock::Text(faux_text("runtime summary"))],
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
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
                timestamp: 0,
                duration_ms: None,
            },
        });
        stream
    }
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_resolves_enterprise_base_url_through_model_runtime_stream_simple() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    sm.append_message(AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text("message to compact".to_owned()),
        timestamp: 1000,
    })))
    .unwrap();
    sm.append_message(AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text("assistant response to compact"))],
        api: "openai-completions".to_owned(),
        provider: "github-copilot".to_owned(),
        model: "gpt-4o".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 100,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 100,
            cost: Default::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1500,
        duration_ms: None,
    })))
    .unwrap();

    let prep = prepare_compaction(
        &sm.get_branch(None),
        &CompactionSettings {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 1,
        },
    )
    .unwrap();

    let mut creds = AuthStorageData::new();
    creds.insert(
        "github-copilot".to_owned(),
        Credential::OAuth(OAuthCredential::new(
            "refresh",
            "access",
            pi_js::time::now_ms() + 60_000,
        )),
    );
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Some(creds)))),
        models_path: Some(None),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let seen_base_url: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    runtime
        .register_native_provider(Arc::new(MockEnterpriseCopilotProvider {
            auth: ProviderAuth {
                api_key: None,
                oauth: Some(Arc::new(EnterpriseCopilotOAuth)),
            },
            seen_base_url: Arc::clone(&seen_base_url),
        }))
        .unwrap();

    let rt_ref = runtime.clone();
    let stream_fn: StreamFn = Arc::new(move |model, ctx, opts| {
        let rt = rt_ref.clone();
        Box::pin(async move {
            let model_owned: Model = model.clone();
            let simple_opts = opts.map(|base| pi_ai::models::ModelsSimpleStreamOptions {
                base,
                transform_headers: None,
            });
            Ok(rt.stream_simple(&model_owned, &ctx, simple_opts))
        })
    });

    let res = compact(
        &prep,
        &base_model(INDIVIDUAL_BASE_URL),
        Some(CompactOptions {
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert!(res.summary.contains("runtime summary"));
    assert_eq!(seen_base_url.lock().unwrap().as_deref(), Some(ENTERPRISE_BASE_URL));
}

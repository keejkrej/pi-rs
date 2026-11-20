//! Port of packages/coding-agent/test/suite/regressions/11520-sdk-summary-oauth-auth.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::types::AgentMessage;
use pi_ai::Result;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, AuthCheck, AuthResult, AuthType, Credential, CredentialStore, LoginOptions, ModelAuth,
    OAuthAuth, OAuthCredential, ProviderAuth, ProviderAuthInteraction,
};
use pi_ai::models::{Provider, RefreshModelsContext};
use pi_ai::providers::faux::{FauxAssistantMessageContent, faux_assistant_message};
use pi_ai::types::{
    ApiStreamOptions, AssistantMessageEvent, Message, Model, ModelCost, ModelInput, SimpleStreamOptions, StopReason,
    TextContent, TranscriptContext, Usage, UsageCost, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use pi_coding_agent::core::agent_session::NavigateTreeOptions;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;
use support::utilities::create_test_resource_loader;
use tempfile::TempDir;

struct UnusedEnvApiKey;

#[async_trait]
impl ApiKeyAuth for UnusedEnvApiKey {
    fn name(&self) -> &str {
        "Env key"
    }

    fn supports_check(&self) -> bool {
        true
    }

    async fn check(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthCheck>> {
        Ok(Some(AuthCheck {
            auth_type: AuthType::ApiKey,
            source: Some("UNUSED_ENV_KEY".to_string()),
        }))
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        let mut headers = IndexMap::new();
        headers.insert("x-auth-mode".to_string(), Some("api-key".to_string()));
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: input
                    .credential
                    .and_then(|c| c.key)
                    .or_else(|| Some("UNUSED_ENV_KEY".to_string())),
                headers: Some(headers),
                base_url: None,
            },
            env: None,
            source: Some("api-key".to_string()),
        }))
    }
}

struct OAuthOnlyOAuth;

#[async_trait]
impl OAuthAuth for OAuthOnlyOAuth {
    fn name(&self) -> &str {
        "Subscription"
    }

    async fn login(
        &self,
        _interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        Ok(OAuthCredential {
            access: "oauth-access".to_string(),
            refresh: "refresh".to_string(),
            expires: pi_js::time::now_ms() + 60_000,
            extra: Default::default(),
        })
    }

    async fn refresh(&self, credential: OAuthCredential, _signal: AbortSignal) -> Result<OAuthCredential> {
        Ok(credential)
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        let mut headers = IndexMap::new();
        headers.insert("x-auth-mode".to_string(), Some("oauth".to_string()));
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: Some(headers),
            base_url: None,
        })
    }
}

struct OAuthOnlyProvider {
    auth: ProviderAuth,
    model: Model,
    requests: Arc<Mutex<Vec<Option<SimpleStreamOptions>>>>,
}

impl Provider for OAuthOnlyProvider {
    fn id(&self) -> &str {
        "oauth-only-provider"
    }

    fn name(&self) -> &str {
        "OAuth Only"
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        vec![self.model.clone()]
    }

    fn refresh_models(&self, _context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        Some(Box::pin(async { Ok(()) }))
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.stream_simple(
            model,
            context,
            options.map(|o| SimpleStreamOptions {
                base: o.base,
                ..Default::default()
            }),
        )
    }

    fn stream_simple(
        &self,
        request_model: &Model,
        _context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.requests.lock().unwrap().push(options);
        let stream = create_assistant_message_event_stream();
        let mut final_msg = faux_assistant_message(
            FauxAssistantMessageContent::Text("branch summary text".to_string()),
            None,
        );
        final_msg.api = request_model.api.clone();
        final_msg.provider = request_model.provider.clone();
        final_msg.model = request_model.id.clone();
        final_msg.usage = Usage {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 2,
            cost: UsageCost::default(),
        };
        final_msg.stop_reason = StopReason::Stop;
        final_msg.timestamp = pi_js::time::now_ms();
        stream.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: final_msg.clone(),
        });
        stream.end(Some(final_msg));
        stream
    }
}

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::from(Message::User(UserMessage {
        content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        timestamp: pi_js::time::now_ms(),
    }))
}

fn assistant_msg(text: &str) -> AgentMessage {
    AgentMessage::from(Message::Assistant(faux_assistant_message(
        FauxAssistantMessageContent::Text(text.to_string()),
        None,
    )))
}

// Issue #11520: SDK summary paths must let ModelRuntime resolve stored OAuth without passing an explicit apiKey override.
#[tokio::test]
async fn lets_the_runtime_resolve_stored_oauth_for_branch_summaries() {
    let dir = TempDir::new().expect("tempdir");
    let cwd = dir.path().to_str().expect("utf8");

    let mut initial_creds = IndexMap::new();
    initial_creds.insert(
        "oauth-only-provider".to_string(),
        Credential::OAuth(OAuthCredential {
            access: "oauth-access".to_string(),
            refresh: "refresh".to_string(),
            expires: pi_js::time::now_ms() + 3_600_000,
            extra: Default::default(),
        }),
    );
    let credentials: Arc<dyn CredentialStore + Send + Sync> = Arc::new(AuthStorage::in_memory(Some(initial_creds)));
    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials),
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .expect("model_runtime");

    let model = Model {
        r#type: None,
        id: "oauth-model".to_string(),
        name: "OAuth Model".to_string(),
        api: "openai-completions".to_string(),
        provider: "oauth-only-provider".to_string(),
        base_url: "https://example.test/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 4_096,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    };

    let requests = Arc::new(Mutex::new(Vec::<Option<SimpleStreamOptions>>::new()));
    model_runtime
        .register_native_provider(Arc::new(OAuthOnlyProvider {
            auth: ProviderAuth {
                api_key: Some(Arc::new(UnusedEnvApiKey)),
                oauth: Some(Arc::new(OAuthOnlyOAuth)),
            },
            model: model.clone(),
            requests: Arc::clone(&requests),
        }))
        .expect("register provider");

    let session_manager = SessionManager::in_memory(Some(cwd), None, None).expect("session_manager");
    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(cwd.to_string()),
        agent_dir: Some(cwd.to_string()),
        model: Some(model),
        model_runtime: Some(model_runtime),
        settings_manager: Some(SettingsManager::in_memory(None, None)),
        session_manager: Some(session_manager.clone()),
        resource_loader: Some(Arc::new(create_test_resource_loader(Default::default()))),
        ..Default::default()
    }))
    .await
    .expect("create_agent_session");
    let session = created.session;

    let target_id = session_manager.append_message(user_msg("first branch")).unwrap();
    session_manager.append_message(assistant_msg("first reply")).unwrap();
    session_manager
        .append_message(user_msg("abandoned branch work"))
        .unwrap();
    session_manager
        .append_message(assistant_msg("abandoned reply"))
        .unwrap();

    let result = session
        .navigate_tree(
            &target_id,
            Some(NavigateTreeOptions {
                summarize: Some(true),
                ..Default::default()
            }),
        )
        .await
        .expect("navigate_tree");

    let summary = result.summary_entry.expect("summary_entry").summary;
    assert!(summary.contains("branch summary text"), "summary: {summary}");

    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let opts = reqs[0].as_ref().expect("stream options");
    assert_eq!(opts.base.api_key.as_deref(), Some("oauth-access"));
    let auth_mode = opts
        .base
        .headers
        .as_ref()
        .and_then(|h| h.get("x-auth-mode"))
        .and_then(|v| v.as_deref());
    assert_eq!(auth_mode, Some("oauth"), "headers: {:?}", opts.base.headers);

    session.dispose();
}

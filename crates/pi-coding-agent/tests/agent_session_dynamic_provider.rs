//! Port of packages/coding-agent/test/agent-session-dynamic-provider.test.ts

use std::sync::{Arc, Mutex, PoisonError};

use pi_ai::compat::get_model;
use pi_ai::models::{Provider, ProviderAuth};
use pi_ai::types::{ApiStreamOptions, AssistantMessageEventStream, Model, SimpleStreamOptions, TranscriptContext};
use pi_ai::{ApiKeyAuth, ApiKeyAuthInput, AuthResult, ModelAuth};
use pi_coding_agent::core::agent_session::{AgentSession, ExtensionBindings};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::{
    CommandHandlerFn, ExtensionApi, InlineExtension, ProviderConfig, RegisteredCommandOptions, SessionStartEvent,
};
use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_js::BoxFuture;
use pretty_assertions::assert_eq;

struct StaticTestApiKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for StaticTestApiKeyAuth {
    fn name(&self) -> &str {
        "Test API key"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some("test-key".to_string()),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some("test".to_string()),
        }))
    }
}

struct NativeAnthropicProvider {
    base_url: String,
    model: Model,
    auth: ProviderAuth,
}

impl NativeAnthropicProvider {
    fn new(base_url: &str) -> Self {
        let mut model = get_model("anthropic", "claude-sonnet-4-5").unwrap();
        model.base_url = base_url.to_string();
        Self {
            base_url: base_url.to_string(),
            model,
            auth: ProviderAuth {
                api_key: Some(Arc::new(StaticTestApiKeyAuth)),
                oauth: None,
            },
        }
    }
}

impl Provider for NativeAnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn name(&self) -> &str {
        "Native Anthropic"
    }

    fn base_url(&self) -> Option<&str> {
        Some(&self.base_url)
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        vec![self.model.clone()]
    }

    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        panic!("unused")
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        panic!("unused")
    }
}

async fn create_session(extension_factories: Vec<InlineExtension>) -> (AgentSession, String) {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[
        &pi_js::env::tmp_dir(),
        "pi-dynamic-provider-test-",
    ]))
    .unwrap();
    let agent_dir = pi_js::path::join(&[&temp_dir, "agent"]);
    pi_js::fs::mkdir(&agent_dir, true).unwrap();

    let settings_manager = SettingsManager::create(&temp_dir, Some(&agent_dir), None);
    let session_manager = SessionManager::in_memory(Some(&temp_dir), None, None).unwrap();
    let auth_storage = Arc::new(AuthStorage::create(Some(&pi_js::path::join(&[
        &agent_dir,
        "auth.json",
    ]))));
    let _ = auth_storage;
    let model_runtime = ModelRuntime::create(ModelRuntimeCreateOptions {
        credentials: Some(auth_storage),
        models_path: Some(pi_js::path::join(&[&agent_dir, "models.json"])),
        ..Default::default()
    })
    .await
    .unwrap();

    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: Some(temp_dir.clone()),
        agent_dir: Some(agent_dir.clone()),
        settings_manager: Some(settings_manager.clone()),
        extension_factories: Some(extension_factories),
        ..Default::default()
    }));
    resource_loader.reload(None).await.unwrap();

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(temp_dir.clone()),
        agent_dir: Some(agent_dir),
        model: Some(get_model("anthropic", "claude-sonnet-4-5").unwrap()),
        settings_manager: Some(settings_manager),
        session_manager: Some(session_manager),
        model_runtime: Some(model_runtime),
        resource_loader: Some(resource_loader),
        ..Default::default()
    }))
    .await
    .unwrap();

    (created.session, temp_dir)
}

async fn capture_prompt_base_url(session: &AgentSession) -> Option<String> {
    let base_url: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let base_url_ref = Arc::clone(&base_url);
    session.agent().set_stream_fn(Some(Arc::new(move |model, _ctx, _opts| {
        *base_url_ref.lock().unwrap_or_else(PoisonError::into_inner) = Some(model.base_url);
        Box::pin(async move { Err(pi_js::Error::msg("stop")) })
    })));
    let _ = session.prompt("hello", None).await;
    base_url.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

#[tokio::test]
async fn applies_top_level_register_provider_overrides_to_the_active_model() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.register_provider(
            "anthropic",
            ProviderConfig {
                base_url: Some("http://localhost:8080/top-level".to_string()),
                ..Default::default()
            },
        )?;
        Ok(())
    }));

    let (session, temp_dir) = create_session(vec![ext]).await;
    assert_eq!(
        session.model().map(|m| m.base_url),
        Some("http://localhost:8080/top-level".to_string())
    );
    assert_eq!(
        capture_prompt_base_url(&session).await.as_deref(),
        Some("http://localhost:8080/top-level")
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn applies_session_start_register_provider_overrides_to_the_active_model() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        let pi_ev = pi.clone();
        pi.on::<SessionStartEvent>(move |_ev, _ctx| {
            let pi_handle = pi_ev.clone();
            Box::pin(async move {
                pi_handle.register_provider(
                    "anthropic",
                    ProviderConfig {
                        base_url: Some("http://localhost:8080/session-start".to_string()),
                        ..Default::default()
                    },
                )?;
                Ok(None)
            })
        });
        Ok(())
    }));

    let (session, temp_dir) = create_session(vec![ext]).await;
    session.bind_extensions(ExtensionBindings::default()).await.unwrap();

    assert_eq!(
        session.model().map(|m| m.base_url),
        Some("http://localhost:8080/session-start".to_string())
    );
    assert_eq!(
        capture_prompt_base_url(&session).await.as_deref(),
        Some("http://localhost:8080/session-start")
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn registers_native_pi_ai_providers_during_extension_loading() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.register_native_provider(Arc::new(NativeAnthropicProvider::new(
            "http://localhost:8080/native-top-level",
        )))?;
        Ok(())
    }));

    let (session, temp_dir) = create_session(vec![ext]).await;
    assert_eq!(
        session.model().map(|m| m.base_url),
        Some("http://localhost:8080/native-top-level".to_string())
    );
    assert_eq!(
        capture_prompt_base_url(&session).await.as_deref(),
        Some("http://localhost:8080/native-top-level")
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn applies_command_time_register_provider_overrides_without_reload() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        let pi_cmd = pi.clone();
        let handler: CommandHandlerFn = Arc::new(move |_args, _ctx| -> BoxFuture<pi_js::Result<()>> {
            let pi_handle = pi_cmd.clone();
            Box::pin(async move {
                pi_handle.register_provider(
                    "anthropic",
                    ProviderConfig {
                        base_url: Some("http://localhost:8080/command".to_string()),
                        ..Default::default()
                    },
                )
            })
        });
        pi.register_command(
            "use-proxy",
            RegisteredCommandOptions {
                description: Some("Use proxy".to_string()),
                get_argument_completions: None,
                handler,
            },
        )?;
        Ok(())
    }));

    let (session, temp_dir) = create_session(vec![ext]).await;
    session.bind_extensions(ExtensionBindings::default()).await.unwrap();
    session.prompt("/use-proxy", None).await.unwrap();

    assert_eq!(
        session.model().map(|m| m.base_url),
        Some("http://localhost:8080/command".to_string())
    );
    assert_eq!(
        capture_prompt_base_url(&session).await.as_deref(),
        Some("http://localhost:8080/command")
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn registers_native_pi_ai_providers_at_command_time() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        let pi_cmd = pi.clone();
        let handler: CommandHandlerFn = Arc::new(move |_args, _ctx| -> BoxFuture<pi_js::Result<()>> {
            let pi_handle = pi_cmd.clone();
            Box::pin(async move {
                pi_handle.register_native_provider(Arc::new(NativeAnthropicProvider::new(
                    "http://localhost:8080/native-command",
                )))
            })
        });
        pi.register_command(
            "use-native",
            RegisteredCommandOptions {
                description: Some("Use native provider".to_string()),
                get_argument_completions: None,
                handler,
            },
        )?;
        Ok(())
    }));

    let (session, temp_dir) = create_session(vec![ext]).await;
    session.bind_extensions(ExtensionBindings::default()).await.unwrap();
    session.prompt("/use-native", None).await.unwrap();

    assert_eq!(
        session.model().map(|m| m.base_url),
        Some("http://localhost:8080/native-command".to_string())
    );
    assert_eq!(
        capture_prompt_base_url(&session).await.as_deref(),
        Some("http://localhost:8080/native-command")
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

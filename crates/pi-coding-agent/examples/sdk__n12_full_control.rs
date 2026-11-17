//! Port of packages/coding-agent/examples/sdk/12-full-control.ts

use std::process::ExitCode;
use std::sync::Arc;

use pi_ai::compat::get_model;
use pi_ai::types::{AssistantMessageEvent, ModelThinkingLevel};
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::diagnostics::ResourceDiagnostic;
use pi_coding_agent::core::extensions::loader::create_extension_runtime;
use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use pi_coding_agent::core::prompt_templates::PromptTemplate;
use pi_coding_agent::core::resource_loader::{
    ContextFile, LoadExtensionsResult, PromptSource, ResourceExtensionPaths, ResourceLoader,
    ResourceLoaderReloadOptions, Theme,
};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::{CompactionSettings, RetrySettings, Settings, SettingsManager};
use pi_coding_agent::core::skills::Skill;
use pi_js::BoxFuture;

struct MinimalResourceLoader;

impl ResourceLoader for MinimalResourceLoader {
    fn get_extensions(&self) -> LoadExtensionsResult {
        let runtime = create_extension_runtime();
        let _ = &runtime;
        LoadExtensionsResult::default()
    }

    fn get_skills(&self) -> (Vec<Skill>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }

    fn get_prompts(&self) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }

    fn get_themes(&self) -> (Vec<Theme>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }

    fn get_agents_files(&self) -> Vec<ContextFile> {
        Vec::new()
    }

    fn get_system_prompt(&self) -> Option<String> {
        Some("You are a minimal assistant.\nAvailable: read, bash. Be concise.".to_string())
    }

    fn get_system_prompt_source(&self) -> Option<PromptSource> {
        None
    }

    fn get_append_system_prompt(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_append_system_prompt_sources(&self) -> Vec<PromptSource> {
        Vec::new()
    }

    fn extend_resources(&self, _paths: ResourceExtensionPaths) {}

    fn reload(&self, _options: Option<ResourceLoaderReloadOptions>) -> BoxFuture<pi_coding_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

async fn run() -> pi_coding_agent::Result<()> {
    let model_runtime = ModelRuntime::create(ModelRuntimeCreateOptions {
        auth_path: Some("/tmp/my-agent/auth.json".to_string()),
        models_path: Some(Some("/tmp/my-agent/models.json".to_string())),
        ..Default::default()
    })
    .await?;

    if let Some(key) = pi_js::env::var("MY_ANTHROPIC_KEY") {
        model_runtime.set_runtime_api_key("anthropic", &key, None).await?;
    }

    let model =
        get_model("anthropic", "claude-sonnet-4-5").ok_or_else(|| pi_coding_agent::Error::msg("Model not found"))?;

    let settings_manager = SettingsManager::in_memory(
        Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(false),
                ..Default::default()
            }),
            retry: Some(RetrySettings {
                enabled: Some(true),
                max_retries: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        }),
        None,
    );

    let cwd = pi_js::env::cwd();
    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some("/tmp/my-agent".to_string()),
        model: Some(model),
        thinking_level: Some(ModelThinkingLevel::Off),
        model_runtime: Some(model_runtime),
        resource_loader: Some(Arc::new(MinimalResourceLoader)),
        tools: Some(vec!["read".to_string(), "bash".to_string()]),
        session_manager: Some(SessionManager::in_memory(Some(&cwd), None, None)?),
        settings_manager: Some(settings_manager),
        ..Default::default()
    }))
    .await?;
    let session = created.session;

    let _unsub = session.subscribe(|event| {
        if let AgentSessionEvent::MessageUpdate {
            assistant_message_event: AssistantMessageEvent::TextDelta { delta, .. },
            ..
        } = event
        {
            pi_js::console::stdout_write(&delta);
        }
    });

    let res = session.prompt("List files in the current directory.", None).await;
    pi_js::console::log("");
    session.dispose();
    res
}

fn main() -> ExitCode {
    pi_js::ensure_crypto_provider();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            pi_js::console::error(&format!("Failed to build runtime: {err}"));
            return ExitCode::from(1);
        }
    };
    match rt.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            pi_js::console::error(&format!("Error: {err}"));
            ExitCode::from(1)
        }
    }
}

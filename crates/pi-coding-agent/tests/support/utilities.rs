//! Port of packages/coding-agent/test/utilities.ts

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Credential, Message, StopReason, TextContent, Usage, UsageCost,
    UserMessage, UserMessageContent,
};
use pi_coding_agent::core::agent_session::AgentSession;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::diagnostics::ResourceDiagnostic;
use pi_coding_agent::core::event_bus::{LocalEventBus, create_event_bus};
use pi_coding_agent::core::extensions::loader::{create_extension_runtime, load_extension_from_factory};
use pi_coding_agent::core::extensions::runner::ExtensionRunner;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, ExtensionCommandContext, ExtensionFactory, LoadExtensionsResult as CoreLoadExtensionsResult,
    RegisteredCommandOptions,
};
use pi_coding_agent::core::prompt_templates::PromptTemplate;
use pi_coding_agent::core::resource_loader::{
    ContextFile, LoadExtensionsResult as LoaderLoadExtensionsResult, PromptSource, ResourceExtensionPaths,
    ResourceLoader, ResourceLoaderReloadOptions, Theme as LoaderTheme,
};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_coding_agent::core::skills::Skill;
use pi_coding_agent::core::source_info::SourceInfo;
use pi_js::{BoxFuture, Error, Result};
use serde_json::Value;

use super::model_runtime_test_utils::{
    ModelRuntime, create_in_memory_model_registry, create_model_registry, get_model_runtime,
};

/// API key for authenticated tests (not used in offline unit tests).
pub fn api_key() -> Option<String> {
    pi_js::env::var("ANTHROPIC_OAUTH_TOKEN").or_else(|| pi_js::env::var("ANTHROPIC_API_KEY"))
}

pub fn pi_agent_dir() -> String {
    pi_js::path::join(&[&pi_js::env::home_dir(), ".pi", "agent"])
}

fn auth_path() -> String {
    pi_js::path::join(&[&pi_agent_dir(), "auth.json"])
}

pub fn has_auth_for_provider(provider: &str) -> bool {
    let p = auth_path();
    if !pi_js::fs::exists(&p) {
        return false;
    }
    let Ok(content) = pi_js::fs::read_to_string(&p) else {
        return false;
    };
    let Ok(Value::Object(map)) = pi_js::json::parse(&content) else {
        return false;
    };
    map.contains_key(provider)
}

pub fn get_real_auth_storage() -> AuthStorage {
    AuthStorage::create(Some(&auth_path()))
}

pub async fn resolve_api_key(provider: &str) -> Option<String> {
    let p = auth_path();
    if !pi_js::fs::exists(&p) {
        return None;
    }
    let content = pi_js::fs::read_to_string(&p).ok()?;
    let val = pi_js::json::parse(&content).ok()?;
    let entry = val.get(provider)?;
    let cred: Credential = serde_json::from_value(entry.clone()).ok()?;
    match cred {
        Credential::ApiKey(api) => api.key,
        Credential::OAuth(oauth) => Some(oauth.access),
    }
}

/// Create a minimal user message for testing.
pub fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: pi_js::time::now_ms(),
    }))
}

/// Create a minimal assistant message for testing.
pub fn assistant_msg(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(text))],
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        model: "test".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 2,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        duration_ms: None,
        timestamp: pi_js::time::now_ms(),
    }))
}

/// Read a session JSONL file and return one label per record: the message role for
/// message entries, otherwise the entry type (e.g. "session", "model_change").
pub fn read_session_file_roles(file: &str) -> Result<Vec<String>> {
    let raw = pi_js::fs::read_to_string(file)?;
    let trimmed = pi_js::str16::trim(&raw);
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut roles = Vec::new();
    for line in trimmed.split('\n') {
        let record = pi_js::json::parse(line)?;
        if let Some(role) = record
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(Value::as_str)
        {
            roles.push(role.to_owned());
        } else if let Some(kind) = record.get("type").and_then(Value::as_str) {
            roles.push(kind.to_owned());
        } else {
            return Err(Error::msg("Session record has neither message.role nor type"));
        }
    }
    Ok(roles)
}

#[derive(Clone, Debug, Default)]
pub struct TestSessionOptions {
    pub in_memory: bool,
    pub system_prompt: Option<String>,
    pub settings_overrides: Option<Value>,
}

pub type TestCommandContext = ExtensionCommandContext;

pub type TestCommandHandler = Arc<dyn Fn(String, TestCommandContext) -> BoxFuture<Result<()>> + Send + Sync>;

#[derive(Clone)]
pub struct TestRegisteredCommandOptions {
    pub description: String,
    pub handler: TestCommandHandler,
}

#[derive(Clone)]
pub struct TestRegisteredCommand {
    pub name: String,
    pub invocation_name: String,
    pub description: String,
    pub source_info: SourceInfo,
    pub handler: TestCommandHandler,
}

#[derive(Clone)]
pub struct TestExtensionApi {
    api: ExtensionApi,
}

impl TestExtensionApi {
    pub fn register_command<F, Fut>(&self, name: &str, description: &str, handler: F)
    where
        F: Fn(String, TestCommandContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let _ = self.api.register_command(
            name,
            RegisteredCommandOptions {
                description: Some(description.to_owned()),
                get_argument_completions: None,
                handler: Arc::new(move |args, ctx| Box::pin(handler(args, ctx))),
            },
        );
    }
}

#[derive(Clone)]
pub struct TestExtensionRunner {
    pub inner: ExtensionRunner,
}

impl TestExtensionRunner {
    pub fn get_registered_commands(&self) -> Vec<TestRegisteredCommand> {
        self.inner
            .get_registered_commands()
            .into_iter()
            .map(|resolved| TestRegisteredCommand {
                name: resolved.command.name,
                invocation_name: resolved.invocation_name,
                description: resolved.command.description.unwrap_or_default(),
                source_info: resolved.command.source_info,
                handler: resolved.command.handler,
            })
            .collect()
    }

    pub fn get_command(&self, invocation_name: &str) -> Option<TestRegisteredCommand> {
        self.inner
            .get_command(invocation_name)
            .map(|resolved| TestRegisteredCommand {
                name: resolved.command.name,
                invocation_name: resolved.invocation_name,
                description: resolved.command.description.unwrap_or_default(),
                source_info: resolved.command.source_info,
                handler: resolved.command.handler,
            })
    }

    pub fn create_command_context(&self) -> TestCommandContext {
        self.inner.create_command_context()
    }
}

pub type TestExtensionFactory = Arc<dyn Fn(TestExtensionApi) -> BoxFuture<Result<()>> + Send + Sync>;

#[derive(Clone)]
pub struct CreateTestExtensionsResultInput {
    pub factory: TestExtensionFactory,
    pub path: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone)]
pub enum TestExtensionInput {
    Factory(TestExtensionFactory),
    Named(CreateTestExtensionsResultInput),
}

#[derive(Clone)]
pub struct LoadExtensionsResult {
    pub extensions: Vec<String>,
    pub errors: Vec<String>,
    pub event_bus: Arc<LocalEventBus>,
    pub core: CoreLoadExtensionsResult,
    pub runner: TestExtensionRunner,
}

pub async fn create_test_extensions_result(
    inputs: Vec<TestExtensionInput>,
    cwd: Option<&str>,
) -> Result<LoadExtensionsResult> {
    let event_bus = create_event_bus();
    let bus_dyn: Arc<dyn pi_coding_agent::core::event_bus::EventBus> = event_bus.clone();
    let runtime = create_extension_runtime();
    let cwd_str = cwd.unwrap_or(".");
    let mut loaded_extensions = Vec::new();
    let mut extensions = Vec::new();
    let mut errors = Vec::new();

    for (index, input) in inputs.into_iter().enumerate() {
        let (factory, extension_path) = match input {
            TestExtensionInput::Factory(f) => (f, format!("<inline:{}>", index + 1)),
            TestExtensionInput::Named(n) => {
                let p = if let Some(name) = n.name {
                    format!("<inline:{name}>")
                } else if let Some(path) = n.path.filter(|p| !p.is_empty()) {
                    path
                } else {
                    format!("<inline:{}>", index + 1)
                };
                (n.factory, p)
            }
        };
        let ext_factory: ExtensionFactory = Arc::new(move |api: ExtensionApi| {
            let f = Arc::clone(&factory);
            Box::pin(async move { f(TestExtensionApi { api }).await })
        });
        match load_extension_from_factory(
            ext_factory,
            cwd_str,
            Arc::clone(&bus_dyn),
            runtime.clone(),
            Some(&extension_path),
        )
        .await
        {
            Ok(loaded) => {
                extensions.push(loaded.path.clone());
                loaded_extensions.push(loaded);
            }
            Err(err) => errors.push(err.to_string()),
        }
    }

    let core = CoreLoadExtensionsResult {
        extensions: loaded_extensions.clone(),
        errors: Vec::new(),
        warnings: Vec::new(),
        runtime: runtime.clone(),
    };
    let auth_storage = Arc::new(AuthStorage::in_memory(None));
    let model_registry = create_in_memory_model_registry(auth_storage).await?;
    let sm = SessionManager::in_memory(Some(cwd_str), None, None)?;
    let runner = ExtensionRunner::new(loaded_extensions, runtime, cwd_str, sm, model_registry);

    Ok(LoadExtensionsResult {
        extensions,
        errors,
        event_bus,
        core,
        runner: TestExtensionRunner { inner: runner },
    })
}

#[derive(Clone, Default)]
pub struct CreateTestResourceLoaderOptions {
    pub extensions_result: Option<LoadExtensionsResult>,
}

#[derive(Clone)]
pub struct TestResourceLoader {
    extensions_result: LoadExtensionsResult,
}

impl TestResourceLoader {
    pub fn get_extensions(&self) -> &LoadExtensionsResult {
        &self.extensions_result
    }
}

impl ResourceLoader for TestResourceLoader {
    fn get_extensions(&self) -> LoaderLoadExtensionsResult {
        LoaderLoadExtensionsResult::default()
    }

    fn get_core_extensions(&self) -> CoreLoadExtensionsResult {
        self.extensions_result.core.clone()
    }

    fn get_skills(&self) -> (Vec<Skill>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }

    fn get_prompts(&self) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }

    fn get_themes(&self) -> (Vec<LoaderTheme>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }

    fn get_agents_files(&self) -> Vec<ContextFile> {
        Vec::new()
    }

    fn get_system_prompt(&self) -> Option<String> {
        None
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

pub fn create_test_resource_loader(options: Option<CreateTestResourceLoaderOptions>) -> TestResourceLoader {
    let extensions_result = options.and_then(|o| o.extensions_result).unwrap_or_else(|| {
        let runtime = create_extension_runtime();
        let core = CoreLoadExtensionsResult {
            extensions: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            runtime: runtime.clone(),
        };
        let model_registry = pi_coding_agent::core::model_registry::ModelRegistry::in_memory();
        let sm = SessionManager::in_memory(None, None, None).unwrap_or_else(|e| panic!("in-memory session: {e}"));
        let runner = ExtensionRunner::new(Vec::new(), runtime, ".", sm, model_registry);
        LoadExtensionsResult {
            extensions: Vec::new(),
            errors: Vec::new(),
            event_bus: create_event_bus(),
            core,
            runner: TestExtensionRunner { inner: runner },
        }
    });
    TestResourceLoader { extensions_result }
}

pub struct TestSessionContext {
    pub session: AgentSession,
    pub session_manager: SessionManager,
    pub settings_manager: SettingsManager,
    pub model_runtime: ModelRuntime,
    pub resource_loader: TestResourceLoader,
    pub temp_dir: String,
}

impl TestSessionContext {
    pub fn cleanup(&self) {
        self.session.dispose();
        if pi_js::fs::exists(&self.temp_dir) {
            let _ = pi_js::fs::rm(&self.temp_dir, true, true);
        }
    }
}

pub async fn create_test_session(options: Option<TestSessionOptions>) -> Result<TestSessionContext> {
    let opts = options.unwrap_or_default();
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-test-"]))?;
    let session_manager = if opts.in_memory {
        SessionManager::in_memory(None, None, None)?
    } else {
        SessionManager::create(&temp_dir, Some(&temp_dir), None)?
    };
    let settings_manager = SettingsManager::create(&temp_dir, Some(&temp_dir), None);
    let auth_path = pi_js::path::join(&[&temp_dir, "auth.json"]);
    let models_path = pi_js::path::join(&[&temp_dir, "models.json"]);
    let auth_storage = Arc::new(AuthStorage::create(Some(&auth_path)));
    let model_registry = create_model_registry(auth_storage, Some(&models_path)).await?;
    let model_runtime = get_model_runtime(&model_registry);
    let resource_loader = create_test_resource_loader(None);
    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(temp_dir.clone()),
        agent_dir: Some(temp_dir.clone()),
        session_manager: Some(session_manager.clone()),
        settings_manager: Some(settings_manager.clone()),
        model_runtime: Some(model_runtime.clone()),
        resource_loader: Some(Arc::new(resource_loader.clone())),
        ..Default::default()
    }))
    .await?;

    Ok(TestSessionContext {
        session: created.session,
        session_manager,
        settings_manager,
        model_runtime,
        resource_loader,
        temp_dir,
    })
}

#[derive(Clone, Debug)]
pub struct TestTreeMessage {
    pub role: &'static str,
    pub text: String,
    pub branch_from: Option<String>,
}

/// Build a session tree for testing using SessionManager.
/// Returns the IDs of all created entries keyed by message text.
pub fn build_test_tree(session: &SessionManager, messages: &[TestTreeMessage]) -> Result<IndexMap<String, String>> {
    let mut ids: IndexMap<String, String> = IndexMap::new();

    for msg in messages {
        if let Some(ref branch_from) = msg.branch_from {
            let branch_from_id = ids
                .get(branch_from)
                .ok_or_else(|| Error::msg(format!("Cannot branch from unknown entry: {branch_from}")))?;
            session.branch(branch_from_id)?;
        }

        let agent_msg = match msg.role {
            "user" => user_msg(&msg.text),
            "assistant" => assistant_msg(&msg.text),
            other => return Err(Error::msg(format!("Unsupported test tree role: {other}"))),
        };
        let id = session.append_message(agent_msg)?;
        ids.insert(msg.text.clone(), id);
    }

    Ok(ids)
}

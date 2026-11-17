//! Local test harness for the coding-agent test suite.
//! Port of packages/coding-agent/test/suite/harness.ts

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_agent_core::agent::{Agent, AgentInitialState, AgentOptions};
use pi_agent_core::types::{AgentMessage, AgentTool, OnPayloadFn, TransformContextFn};
use pi_ai::compat::stream_simple;
use pi_ai::providers::faux::{
    FauxModelDefinition, FauxProviderRegistration, FauxResponseStep, RegisterFauxProviderOptions,
    register_faux_provider,
};
use pi_ai::types::{
    AssistantContentBlock, Model, OnResponseFn, ToolResultMessage, UserContentBlock, UserMessageContent,
};
use pi_ai::{ApiKeyCredential, Credential, CredentialStore};
use pi_coding_agent::core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::{
    AfterProviderResponseEvent, AutocompleteProviderFactory, CustomUiFactory, EditorFactory,
    ExtensionCustomOptions, ExtensionRunnerRef, ExtensionUIContext, ExtensionUIDialogOptions,
    ExtensionWidgetOptions, FooterFactory, HeaderFactory, InlineExtension, NotifyType,
    ProviderChatModelConfig, ProviderConfig, ProviderModelConfig, SetThemeResult, TerminalInputHandler, Theme,
    ThemeInfo, WidgetFactory, WorkingIndicatorOptions, create_extension_runner_ref,
};
use pi_coding_agent::core::messages::convert_to_llm;
use pi_coding_agent::core::resource_loader::ResourceLoader;
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::{Settings, SettingsManager};
use pi_js::Unsubscribe;
use serde_json::Value;

use super::super::model_runtime_test_utils::{
    create_in_memory_model_registry, create_model_registry, get_model_runtime,
};
use super::super::utilities::{
    TestExtensionInput, create_test_extensions_result, create_test_resource_loader,
};

pub fn get_message_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Message(pi_ai::types::Message::User(user)) => match &user.content {
            UserMessageContent::Text(s) => s.clone(),
            UserMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        },
        AgentMessage::Message(pi_ai::types::Message::Assistant(assistant)) => assistant
            .content
            .iter()
            .filter_map(|b| match b {
                AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        AgentMessage::Message(pi_ai::types::Message::ToolResult(tr)) => tr
            .content
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        AgentMessage::Message(pi_ai::types::Message::System(sys)) => sys.content.clone(),
        AgentMessage::Custom(val) => match val.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|p| {
                    if p.get("type").and_then(Value::as_str) == Some("text") {
                        p.get("text").and_then(Value::as_str)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        },
    }
}

pub fn get_user_texts(harness: &Harness) -> Vec<String> {
    harness
        .session
        .messages()
        .into_iter()
        .filter(|m| m.role() == Some("user"))
        .map(|m| get_message_text(&m))
        .collect()
}

pub fn get_assistant_texts(harness: &Harness) -> Vec<String> {
    harness
        .session
        .messages()
        .into_iter()
        .filter(|m| m.role() == Some("assistant"))
        .map(|m| get_message_text(&m))
        .collect()
}

/// The latest result of `tool_name` in the session transcript.
pub fn get_tool_result(harness: &Harness, tool_name: &str) -> ToolResultMessage {
    harness
        .session
        .messages()
        .into_iter()
        .rev()
        .find_map(|m| match m {
            AgentMessage::Message(pi_ai::types::Message::ToolResult(tr)) if tr.tool_name == tool_name => Some(tr),
            _ => None,
        })
        .unwrap_or_else(|| panic!("No {tool_name} tool result"))
}

/// No-op UI context suitable for tests.
#[derive(Default)]
pub struct TestUiContext;

#[async_trait::async_trait]
impl ExtensionUIContext for TestUiContext {
    async fn select(
        &self,
        _title: &str,
        _options: Vec<String>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    async fn confirm(
        &self,
        _title: &str,
        _message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<bool> {
        Ok(false)
    }

    async fn input(
        &self,
        _title: &str,
        _placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    fn notify(&self, _message: &str, _notify_type: Option<NotifyType>) {}

    fn on_terminal_input(&self, _handler: TerminalInputHandler) -> Unsubscribe {
        Unsubscribe::noop()
    }

    fn set_status(&self, _key: &str, _text: Option<&str>) {}

    fn set_working_message(&self, _message: Option<&str>) {}

    fn set_working_visible(&self, _visible: bool) {}

    fn set_working_indicator(&self, _options: Option<WorkingIndicatorOptions>) {}

    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}

    fn set_widget_lines(&self, _key: &str, _content: Option<Vec<String>>, _options: Option<ExtensionWidgetOptions>) {}

    fn set_widget_factory(
        &self,
        _key: &str,
        _factory: Option<WidgetFactory>,
        _options: Option<ExtensionWidgetOptions>,
    ) {
    }

    fn set_footer(&self, _factory: Option<FooterFactory>) {}

    fn set_header(&self, _factory: Option<HeaderFactory>) {}

    fn set_title(&self, _title: &str) {}

    async fn custom(&self, _factory: CustomUiFactory, _options: Option<ExtensionCustomOptions>) -> pi_js::Result<Value> {
        Ok(Value::Null)
    }

    fn paste_to_editor(&self, _text: &str) {}

    fn set_editor_text(&self, _text: &str) {}

    fn get_editor_text(&self) -> String {
        String::new()
    }

    async fn editor(&self, _title: &str, _prefill: Option<&str>) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    fn add_autocomplete_provider(&self, _factory: AutocompleteProviderFactory) {}

    fn set_editor_component(&self, _factory: Option<EditorFactory>) {}

    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }

    fn theme(&self) -> Theme {
        Theme::default()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }

    fn get_theme(&self, _name: &str) -> Option<Theme> {
        None
    }

    fn set_theme_by_name(&self, _name: &str) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: Some("Theme switching not available in tests".to_string()),
        }
    }

    fn set_theme(&self, _theme: Theme) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: Some("Theme switching not available in tests".to_string()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        false
    }

    fn set_tools_expanded(&self, _expanded: bool) {}
}

pub fn create_test_ui_context() -> Arc<dyn ExtensionUIContext> {
    Arc::new(TestUiContext)
}

#[derive(Clone, Default)]
pub struct HarnessOptions {
    pub models: Option<Vec<FauxModelDefinition>>,
    pub settings: Option<Settings>,
    pub tools: Option<Vec<AgentTool>>,
    pub initial_active_tool_names: Option<Vec<String>>,
    pub allowed_tool_names: Option<Vec<String>>,
    pub excluded_tool_names: Option<Vec<String>>,
    pub resource_loader: Option<Arc<dyn ResourceLoader>>,
    pub extension_factories: Option<Vec<InlineExtension>>,
    pub test_extensions: Option<Vec<TestExtensionInput>>,
    pub with_configured_auth: Option<bool>,
    pub models_json: Option<Value>,
    pub session_manager: Option<SessionManager>,
}

pub struct Harness {
    pub session: AgentSession,
    pub session_manager: SessionManager,
    pub settings_manager: SettingsManager,
    pub auth_storage: Arc<AuthStorage>,
    pub faux: FauxProviderRegistration,
    pub models: Vec<Model>,
    events: Arc<Mutex<Vec<AgentSessionEvent>>>,
    pub temp_dir: String,
    _unsub: Unsubscribe,
}

impl Harness {
    pub fn get_model(&self) -> Model {
        self.faux.get_model()
    }

    pub fn get_model_by_id(&self, model_id: &str) -> Option<Model> {
        self.faux.get_model_model_id(model_id)
    }

    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.faux.set_responses(responses);
    }

    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.faux.append_responses(responses);
    }

    pub fn get_pending_response_count(&self) -> i64 {
        self.faux.get_pending_response_count()
    }

    pub fn events(&self) -> Vec<AgentSessionEvent> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn cleanup(self) {
        self.session.dispose();
        self.faux.unregister();
        if pi_js::fs::exists(&self.temp_dir) {
            let _ = pi_js::fs::rm(&self.temp_dir, true, true);
        }
    }
}

pub async fn create_harness(options: HarnessOptions) -> Harness {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-suite-"])).unwrap();
    let faux_provider = register_faux_provider(Some(RegisterFauxProviderOptions {
        models: options.models,
        ..Default::default()
    }));
    faux_provider.set_responses(vec![]);
    let model = faux_provider.get_model();

    let tool_map = options.tools.map(|tools| {
        tools
            .into_iter()
            .map(|t| (t.name.clone(), t))
            .collect::<IndexMap<String, AgentTool>>()
    });
    let with_configured_auth = options.with_configured_auth.unwrap_or(true);
    let extension_runner_ref: ExtensionRunnerRef = create_extension_runner_ref();

    let session_manager = options
        .session_manager
        .unwrap_or_else(|| SessionManager::in_memory(Some(&temp_dir), None, None).unwrap());
    let settings_manager = SettingsManager::in_memory(options.settings, None);

    let auth_storage = Arc::new(AuthStorage::in_memory(None));
    if with_configured_auth {
        auth_storage
            .modify(
                &model.provider,
                Box::new(|_prev| {
                    Box::pin(async move {
                        Ok(Some(Credential::ApiKey(ApiKeyCredential {
                            key: Some("faux-key".to_string()),
                            env: None,
                        })))
                    })
                }),
                None,
            )
            .await
            .unwrap();
    }

    let models_path = options
        .models_json
        .as_ref()
        .map(|_| pi_js::path::join(&[&temp_dir, "models.json"]));
    if let (Some(path), Some(val)) = (&models_path, &options.models_json) {
        pi_js::fs::write_file(path, pi_js::json::stringify(val).as_bytes(), None).unwrap();
    }
    let model_registry = if let Some(ref path) = models_path {
        create_model_registry(auth_storage.clone(), Some(path)).await
    } else {
        create_in_memory_model_registry(auth_storage.clone()).await
    };
    if with_configured_auth {
        let models = faux_provider
            .models()
            .iter()
            .map(|m| {
                ProviderModelConfig::Chat(ProviderChatModelConfig {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    api: Some(m.api.clone()),
                    base_url: Some(m.base_url.clone()),
                    input: m.input.clone(),
                    input_limits: m.input_limits.clone(),
                    cost: m.cost.clone(),
                    headers: None,
                    reasoning: m.reasoning,
                    thinking_level_map: m.thinking_level_map.clone(),
                    prompt_cache: m.prompt_cache.clone(),
                    context_window: m.context_window,
                    max_tokens: m.max_tokens,
                    sampling_params: m.sampling_params.clone(),
                    compat: m.compat.clone(),
                })
            })
            .collect();
        model_registry.register_provider(
            &model.provider,
            ProviderConfig {
                base_url: Some(model.base_url.clone()),
                api_key: Some("faux-key".to_string()),
                api: Some(faux_provider.api().to_string()),
                models: Some(models),
                ..Default::default()
            },
            None,
        );
    }

    let runner_for_payload = Arc::clone(&extension_runner_ref);
    let on_payload: OnPayloadFn = Arc::new(move |payload, _model| {
        let runner_opt = runner_for_payload.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Box::pin(async move {
            let Some(runner) = runner_opt else {
                return Ok(None);
            };
            if !runner.has_handlers("before_provider_request") {
                return Ok(None);
            }
            Ok(Some(runner.emit_before_provider_request(payload).await?))
        })
    });

    let runner_for_response = Arc::clone(&extension_runner_ref);
    let on_response: OnResponseFn<Model> = Arc::new(move |response, _model| {
        let runner_opt = runner_for_response.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Box::pin(async move {
            let Some(runner) = runner_opt else {
                return Ok(());
            };
            if !runner.has_handlers("after_provider_response") {
                return Ok(());
            }
            runner
                .emit_after_provider_response(AfterProviderResponseEvent {
                    status: response.status,
                    headers: response.headers,
                })
                .await
        })
    });

    let runner_for_ctx = Arc::clone(&extension_runner_ref);
    let transform_context: TransformContextFn = Arc::new(move |messages, _signal| {
        let runner_opt = runner_for_ctx.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Box::pin(async move {
            let Some(runner) = runner_opt else {
                return Ok(messages);
            };
            runner.emit_context(messages).await
        })
    });

    let get_api_key = Arc::new(move |_provider: &str| {
        Box::pin(async move {
            Ok(if with_configured_auth {
                Some("faux-key".to_string())
            } else {
                None
            })
        })
    });

    let on_response_cb = Arc::clone(&on_response);
    let stream_fn = Arc::new(move |req_model: Model, ctx, opts: Option<pi_ai::SimpleStreamOptions>| {
        let cb = Arc::clone(&on_response_cb);
        Box::pin(async move {
            let mut next_opts = opts.unwrap_or_default();
            if next_opts.base.on_response.is_none() {
                next_opts.base.on_response = Some(cb);
            }
            Ok(stream_simple(&req_model, &ctx, Some(next_opts)))
        })
    });

    let agent = Agent::new(AgentOptions {
        get_api_key: Some(get_api_key),
        stream_fn: Some(stream_fn),
        initial_state: Some(AgentInitialState {
            model: Some(model.clone()),
            system_prompt: Some(String::new()),
            tools: Some(vec![]),
            ..Default::default()
        }),
        convert_to_llm: Some(Arc::new(|msgs| convert_to_llm(&msgs))),
        on_payload: Some(on_payload),
        transform_context: Some(transform_context),
        ..Default::default()
    });

    let mut ext_inputs: Vec<TestExtensionInput> = Vec::new();
    if let Some(inline_exts) = options.extension_factories {
        ext_inputs.extend(inline_exts.into_iter().map(TestExtensionInput::Inline));
    }
    if let Some(test_exts) = options.test_extensions {
        ext_inputs.extend(test_exts);
    }
    let extensions_result = if ext_inputs.is_empty() {
        None
    } else {
        Some(create_test_extensions_result(&ext_inputs, &temp_dir).await)
    };
    let resource_loader = options
        .resource_loader
        .unwrap_or_else(|| create_test_resource_loader(extensions_result));

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager: session_manager.clone(),
        settings_manager: settings_manager.clone(),
        cwd: temp_dir.clone(),
        model_runtime: get_model_runtime(&model_registry),
        resource_loader,
        base_tools_override: tool_map,
        initial_active_tool_names: options.initial_active_tool_names,
        allowed_tool_names: options.allowed_tool_names,
        excluded_tool_names: options.excluded_tool_names,
        extension_runner_ref: Some(extension_runner_ref),
        scoped_models: None,
        custom_tools: None,
        cache_warmer: None,
        uses_default_tools: None,
        session_start_event: None,
        initial_model: Some(model),
        initial_thinking_level: None,
    });

    let events = Arc::new(Mutex::new(Vec::new()));
    let events_ref = Arc::clone(&events);
    let unsub = session.subscribe(move |ev| {
        events_ref.lock().unwrap_or_else(PoisonError::into_inner).push(ev);
    });

    let models = faux_provider.models().to_vec();
    Harness {
        session,
        session_manager,
        settings_manager,
        auth_storage,
        faux: faux_provider,
        models,
        events,
        temp_dir,
        _unsub: unsub,
    }
}

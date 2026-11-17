// PORT: QuickJS plugin runtime enabled per user requirement (overrides §10.6)
//! End-to-end tests loading and executing unmodified upstream TypeScript extension
//! examples inside the QuickJS WASI plugin host.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::extensions::mcp_wire::{
    ModelRegistry, SessionManager, SourceInfo, SourceOrigin, SourceScope, create_event_bus,
};
use pi_coding_agent::core::extensions::{
    AutocompleteProviderFactory, CustomUiFactory, EditorFactory, ExtensionActions, ExtensionContextActions,
    ExtensionCustomOptions, ExtensionMode, ExtensionRunner, ExtensionUIContext, ExtensionUIDialogOptions,
    ExtensionWidgetOptions, FooterFactory, HeaderFactory, NotifyType, SessionStartEvent, SessionStartReason,
    SetThemeResult, SlashCommandInfo, SlashCommandSource, TerminalInputHandler, Theme, ThemeInfo, ToolCallEvent,
    WidgetFactory, WorkingIndicatorOptions, load_extensions, wrap_registered_tool,
};
use pi_coding_agent::core::tools::bash::BashToolInput;
use pi_js::Unsubscribe;
use serde_json::Value;

fn example_path(file: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("package/examples/extensions")
        .join(file)
        .to_string_lossy()
        .into_owned()
}

fn default_actions(commands: Arc<Mutex<Vec<SlashCommandInfo>>>) -> ExtensionActions {
    ExtensionActions {
        send_message: Arc::new(|_, _| {}),
        send_user_message: Arc::new(|_, _| {}),
        append_entry: Arc::new(|_, _| {}),
        set_session_name: Arc::new(|_| {}),
        get_session_name: Arc::new(|| None),
        set_label: Arc::new(|_, _| {}),
        get_active_tools: Arc::new(Vec::new),
        get_all_tools: Arc::new(Vec::new),
        get_settings: Arc::new(Default::default),
        set_active_tools: Arc::new(|_| {}),
        refresh_tools: Arc::new(|| {}),
        get_commands: Arc::new(move || commands.lock().unwrap().clone()),
        set_model: Arc::new(|_| Box::pin(async { Ok(false) })),
        get_thinking_level: Arc::new(|| pi_agent_core::types::ThinkingLevel::Off),
        set_thinking_level: Arc::new(|_| {}),
    }
}

fn default_ctx_actions() -> ExtensionContextActions {
    ExtensionContextActions {
        get_model: Arc::new(|| None),
        get_scoped_models: Arc::new(Vec::new),
        is_idle: Arc::new(|| true),
        is_project_trusted: Arc::new(|| true),
        get_signal: Arc::new(|| None),
        abort: Arc::new(|| {}),
        has_pending_messages: Arc::new(|| false),
        shutdown: Arc::new(|| {}),
        get_context_usage: Arc::new(|| None),
        compact: Arc::new(|_| {}),
        get_system_prompt: Arc::new(String::new),
        get_system_prompt_options: None,
        execute_tool: None,
        get_callable_tools: None,
    }
}

#[derive(Default)]
struct RecordedUiState {
    select_calls: Vec<(String, Vec<String>)>,
    confirm_calls: Vec<(String, String)>,
    notifications: Vec<(String, Option<NotifyType>)>,
    select_responses: VecDeque<Option<String>>,
    confirm_responses: VecDeque<bool>,
}

struct MockUiContext {
    state: Arc<Mutex<RecordedUiState>>,
}

#[async_trait]
impl ExtensionUIContext for MockUiContext {
    async fn select(
        &self,
        title: &str,
        options: Vec<String>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        let mut st = self.state.lock().unwrap();
        st.select_calls.push((title.to_string(), options));
        Ok(st.select_responses.pop_front().flatten())
    }

    async fn confirm(
        &self,
        title: &str,
        message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<bool> {
        let mut st = self.state.lock().unwrap();
        st.confirm_calls.push((title.to_string(), message.to_string()));
        Ok(st.confirm_responses.pop_front().unwrap_or(false))
    }

    async fn input(
        &self,
        _title: &str,
        _placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    fn notify(&self, message: &str, notify_type: Option<NotifyType>) {
        self.state
            .lock()
            .unwrap()
            .notifications
            .push((message.to_string(), notify_type));
    }

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
    async fn custom(
        &self,
        _factory: CustomUiFactory,
        _options: Option<ExtensionCustomOptions>,
    ) -> pi_js::Result<Value> {
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
            error: Some("mock".to_string()),
        }
    }
    fn set_theme(&self, _theme: Theme) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: Some("mock".to_string()),
        }
    }
    fn get_tools_expanded(&self) -> bool {
        false
    }
    fn set_tools_expanded(&self, _expanded: bool) {}
}

#[tokio::test]
async fn executes_hello_ts_example_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let result = load_extensions(&[example_path("hello.ts")], cwd, None, None).await;
    assert_eq!(result.errors, vec![], "load errors: {:?}", result.errors);
    assert_eq!(result.extensions.len(), 1);

    let runner = ExtensionRunner::new(
        result.extensions,
        result.runtime,
        cwd,
        SessionManager::default(),
        ModelRegistry::default(),
    );
    runner.bind_core(
        default_actions(Arc::new(Mutex::new(Vec::new()))),
        default_ctx_actions(),
        None,
    );

    let tools = runner.get_all_registered_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].definition.name, "hello");
    assert_eq!(tools[0].definition.parameters.to_value()["type"], "object");

    let agent_tool = wrap_registered_tool(&tools[0], &runner);
    let out = (agent_tool.execute)("call-1".to_string(), serde_json::json!({ "name": "World" }), None, None)
        .await
        .expect("execute hello");

    assert_eq!(out.content.len(), 1);
    let UserContentBlock::Text(text) = &out.content[0] else {
        panic!("expected text block");
    };
    assert_eq!(text.text, "Hello, World!");
    assert_eq!(out.details, Some(serde_json::json!({ "greeted": "World" })));
}

#[tokio::test]
async fn executes_commands_ts_example_completions_and_ui_flow() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let path = example_path("commands.ts");
    let result = load_extensions(&[path.clone()], cwd, None, None).await;
    assert_eq!(result.errors, vec![], "load errors: {:?}", result.errors);

    let commands_list = Arc::new(Mutex::new(vec![SlashCommandInfo {
        name: "hello".to_string(),
        description: Some("Say hello".to_string()),
        source: SlashCommandSource::Extension,
        source_info: SourceInfo {
            path: "/ext/hello.ts".to_string(),
            source: "local".to_string(),
            scope: SourceScope::Project,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        },
    }]));

    let runner = ExtensionRunner::new(
        result.extensions,
        result.runtime,
        cwd,
        SessionManager::default(),
        ModelRegistry::default(),
    );
    runner.bind_core(default_actions(Arc::clone(&commands_list)), default_ctx_actions(), None);

    let ui_state = Arc::new(Mutex::new(RecordedUiState::default()));
    {
        let mut st = ui_state.lock().unwrap();
        st.select_responses.push_back(Some("/hello - Say hello".to_string()));
        st.confirm_responses.push_back(true);
    }
    runner.set_ui_context(
        Some(Arc::new(MockUiContext {
            state: Arc::clone(&ui_state),
        })),
        ExtensionMode::Tui,
    );

    let resolved = runner.get_command("commands").expect("commands command");
    let completions = resolved
        .command
        .get_argument_completions
        .as_ref()
        .expect("argument completions")("ex")
    .await
    .expect("completion items");
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].value, "extension");

    // 1. Empty filter -> selects /hello -> confirms -> notifies path
    (resolved.command.handler)(String::new(), runner.create_command_context())
        .await
        .expect("run /commands");

    // 2. Non-matching filter -> notifies "No prompt commands found"
    (resolved.command.handler)("prompt".to_string(), runner.create_command_context())
        .await
        .expect("run /commands prompt");

    let st = ui_state.lock().unwrap();
    assert_eq!(st.select_calls.len(), 1);
    assert_eq!(st.select_calls[0].0, "Available Commands");
    assert_eq!(
        st.select_calls[0].1,
        vec!["--- Extensions ---".to_string(), "/hello - Say hello".to_string()]
    );
    assert_eq!(st.confirm_calls.len(), 1);
    assert_eq!(st.confirm_calls[0].0, "hello");
    assert!(st.notifications.iter().any(|(m, _)| m == "/ext/hello.ts"));
    assert!(st.notifications.iter().any(|(m, _)| m == "No prompt commands found"));
}

#[tokio::test]
async fn executes_dynamic_tools_ts_example_runtime_registration() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let result = load_extensions(&[example_path("dynamic-tools.ts")], cwd, None, None).await;
    assert_eq!(result.errors, vec![], "load errors: {:?}", result.errors);

    let runner = ExtensionRunner::new(
        result.extensions,
        result.runtime,
        cwd,
        SessionManager::default(),
        ModelRegistry::default(),
    );
    runner.bind_core(
        default_actions(Arc::new(Mutex::new(Vec::new()))),
        default_ctx_actions(),
        None,
    );
    let ui_state = Arc::new(Mutex::new(RecordedUiState::default()));
    runner.set_ui_context(
        Some(Arc::new(MockUiContext {
            state: Arc::clone(&ui_state),
        })),
        ExtensionMode::Tui,
    );

    assert!(runner.get_all_registered_tools().is_empty());

    // session_start registers `echo_session` dynamically
    runner
        .emit(SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        })
        .await;

    let session_tool = runner
        .get_all_registered_tools()
        .into_iter()
        .find(|t| t.definition.name == "echo_session")
        .expect("echo_session registered on session_start");
    let wrapped_session = wrap_registered_tool(&session_tool, &runner);
    let session_out =
        (wrapped_session.execute)("call-s".to_string(), serde_json::json!({ "message": "hi" }), None, None)
            .await
            .unwrap();
    let UserContentBlock::Text(t) = &session_out.content[0] else {
        panic!("expected text");
    };
    assert_eq!(t.text, "[session] hi");

    // /add-echo-tool registers another tool dynamically at runtime
    let add_cmd = runner.get_command("add-echo-tool").expect("add-echo-tool command");
    (add_cmd.command.handler)("bad-name!".to_string(), runner.create_command_context())
        .await
        .unwrap();
    assert_eq!(runner.get_all_registered_tools().len(), 1);

    (add_cmd.command.handler)("custom_ping".to_string(), runner.create_command_context())
        .await
        .unwrap();
    let custom_tool = runner
        .get_all_registered_tools()
        .into_iter()
        .find(|t| t.definition.name == "custom_ping")
        .expect("custom_ping registered dynamically");
    let wrapped_custom = wrap_registered_tool(&custom_tool, &runner);
    let custom_out = (wrapped_custom.execute)(
        "call-c".to_string(),
        serde_json::json!({ "message": "pong" }),
        None,
        None,
    )
    .await
    .unwrap();
    let UserContentBlock::Text(t2) = &custom_out.content[0] else {
        panic!("expected text");
    };
    assert_eq!(t2.text, "[custom_ping] pong");
}

#[tokio::test]
async fn executes_permission_gate_ts_example_blocking_and_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let result = load_extensions(&[example_path("permission-gate.ts")], cwd, None, None).await;
    assert_eq!(result.errors, vec![], "load errors: {:?}", result.errors);

    let runner = ExtensionRunner::new(
        result.extensions,
        result.runtime,
        cwd,
        SessionManager::default(),
        ModelRegistry::default(),
    );

    // Non-dangerous command -> allowed without UI
    let mut safe_ev = ToolCallEvent::Bash(pi_coding_agent::core::extensions::BashToolCallEvent {
        tool_call_id: "t1".to_string(),
        parent_tool_call_id: None,
        input: BashToolInput {
            command: "ls -la".to_string(),
            timeout: None,
        },
    });
    assert_eq!(runner.emit_tool_call(&mut safe_ev).await.unwrap(), None);

    // Dangerous without UI -> blocked automatically
    let mut rm_ev = ToolCallEvent::Bash(pi_coding_agent::core::extensions::BashToolCallEvent {
        tool_call_id: "t2".to_string(),
        parent_tool_call_id: None,
        input: BashToolInput {
            command: "rm -rf /tmp/danger".to_string(),
            timeout: None,
        },
    });
    let no_ui_res = runner.emit_tool_call(&mut rm_ev).await.unwrap().expect("blocked");
    assert_eq!(no_ui_res.block, Some(true));
    assert!(no_ui_res.reason.unwrap().contains("no UI"));

    // Interactive UI: user clicks No then Yes
    let ui_state = Arc::new(Mutex::new(RecordedUiState::default()));
    {
        let mut st = ui_state.lock().unwrap();
        st.select_responses.push_back(Some("No".to_string()));
        st.select_responses.push_back(Some("Yes".to_string()));
    }
    runner.set_ui_context(
        Some(Arc::new(MockUiContext {
            state: Arc::clone(&ui_state),
        })),
        ExtensionMode::Tui,
    );

    let mut sudo_ev = ToolCallEvent::Bash(pi_coding_agent::core::extensions::BashToolCallEvent {
        tool_call_id: "t3".to_string(),
        parent_tool_call_id: None,
        input: BashToolInput {
            command: "sudo reboot".to_string(),
            timeout: None,
        },
    });
    let denied = runner.emit_tool_call(&mut sudo_ev).await.unwrap().expect("denied");
    assert_eq!(denied.block, Some(true));
    assert_eq!(denied.reason.as_deref(), Some("Blocked by user"));

    let mut chmod_ev = ToolCallEvent::Bash(pi_coding_agent::core::extensions::BashToolCallEvent {
        tool_call_id: "t4".to_string(),
        parent_tool_call_id: None,
        input: BashToolInput {
            command: "chmod 777 /tmp/ok".to_string(),
            timeout: None,
        },
    });
    assert_eq!(runner.emit_tool_call(&mut chmod_ev).await.unwrap(), None);
}

#[tokio::test]
async fn executes_event_bus_ts_example_session_start_and_emit_command() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let bus = create_event_bus() as Arc<dyn pi_coding_agent::core::extensions::mcp_wire::EventBus>;
    let result = load_extensions(&[example_path("event-bus.ts")], cwd, Some(bus), None).await;
    assert_eq!(result.errors, vec![], "load errors: {:?}", result.errors);

    let runner = ExtensionRunner::new(
        result.extensions,
        result.runtime,
        cwd,
        SessionManager::default(),
        ModelRegistry::default(),
    );
    let ui_state = Arc::new(Mutex::new(RecordedUiState::default()));
    runner.set_ui_context(
        Some(Arc::new(MockUiContext {
            state: Arc::clone(&ui_state),
        })),
        ExtensionMode::Tui,
    );

    // session_start captures currentCtx and emits initial notification on the bus
    runner
        .emit(SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        })
        .await;

    // Later /emit command emits on the bus, proving stored currentCtx stays valid
    let emit_cmd = runner.get_command("emit").expect("/emit command");
    (emit_cmd.command.handler)("hello world".to_string(), runner.create_command_context())
        .await
        .unwrap();

    let st = ui_state.lock().unwrap();
    let msgs: Vec<_> = st.notifications.iter().map(|(m, _)| m.as_str()).collect();
    assert!(
        msgs.contains(&"Event from event-bus-example: Session started"),
        "missing session_start event: {msgs:?}"
    );
    assert!(
        msgs.contains(&"Event from /emit command: hello world"),
        "missing /emit event: {msgs:?}"
    );
}

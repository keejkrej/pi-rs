// PORT: QuickJS plugin runtime enabled per user requirement (overrides §10.6)
//! End-to-end tests loading and executing unmodified upstream TypeScript extension
//! examples inside the QuickJS WASI plugin host.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::event_bus::{EventBus, create_event_bus};
use pi_coding_agent::core::extensions::{
    AfterProviderResponseEvent, AgentStartEvent, AutocompleteProviderFactory, BashToolCallEvent, CustomUiFactory,
    EditorFactory, ExtensionActions, ExtensionContextActions, ExtensionCustomOptions, ExtensionMode, ExtensionRunner,
    ExtensionUIContext, ExtensionUIDialogOptions, ExtensionWidgetOptions, FooterFactory, HeaderFactory,
    InputEventResult, InputSource, NotifyType, SessionShutdownEvent, SessionShutdownReason, SessionStartEvent,
    SessionStartReason, SetThemeResult, SlashCommandInfo, SlashCommandSource, TerminalInputHandler, Theme, ThemeInfo,
    ToolCallEvent, WidgetFactory, WorkingIndicatorOptions, WriteToolCallEvent, load_extensions, wrap_registered_tool,
};
use pi_coding_agent::core::model_registry::ModelRegistry;
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use pi_coding_agent::core::tools::bash::BashToolInput;
use pi_coding_agent::core::tools::write::WriteToolInput;
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
    statuses: IndexMap<String, Option<String>>,
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
    fn set_status(&self, key: &str, text: Option<&str>) {
        self.state
            .lock()
            .unwrap()
            .statuses
            .insert(key.to_string(), text.map(str::to_string));
    }
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
            changelog_path: None,
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
    let bus = create_event_bus() as Arc<dyn EventBus>;
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

#[tokio::test]
async fn executes_protected_paths_ts_example() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let result = load_extensions(&[example_path("protected-paths.ts")], cwd, None, None).await;
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

    let mut env_write = ToolCallEvent::Write(WriteToolCallEvent {
        tool_call_id: "w1".to_string(),
        parent_tool_call_id: None,
        input: WriteToolInput {
            path: "/tmp/project/.env".to_string(),
            content: "SECRET=1".to_string(),
        },
    });
    let env_res = runner.emit_tool_call(&mut env_write).await.unwrap().expect("blocked");
    assert_eq!(env_res.block, Some(true));
    assert_eq!(
        env_res.reason.as_deref(),
        Some(r#"Path "/tmp/project/.env" is protected"#)
    );

    let mut git_write = ToolCallEvent::Write(WriteToolCallEvent {
        tool_call_id: "w2".to_string(),
        parent_tool_call_id: None,
        input: WriteToolInput {
            path: "/tmp/project/.git/config".to_string(),
            content: String::new(),
        },
    });
    let git_res = runner.emit_tool_call(&mut git_write).await.unwrap().expect("blocked");
    assert_eq!(git_res.block, Some(true));

    let mut safe_write = ToolCallEvent::Write(WriteToolCallEvent {
        tool_call_id: "w3".to_string(),
        parent_tool_call_id: None,
        input: WriteToolInput {
            path: "/tmp/project/src/main.rs".to_string(),
            content: "fn main() {}".to_string(),
        },
    });
    assert_eq!(runner.emit_tool_call(&mut safe_write).await.unwrap(), None);

    let st = ui_state.lock().unwrap();
    assert_eq!(st.notifications.len(), 2);
    assert_eq!(st.notifications[0].1, Some(NotifyType::Warning));
    assert!(st.notifications[0].0.contains("/tmp/project/.env"));
}

#[tokio::test]
async fn executes_provider_payload_ts_example() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    std::fs::create_dir_all(dir.path().join(".pi")).unwrap();

    let result = load_extensions(&[example_path("provider-payload.ts")], cwd, None, None).await;
    assert_eq!(result.errors, vec![], "load errors: {:?}", result.errors);

    let runner = ExtensionRunner::new(
        result.extensions,
        result.runtime,
        cwd,
        SessionManager::default(),
        ModelRegistry::default(),
    );

    let payload = serde_json::json!({ "model": "claude-sonnet-4", "temperature": 0.5 });
    let out = runner.emit_before_provider_request(payload.clone()).await;
    assert_eq!(out, payload);

    let mut headers = IndexMap::new();
    headers.insert("x-request-id".to_string(), "req-42".to_string());
    runner.emit(AfterProviderResponseEvent { status: 200, headers }).await;

    let log_content = std::fs::read_to_string(dir.path().join(".pi/provider-payload.log")).unwrap();
    assert!(log_content.contains("\"claude-sonnet-4\""), "log={log_content}");
    assert!(
        log_content.contains(r#"[200] {"x-request-id":"req-42"}"#),
        "log={log_content}"
    );
}

#[tokio::test]
async fn executes_system_prompt_header_ts_example() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let result = load_extensions(&[example_path("system-prompt-header.ts")], cwd, None, None).await;
    assert_eq!(result.errors, vec![], "load errors: {:?}", result.errors);

    let runner = ExtensionRunner::new(
        result.extensions,
        result.runtime,
        cwd,
        SessionManager::default(),
        ModelRegistry::default(),
    );
    let mut ctx_actions = default_ctx_actions();
    ctx_actions.get_system_prompt = Arc::new(|| "Custom system prompt!".to_string());
    runner.bind_core(default_actions(Arc::new(Mutex::new(Vec::new()))), ctx_actions, None);

    let ui_state = Arc::new(Mutex::new(RecordedUiState::default()));
    runner.set_ui_context(
        Some(Arc::new(MockUiContext {
            state: Arc::clone(&ui_state),
        })),
        ExtensionMode::Tui,
    );

    runner.emit(AgentStartEvent {}).await;
    {
        let st = ui_state.lock().unwrap();
        assert_eq!(
            st.statuses.get("system-prompt").and_then(|v| v.as_deref()),
            Some("System: 21 chars")
        );
    }

    runner
        .emit(SessionShutdownEvent {
            reason: SessionShutdownReason::Quit,
            target_session_file: None,
        })
        .await;
    {
        let st = ui_state.lock().unwrap();
        assert_eq!(st.statuses.get("system-prompt"), Some(&None));
    }
}

#[tokio::test]
async fn executes_input_transform_ts_example() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let result = load_extensions(&[example_path("input-transform.ts")], cwd, None, None).await;
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

    let passthrough = runner.emit_input("hello", None, InputSource::Interactive, None).await;
    assert_eq!(passthrough, InputEventResult::Continue);

    let ext_skip = runner
        .emit_input("?quick ignored", None, InputSource::Extension, None)
        .await;
    assert_eq!(ext_skip, InputEventResult::Continue);

    let transformed = runner
        .emit_input("?quick What is Rust?", None, InputSource::Interactive, None)
        .await;
    assert_eq!(
        transformed,
        InputEventResult::Transform {
            text: "Respond briefly in 1-2 sentences: What is Rust?".to_string(),
            images: None,
        }
    );

    let empty_quick = runner
        .emit_input("?quick   ", None, InputSource::Interactive, None)
        .await;
    assert_eq!(empty_quick, InputEventResult::Handled);

    let ping = runner.emit_input("PING", None, InputSource::Interactive, None).await;
    assert_eq!(ping, InputEventResult::Handled);

    let st = ui_state.lock().unwrap();
    assert_eq!(
        st.notifications,
        vec![
            ("Usage: ?quick <question>".to_string(), Some(NotifyType::Warning)),
            ("pong".to_string(), Some(NotifyType::Info)),
        ]
    );
}

#[tokio::test]
async fn executes_js_mjs_and_cjs_extensions_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();

    let js_path = dir.path().join("plain.js");
    std::fs::write(
        &js_path,
        r#"export default function(pi) {
            pi.on("tool_call", async (ev) => {
                if (ev.toolName === "bash" && ev.input.command === "block-js") {
                    return { block: true, reason: "blocked by js" };
                }
            });
        }"#,
    )
    .unwrap();

    let mjs_path = dir.path().join("esm.mjs");
    std::fs::write(
        &mjs_path,
        r#"import { Type } from "typebox";
        export default function(pi) {
            pi.registerTool({
                name: "mjs_ping",
                label: "MJS Ping",
                description: "Returns pong",
                parameters: Type.Object({ msg: Type.String() }),
                async execute(_id, params) {
                    return { content: [{ type: "text", text: "mjs:" + params.msg }] };
                }
            });
        }"#,
    )
    .unwrap();

    let cjs_path = dir.path().join("legacy.cjs");
    std::fs::write(
        &cjs_path,
        r#"module.exports = function(pi) {
            pi.on("input", async (ev) => {
                if (ev.text === "cjs") {
                    return { action: "transform", text: "cjs-transformed" };
                }
                return { action: "continue" };
            });
        };"#,
    )
    .unwrap();

    let paths = vec![
        js_path.to_string_lossy().into_owned(),
        mjs_path.to_string_lossy().into_owned(),
        cjs_path.to_string_lossy().into_owned(),
    ];
    let result = load_extensions(&paths, cwd, None, None).await;
    assert_eq!(result.errors, vec![], "load errors: {:?}", result.errors);
    assert_eq!(result.extensions.len(), 3);

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

    let mut ev = ToolCallEvent::Bash(BashToolCallEvent {
        tool_call_id: "b1".to_string(),
        parent_tool_call_id: None,
        input: BashToolInput {
            command: "block-js".to_string(),
            timeout: None,
        },
    });
    let res = runner.emit_tool_call(&mut ev).await.unwrap().expect("blocked");
    assert_eq!(res.block, Some(true));
    assert_eq!(res.reason.as_deref(), Some("blocked by js"));

    let tools = runner.get_all_registered_tools();
    assert_eq!(tools.len(), 1);
    let agent_tool = wrap_registered_tool(&tools[0], &runner);
    let out = (agent_tool.execute)("call-mjs".to_string(), serde_json::json!({ "msg": "ok" }), None, None)
        .await
        .expect("execute mjs_ping");
    let UserContentBlock::Text(t) = &out.content[0] else {
        panic!("expected text block");
    };
    assert_eq!(t.text, "mjs:ok");

    let transformed = runner.emit_input("cjs", None, InputSource::Interactive, None).await;
    assert_eq!(
        transformed,
        InputEventResult::Transform {
            text: "cjs-transformed".to_string(),
            images: None,
        }
    );
}

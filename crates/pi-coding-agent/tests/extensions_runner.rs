// PORT: QuickJS plugin runtime enabled per user requirement (overrides §10.6)
//! Port of packages/coding-agent/test/extensions-runner.test.ts

use std::fs;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent_core::types::AgentMessage;
use pi_ai::types::{Model, ProviderHeaders, TextContent};
use pi_coding_agent::core::extensions::mcp_wire::{
    BashResult, EventBus, ModelRegistry, ProjectedSessionEntry, ResourceDiagnosticType, ScopedModel, SessionEntry,
    SessionManager, build_system_prompt, create_event_bus,
};
use pi_coding_agent::core::extensions::{
    AgentActivityOutcome, AgentEndEvent, AutocompleteProviderFactory, BoundaryBaseEvent, BoundaryContextPreview,
    BoundaryResult, BuildSystemPromptOptions, CustomToolResultEvent, CustomUiFactory, EditorFactory,
    EmitProjectTrustOutput, ExtensionActions, ExtensionApi, ExtensionCommandContextActions, ExtensionContextActions,
    ExtensionCustomOptions, ExtensionMode, ExtensionRunner, ExtensionUIContext, ExtensionUIDialogOptions,
    ExtensionWidgetOptions, FooterFactory, ForkPosition, HeaderFactory, NotifyType, ProjectTrustContext,
    ProjectTrustEvent, ProjectTrustEventDecision, ProjectTrustEventResult, ProviderActions, ProviderConfig,
    SessionBoundaryDraft, SetThemeResult, TerminalInputHandler, Theme, ThemeInfo, ToolResultContentBlock,
    ToolResultEvent, UserBashEvent, UserBashEventResult, WidgetFactory, WorkingIndicatorOptions,
    create_extension_runtime, discover_and_load_extensions, emit_project_trust_event, load_extension_from_factory,
    load_extensions,
};
use pi_js::Unsubscribe;
use pi_js::abort::AbortController;
use pi_tui::keybindings::{KeyIdOrList, KeybindingsConfig};
use serde_json::Value;

fn default_actions() -> ExtensionActions {
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
        get_commands: Arc::new(Vec::new),
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

struct DummyUI;

#[async_trait]
impl ExtensionUIContext for DummyUI {
    async fn select(
        &self,
        _: &str,
        _: Vec<String>,
        _: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    async fn confirm(&self, _: &str, _: &str, _: Option<ExtensionUIDialogOptions>) -> pi_js::Result<bool> {
        Ok(false)
    }
    async fn input(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    fn notify(&self, _: &str, _: Option<NotifyType>) {}
    fn on_terminal_input(&self, _: TerminalInputHandler) -> Unsubscribe {
        Unsubscribe::noop()
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_working_message(&self, _: Option<&str>) {}
    fn set_working_visible(&self, _: bool) {}
    fn set_working_indicator(&self, _: Option<WorkingIndicatorOptions>) {}
    fn set_hidden_thinking_label(&self, _: Option<&str>) {}
    fn set_widget_lines(&self, _: &str, _: Option<Vec<String>>, _: Option<ExtensionWidgetOptions>) {}
    fn set_widget_factory(&self, _: &str, _: Option<WidgetFactory>, _: Option<ExtensionWidgetOptions>) {}
    fn set_footer(&self, _: Option<FooterFactory>) {}
    fn set_header(&self, _: Option<HeaderFactory>) {}
    fn set_title(&self, _: &str) {}
    async fn custom(&self, _: CustomUiFactory, _: Option<ExtensionCustomOptions>) -> pi_js::Result<Value> {
        Ok(Value::Null)
    }
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    async fn editor(&self, _: &str, _: Option<&str>) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    fn add_autocomplete_provider(&self, _: AutocompleteProviderFactory) {}
    fn set_editor_component(&self, _: Option<EditorFactory>) {}
    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }
    fn get_theme(&self, _: &str) -> Option<Theme> {
        None
    }
    fn set_theme_by_name(&self, _: &str) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn set_theme(&self, _: Theme) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn get_tools_expanded(&self) -> bool {
        false
    }
    fn set_tools_expanded(&self, _: bool) {}
}

fn setup_dirs() -> (tempfile::TempDir, String, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let ext_dir = dir.path().join("extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    (dir, root, ext_dir)
}

#[tokio::test]
async fn scoped_models_returns_live_get_scoped_models_action() {
    let (_d, root, _) = setup_dirs();
    let result = discover_and_load_extensions(&[], &root, Some(&root), None).await;
    let runner = ExtensionRunner::new(
        result.extensions,
        result.runtime,
        &root,
        SessionManager::default(),
        ModelRegistry::default(),
    );
    assert_eq!(runner.create_context().scoped_models().unwrap(), vec![]);

    let dummy_model: Model = serde_json::from_value(serde_json::json!({
        "id": "scoped-test",
        "name": "Scoped Test",
        "api": "openai-completions",
        "provider": "openai",
        "baseUrl": "https://example.invalid",
        "reasoning": false,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 8192,
        "maxTokens": 1024
    }))
    .unwrap();
    let scoped = vec![ScopedModel {
        model: dummy_model,
        thinking_level: Some(pi_agent_core::types::ThinkingLevel::High),
    }];
    let mut ctx_act = default_ctx_actions();
    let scoped_ret = scoped.clone();
    ctx_act.get_scoped_models = Arc::new(move || scoped_ret.clone());
    runner.bind_core(default_actions(), ctx_act, None);

    assert_eq!(runner.create_context().scoped_models().unwrap(), scoped);
}

#[tokio::test]
async fn project_trust_skips_undecided_and_returns_first_decision() {
    let (_d, root, ext_dir) = setup_dirs();
    let undecided = ext_dir.join("undecided.ts");
    let decided = ext_dir.join("decided.ts");
    fs::write(
        &undecided,
        r#"export default function(pi) { pi.on("project_trust", () => ({ trusted: "undecided", remember: true })); }"#,
    )
    .unwrap();
    fs::write(
        &decided,
        r#"export default function(pi) { pi.on("project_trust", () => ({ trusted: "no", remember: true })); }"#,
    )
    .unwrap();

    let ext_res = load_extensions(
        &[
            undecided.to_string_lossy().into_owned(),
            decided.to_string_lossy().into_owned(),
        ],
        &root,
        None,
        None,
    )
    .await;
    let out: EmitProjectTrustOutput = emit_project_trust_event(
        &ext_res,
        ProjectTrustEvent { cwd: root.clone() },
        ProjectTrustContext {
            cwd: root,
            mode: ExtensionMode::Tui,
            has_ui: false,
            ui: Arc::new(DummyUI),
        },
    )
    .await;

    assert_eq!(out.errors, vec![]);
    assert_eq!(
        out.result,
        Some(ProjectTrustEventResult {
            trusted: ProjectTrustEventDecision::No,
            remember: Some(true),
        })
    );
}

#[tokio::test]
async fn shortcut_conflicts_detection_and_reserved_keys() {
    let mut default_kb = KeybindingsConfig::new();
    default_kb.insert(
        "app.interrupt".to_string(),
        Some(KeyIdOrList::One("ctrl+c".to_string())),
    );
    default_kb.insert("app.clear".to_string(), Some(KeyIdOrList::One("ctrl+x".to_string())));
    default_kb.insert(
        "app.model.cycleForward".to_string(),
        Some(KeyIdOrList::One("ctrl+p".to_string())),
    );
    default_kb.insert(
        "app.clipboard.pasteImage".to_string(),
        Some(KeyIdOrList::One("ctrl+v".to_string())),
    );

    // 1. Conflicts with built-in reserved ctrl+c -> skipped
    {
        let (_d, root, ext_dir) = setup_dirs();
        fs::write(
            ext_dir.join("conflict.ts"),
            r#"export default function(pi) { pi.registerShortcut("ctrl+c", { handler: async () => {} }); }"#,
        )
        .unwrap();
        let res = discover_and_load_extensions(&[], &root, Some(&root), None).await;
        let runner = ExtensionRunner::new(
            res.extensions,
            res.runtime,
            &root,
            Default::default(),
            Default::default(),
        );
        let cap = pi_js::console::testing::capture();
        let shortcuts = runner.get_shortcuts(&default_kb);
        let stderr = cap.stderr();
        assert!(stderr.contains("conflicts with built-in"), "stderr={stderr}");
        assert!(!shortcuts.contains_key("ctrl+c"));
        assert!(!runner.get_shortcut_diagnostics().is_empty());
    }

    // 2. Allowed when reserved binding rebound away
    {
        let (_d, root, ext_dir) = setup_dirs();
        fs::write(
            ext_dir.join("freed.ts"),
            r#"export default function(pi) { pi.registerShortcut("ctrl+p", { handler: async () => {} }); }"#,
        )
        .unwrap();
        let res = discover_and_load_extensions(&[], &root, Some(&root), None).await;
        let runner = ExtensionRunner::new(
            res.extensions,
            res.runtime,
            &root,
            Default::default(),
            Default::default(),
        );
        let mut kb: KeybindingsConfig = default_kb.clone();
        kb.insert(
            "app.model.cycleForward".to_string(),
            Some(KeyIdOrList::One("ctrl+n".to_string())),
        );
        let shortcuts = runner.get_shortcuts(&kb);
        assert!(shortcuts.contains_key("ctrl+p"));
        assert!(runner.get_shortcut_diagnostics().is_empty());
    }

    // 3. Rebound reserved app.interrupt -> blocks ctrl+x even when non-reserved bound too
    {
        let (_d, root, ext_dir) = setup_dirs();
        fs::write(
            ext_dir.join("rebound.ts"),
            r#"export default function(pi) { pi.registerShortcut("ctrl+x", { handler: async () => {} }); }"#,
        )
        .unwrap();
        let res = discover_and_load_extensions(&[], &root, Some(&root), None).await;
        let runner = ExtensionRunner::new(
            res.extensions,
            res.runtime,
            &root,
            Default::default(),
            Default::default(),
        );
        let mut kb = default_kb.clone();
        kb.insert(
            "app.interrupt".to_string(),
            Some(KeyIdOrList::One("ctrl+x".to_string())),
        );
        let shortcuts = runner.get_shortcuts(&kb);
        assert!(!shortcuts.contains_key("ctrl+x"));
    }

    // 4. Multi-key reserved vs non-reserved & extension vs extension
    {
        let (_d, root, ext_dir) = setup_dirs();
        fs::write(
            ext_dir.join("ext1.ts"),
            r#"export default function(pi) { pi.registerShortcut("ctrl+shift+x", { description: "First", handler: async () => {} }); }"#,
        )
        .unwrap();
        fs::write(
            ext_dir.join("ext2.ts"),
            r#"export default function(pi) { pi.registerShortcut("ctrl+shift+x", { description: "Second", handler: async () => {} }); }"#,
        )
        .unwrap();
        let res = discover_and_load_extensions(&[], &root, Some(&root), None).await;
        let runner = ExtensionRunner::new(
            res.extensions,
            res.runtime,
            &root,
            Default::default(),
            Default::default(),
        );
        let shortcuts = runner.get_shortcuts(&default_kb);
        assert!(shortcuts.contains_key("ctrl+shift+x"));
        assert!(
            runner
                .get_shortcut_diagnostics()
                .iter()
                .any(|d| d.r#type == ResourceDiagnosticType::Warning && d.message.contains("shortcut conflict"))
        );
    }
}

#[tokio::test]
async fn tool_collection_and_missing_schema_rejection() {
    let (_d, root, ext_dir) = setup_dirs();
    let bad_path = ext_dir.join("missing-parameters.js");
    fs::write(
        &bad_path,
        r#"export default function(pi) { pi.registerTool({ name: "noop", label: "No-op", description: "Do nothing", execute: async () => ({ content: [] }) }); }"#,
    )
    .unwrap();
    let bad_str = bad_path.to_string_lossy().into_owned();
    let bad_res = load_extensions(&[bad_str.clone()], &root, None, None).await;
    assert_eq!(bad_res.extensions.len(), 0);
    assert_eq!(bad_res.errors.len(), 1);
    assert_eq!(
        bad_res.errors[0].error,
        format!(
            "Failed to load extension: Tool \"noop\" registered by extension \"{bad_str}\" must define an object parameter schema."
        )
    );

    let first = ext_dir.join("a-first.ts");
    let second = ext_dir.join("b-second.ts");
    let make = |name: &str, desc: &str| {
        format!(
            r#"
        import {{ Type }} from "typebox";
        export default function(pi) {{
            pi.registerTool({{ name: "{name}", label: "{name}", description: "{desc}", parameters: Type.Object({{}}), execute: async () => ({{ content: [] }}) }});
        }}
        "#
        )
    };
    fs::write(&first, make("shared", "first")).unwrap();
    fs::write(&second, make("shared", "second")).unwrap();
    let good_res = load_extensions(
        &[
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
        ],
        &root,
        None,
        None,
    )
    .await;
    let runner = ExtensionRunner::new(
        good_res.extensions,
        good_res.runtime,
        &root,
        Default::default(),
        Default::default(),
    );
    let tools = runner.get_all_registered_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].definition.description, "first");
}

#[tokio::test]
async fn command_collection_and_duplicate_suffixing() {
    let (_d, root, ext_dir) = setup_dirs();
    fs::write(
        ext_dir.join("cmd-a.ts"),
        r#"export default function(pi) { pi.registerCommand("shared-cmd", { description: "First command", handler: async () => {} }); }"#,
    )
    .unwrap();
    fs::write(
        ext_dir.join("cmd-b.ts"),
        r#"export default function(pi) { pi.registerCommand("shared-cmd", { description: "Second command", handler: async () => {} }); }"#,
    )
    .unwrap();

    let res = load_extensions(
        &[
            ext_dir.join("cmd-a.ts").to_string_lossy().into_owned(),
            ext_dir.join("cmd-b.ts").to_string_lossy().into_owned(),
        ],
        &root,
        None,
        None,
    )
    .await;
    let runner = ExtensionRunner::new(
        res.extensions,
        res.runtime,
        &root,
        Default::default(),
        Default::default(),
    );
    let commands = runner.get_registered_commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].invocation_name, "shared-cmd:1");
    assert_eq!(commands[1].invocation_name, "shared-cmd:2");
    assert_eq!(commands[0].command.description.as_deref(), Some("First command"));
    assert_eq!(commands[1].command.description.as_deref(), Some("Second command"));
    assert_eq!(runner.get_command_diagnostics(), vec![]);
    assert_eq!(
        runner
            .get_command("shared-cmd:1")
            .unwrap()
            .command
            .description
            .as_deref(),
        Some("First command")
    );
}

#[tokio::test]
async fn context_creation_signal_mode_and_invalidate() {
    let (_d, root, _) = setup_dirs();
    let res = discover_and_load_extensions(&[], &root, Some(&root), None).await;
    let runner = ExtensionRunner::new(
        res.extensions,
        res.runtime,
        &root,
        Default::default(),
        Default::default(),
    );
    let controller = AbortController::new();
    let sig = controller.signal();

    let mut ctx_actions = default_ctx_actions();
    ctx_actions.get_signal = Arc::new(move || Some(sig.clone()));
    ctx_actions.is_project_trusted = Arc::new(|| false);
    runner.bind_core(default_actions(), ctx_actions, None);

    let ctx = runner.create_context();
    assert_eq!(ctx.mode().unwrap(), ExtensionMode::Print);
    assert!(!ctx.has_ui().unwrap());
    assert!(!ctx.is_project_trusted().unwrap());
    assert!(!ctx.signal().unwrap().unwrap().aborted());
    controller.abort(None);
    assert!(ctx.signal().unwrap().unwrap().aborted());

    runner.set_ui_context(Some(Arc::new(DummyUI)), ExtensionMode::Rpc);
    assert_eq!(ctx.mode().unwrap(), ExtensionMode::Rpc);
    assert!(ctx.has_ui().unwrap());

    runner.set_ui_context(Some(Arc::new(DummyUI)), ExtensionMode::Tui);
    assert_eq!(ctx.mode().unwrap(), ExtensionMode::Tui);
    assert!(ctx.has_ui().unwrap());

    runner.invalidate(None);
    assert!(ctx.cwd().is_err());
}

#[tokio::test]
async fn user_bash_fails_closed_on_error_or_invalid_shapes_and_accepts_valid_overrides() {
    let invalid_cases = [
        "{}",
        "{ operations: null }",
        "{ operations: {} }",
        "{ result: null }",
        r#"{ result: { output: "handled" } }"#,
        r#"{ operations: { exec: async () => ({ exitCode: 0 }) }, result: { output: "handled", exitCode: 0, cancelled: false, truncated: false } }"#,
    ];

    for payload in invalid_cases {
        let (_d, root, ext_dir) = setup_dirs();
        fs::write(
            ext_dir.join("invalid-result.ts"),
            format!(r#"export default function(pi) {{ pi.on("user_bash", async () => ({payload})); }}"#),
        )
        .unwrap();
        let res = discover_and_load_extensions(&[], &root, Some(&root), None).await;
        let runner = ExtensionRunner::new(
            res.extensions,
            res.runtime,
            &root,
            Default::default(),
            Default::default(),
        );
        let errs = Arc::new(Mutex::new(Vec::<String>::new()));
        let errs_c = Arc::clone(&errs);
        let _u = runner.on_error_typed(Arc::new(move |e| errs_c.lock().unwrap().push(e.error)));

        let err = runner
            .emit_user_bash(UserBashEvent {
                command: "pwd".to_string(),
                exclude_from_context: false,
                cwd: root.clone(),
            })
            .await;
        assert!(err.is_err(), "payload `{payload}` should fail");
        let rec = errs.lock().unwrap();
        assert!(
            rec.iter().any(|m| m.contains("Invalid user_bash handler result")),
            "payload={payload}, errs={rec:?}"
        );
    }

    // Throwing user_bash handler fails closed
    {
        let (_d, root, ext_dir) = setup_dirs();
        fs::write(
            ext_dir.join("throws.ts"),
            r#"export default function(pi) { pi.on("user_bash", async () => { throw new Error("Routing failed"); }); }"#,
        )
        .unwrap();
        let res = discover_and_load_extensions(&[], &root, Some(&root), None).await;
        let runner = ExtensionRunner::new(
            res.extensions,
            res.runtime,
            &root,
            Default::default(),
            Default::default(),
        );
        let errs = Arc::new(Mutex::new(Vec::<String>::new()));
        let errs_c = Arc::clone(&errs);
        let _u = runner.on_error_typed(Arc::new(move |e| errs_c.lock().unwrap().push(e.error)));

        let out = runner
            .emit_user_bash(UserBashEvent {
                command: "pwd".to_string(),
                exclude_from_context: false,
                cwd: root.clone(),
            })
            .await;
        assert!(out.is_err());
        assert!(errs.lock().unwrap().iter().any(|e| e.contains("Routing failed")));
    }

    // Valid operations and result overrides
    {
        let (_d, root, ext_dir) = setup_dirs();
        fs::write(
            ext_dir.join("valid.ts"),
            r#"
            export default function(pi) {
                pi.on("user_bash", async (event) => {
                    if (event.command === "operations") {
                        return { operations: { exec: async () => ({ exitCode: 7 }) } };
                    }
                    return { result: { output: "handled", exitCode: 0, cancelled: false, truncated: false } };
                });
            }
            "#,
        )
        .unwrap();
        let res = discover_and_load_extensions(&[], &root, Some(&root), None).await;
        let runner = ExtensionRunner::new(
            res.extensions,
            res.runtime,
            &root,
            Default::default(),
            Default::default(),
        );

        let ops_res = runner
            .emit_user_bash(UserBashEvent {
                command: "operations".to_string(),
                exclude_from_context: false,
                cwd: root.clone(),
            })
            .await
            .unwrap()
            .expect("operations override");
        let UserBashEventResult::Operations { operations } = ops_res else {
            panic!("expected Operations");
        };
        let exec_res = operations.exec("operations", &root, Default::default()).await.unwrap();
        assert_eq!(exec_res.exit_code, Some(7));

        let res_override = runner
            .emit_user_bash(UserBashEvent {
                command: "result".to_string(),
                exclude_from_context: false,
                cwd: root.clone(),
            })
            .await
            .unwrap()
            .expect("result override");
        let UserBashEventResult::Result { result } = res_override else {
            panic!("expected Result");
        };
        assert_eq!(
            result,
            BashResult {
                output: "handled".to_string(),
                exit_code: Some(0),
                cancelled: false,
                truncated: false,
                full_output_path: None,
            }
        );
    }
}

#[tokio::test]
async fn message_and_entry_renderers_and_markdown_transformers() {
    let (_d, root, ext_dir) = setup_dirs();
    fs::write(
        ext_dir.join("renderers.ts"),
        r#"
        export default function(pi) {
            pi.registerMarkdownTransformer((markdown) => markdown + "!");
            pi.registerMessageRenderer("my-type", () => null);
            pi.registerEntryRenderer("my-entry", () => null);
        }
        "#,
    )
    .unwrap();
    let res = discover_and_load_extensions(&[], &root, Some(&root), None).await;
    let runner = ExtensionRunner::new(
        res.extensions,
        res.runtime,
        &root,
        Default::default(),
        Default::default(),
    );
    assert_eq!(runner.get_markdown_transformers().len(), 1);
    assert!(runner.get_message_renderer("my-type").is_some());
    assert!(runner.get_message_renderer("missing").is_none());
    assert!(runner.get_entry_renderer("my-entry").is_some());
    assert!(runner.get_entry_renderer("missing").is_none());
}

#[tokio::test]
async fn flags_collection_dedupe_validation_and_set_value() {
    let (_d, root, ext_dir) = setup_dirs();
    fs::write(
        ext_dir.join("bad-flag.ts"),
        r#"export default function(pi) { pi.registerFlag("safe-mode", { type: "boolean", default: "false" }); }"#,
    )
    .unwrap();
    let bad = load_extensions(
        &[ext_dir.join("bad-flag.ts").to_string_lossy().into_owned()],
        &root,
        None,
        None,
    )
    .await;
    assert_eq!(bad.extensions.len(), 0);
    assert!(
        bad.errors[0]
            .error
            .contains("Invalid default for flag \"safe-mode\": expected boolean, got string")
    );

    let f1 = ext_dir.join("a.ts");
    let f2 = ext_dir.join("b.ts");
    fs::write(
        &f1,
        r#"export default function(pi) { pi.registerFlag("shared-flag", { description: "first", type: "boolean", default: true }); }"#,
    )
    .unwrap();
    fs::write(
        &f2,
        r#"export default function(pi) { pi.registerFlag("shared-flag", { description: "second", type: "boolean", default: false }); }"#,
    )
    .unwrap();
    let good = load_extensions(
        &[f1.to_string_lossy().into_owned(), f2.to_string_lossy().into_owned()],
        &root,
        None,
        None,
    )
    .await;
    let runner = ExtensionRunner::new(
        good.extensions,
        good.runtime,
        &root,
        Default::default(),
        Default::default(),
    );
    let flags = runner.get_flags();
    assert_eq!(flags.get("shared-flag").unwrap().description.as_deref(), Some("first"));
    runner.set_flag_value("--test-flag", pi_coding_agent::core::extensions::FlagValue::Bool(true));
    assert_eq!(
        runner.get_flag_values().get("--test-flag"),
        Some(&pi_coding_agent::core::extensions::FlagValue::Bool(true))
    );
}

#[tokio::test]
async fn before_agent_start_keeps_get_system_prompt_in_sync() {
    let (_d, root, ext_dir) = setup_dirs();
    let p1 = ext_dir.join("1.ts");
    let p2 = ext_dir.join("2.ts");
    fs::write(
        &p1,
        r#"export default function(pi) { pi.on("before_agent_start", async (_ev, ctx) => ({ systemPrompt: ctx.getSystemPrompt() + "\nfirst" })); }"#,
    )
    .unwrap();
    fs::write(
        &p2,
        r#"export default function(pi) { pi.on("before_agent_start", async (_ev, ctx) => ({ systemPrompt: ctx.getSystemPrompt() + "\nsecond" })); }"#,
    )
    .unwrap();
    let res = load_extensions(
        &[p1.to_string_lossy().into_owned(), p2.to_string_lossy().into_owned()],
        &root,
        None,
        None,
    )
    .await;
    assert_eq!(res.errors, vec![]);
    let runner = ExtensionRunner::new(
        res.extensions,
        res.runtime,
        &root,
        Default::default(),
        Default::default(),
    );
    runner.bind_core(default_actions(), default_ctx_actions(), None);

    let combined = runner
        .emit_before_agent_start(
            "hello",
            None,
            BuildSystemPromptOptions {
                cwd: root.clone(),
                custom_prompt: Some("base".to_string()),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(combined.messages.len(), 0);
    let rendered = build_system_prompt(&combined.system_prompt_options);
    assert_eq!(rendered, "base\nfirst\nsecond");
}

#[tokio::test]
async fn boundary_chaining_repair_and_throw_preservation() {
    let (_d, root, _) = setup_dirs();
    let rt = create_extension_runtime();
    let bus = create_event_bus() as Arc<dyn EventBus>;

    let first = load_extension_from_factory(
        Arc::new(|pi: ExtensionApi| {
            Box::pin(async move {
                pi.on::<pi_coding_agent::core::extensions::AgentBeforeSettleEvent>(move |ev, _| {
                    Box::pin(async move {
                        let mut next = ev.entries.clone();
                        next.push(SessionBoundaryDraft::Custom {
                            custom_type: "first".to_string(),
                            data: Some(serde_json::json!(1)),
                        });
                        Ok(Some(BoundaryResult {
                            entries: Some(next),
                            r#continue: Some(true),
                        }))
                    })
                });
                Ok(())
            })
        }),
        &root,
        Arc::clone(&bus),
        rt.clone(),
        Some("<inline:first>"),
    )
    .await
    .unwrap();

    let second = load_extension_from_factory(
        Arc::new(|pi: ExtensionApi| {
            Box::pin(async move {
                pi.on::<pi_coding_agent::core::extensions::AgentBeforeSettleEvent>(move |ev, _| {
                    Box::pin(async move {
                        assert_eq!(ev.entries.len(), 1);
                        assert!(ev.r#continue);
                        assert_eq!(ev.context.context_entries.len(), 1);
                        Ok(Some(BoundaryResult {
                            entries: Some(Vec::new()),
                            r#continue: None,
                        }))
                    })
                });
                Ok(())
            })
        }),
        &root,
        bus,
        rt.clone(),
        Some("<inline:second>"),
    )
    .await
    .unwrap();

    let runner = ExtensionRunner::new(vec![first, second], rt, &root, Default::default(), Default::default());
    let dispatched = runner
        .emit_boundary(
            BoundaryBaseEvent::AgentBeforeSettle {
                outcome: AgentActivityOutcome::Completed,
            },
            |entries| {
                let context_entries = entries
                    .iter()
                    .enumerate()
                    .map(|(i, _)| ProjectedSessionEntry {
                        source_entry: SessionEntry {
                            r#type: "custom".to_string(),
                            id: format!("draft-{i}"),
                            parent_id: None,
                            timestamp: String::new(),
                            fields: Default::default(),
                        },
                        messages: Vec::new(),
                    })
                    .collect();
                async move {
                    Ok(BoundaryContextPreview {
                        context_entries,
                        ..Default::default()
                    })
                }
            },
        )
        .await;
    assert!(dispatched.valid);
    assert!(dispatched.r#continue);
    assert_eq!(dispatched.entries, vec![]);

    // Also verify QuickJS mutation before throw keeps mutated entries
    let rt2 = create_extension_runtime();
    let (_d2, root2, ext2) = setup_dirs();
    fs::write(
        ext2.join("mut-throw.ts"),
        r#"
        export default function(pi) {
            pi.on("agent_before_settle", (event) => {
                event.entries.push({ type: "custom", customType: "kept" });
                throw new Error("boundary failed");
            });
        }
        "#,
    )
    .unwrap();
    let res2 = discover_and_load_extensions(&[], &root2, Some(&root2), None).await;
    let runner2 = ExtensionRunner::new(res2.extensions, rt2, &root2, Default::default(), Default::default());
    let errs = Arc::new(Mutex::new(Vec::<String>::new()));
    let errs_c = Arc::clone(&errs);
    let _u = runner2.on_error_typed(Arc::new(move |e| errs_c.lock().unwrap().push(e.error)));
    let b2 = runner2
        .emit_boundary(
            BoundaryBaseEvent::AgentBeforeSettle {
                outcome: AgentActivityOutcome::Completed,
            },
            |_| async { Ok(BoundaryContextPreview::default()) },
        )
        .await;
    assert_eq!(b2.entries.len(), 1);
    assert_eq!(errs.lock().unwrap().as_slice(), &["boundary failed"]);
}

#[tokio::test]
async fn tool_result_chaining_and_partial_patches() {
    let (_d, root, ext_dir) = setup_dirs();
    let p1 = ext_dir.join("1.ts");
    let p2 = ext_dir.join("2.ts");
    fs::write(
        &p1,
        r#"export default function(pi) { pi.on("tool_result", async (ev) => ({ content: [...ev.content, { type: "text", text: "first" }], details: { source: "ext1" } })); }"#,
    )
    .unwrap();
    fs::write(
        &p2,
        r#"export default function(pi) { pi.on("tool_result", async () => ({ isError: true })); }"#,
    )
    .unwrap();
    let res = load_extensions(
        &[p1.to_string_lossy().into_owned(), p2.to_string_lossy().into_owned()],
        &root,
        None,
        None,
    )
    .await;
    let runner = ExtensionRunner::new(
        res.extensions,
        res.runtime,
        &root,
        Default::default(),
        Default::default(),
    );
    let chained = runner
        .emit_tool_result(ToolResultEvent::Custom(CustomToolResultEvent {
            tool_call_id: "call-1".to_string(),
            parent_tool_call_id: None,
            tool_name: "my_tool".to_string(),
            input: Default::default(),
            content: vec![ToolResultContentBlock::Text(TextContent {
                text: "base".to_string(),
                text_signature: None,
            })],
            structured_content: None,
            is_error: false,
            usage: None,
            details: Some(serde_json::json!({ "initial": true })),
        }))
        .await
        .expect("modified tool result");

    assert_eq!(chained.is_error, Some(true));
    assert_eq!(chained.details, Some(serde_json::json!({ "source": "ext1" })));
    let blocks = chained.content.unwrap();
    assert_eq!(blocks.len(), 2);
}

#[tokio::test]
async fn event_subscriptions_8967_and_before_provider_headers() {
    let (_d, root, _) = setup_dirs();
    let rt = create_extension_runtime();
    let bus = create_event_bus() as Arc<dyn EventBus>;
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));

    // Self-removal without skipping neighbors
    let calls_c = Arc::clone(&calls);
    let unsub_slot: Arc<Mutex<Option<Unsubscribe>>> = Arc::new(Mutex::new(None));
    let unsub_slot_c = Arc::clone(&unsub_slot);
    let ext = load_extension_from_factory(
        Arc::new(move |pi: ExtensionApi| {
            let calls_c = Arc::clone(&calls_c);
            let unsub_slot_c = Arc::clone(&unsub_slot_c);
            Box::pin(async move {
                let calls_a = Arc::clone(&calls_c);
                let slot_a = Arc::clone(&unsub_slot_c);
                let unsub = pi.on::<AgentEndEvent>(move |_, _| {
                    let calls_a = Arc::clone(&calls_a);
                    let slot_a = Arc::clone(&slot_a);
                    Box::pin(async move {
                        calls_a.lock().unwrap().push("A".to_string());
                        if let Some(u) = slot_a.lock().unwrap().take() {
                            u.call();
                        }
                        Ok(None)
                    })
                });
                *unsub_slot_c.lock().unwrap() = Some(unsub);
                let calls_b = Arc::clone(&calls_c);
                pi.on::<AgentEndEvent>(move |_, _| {
                    let calls_b = Arc::clone(&calls_b);
                    Box::pin(async move {
                        calls_b.lock().unwrap().push("B".to_string());
                        Ok(None)
                    })
                });
                Ok(())
            })
        }),
        &root,
        bus,
        rt.clone(),
        None,
    )
    .await
    .unwrap();
    let runner = ExtensionRunner::new(vec![ext], rt, &root, Default::default(), Default::default());
    runner.emit(AgentEndEvent { messages: vec![] }).await;
    assert_eq!(calls.lock().unwrap().as_slice(), &["A", "B"]);
    runner.emit(AgentEndEvent { messages: vec![] }).await;
    assert_eq!(calls.lock().unwrap().as_slice(), &["A", "B", "B"]);

    // before_provider_headers QuickJS in-place mutation + throwing isolation
    let (_dh, rooth, ext_dirh) = setup_dirs();
    let h1 = ext_dirh.join("a-throws.ts");
    let h2 = ext_dirh.join("b-good.ts");
    fs::write(
        &h1,
        r#"export default function(pi) { pi.on("before_provider_headers", () => { throw new Error("header handler boom"); }); }"#,
    )
    .unwrap();
    fs::write(
        &h2,
        r#"export default function(pi) { pi.on("before_provider_headers", (event) => { event.headers["X-Good"] = "yes"; }); }"#,
    )
    .unwrap();
    let rh = load_extensions(
        &[h1.to_string_lossy().into_owned(), h2.to_string_lossy().into_owned()],
        &rooth,
        None,
        None,
    )
    .await;
    let r_hdr = ExtensionRunner::new(
        rh.extensions,
        rh.runtime,
        &rooth,
        Default::default(),
        Default::default(),
    );
    let errs = Arc::new(Mutex::new(Vec::<String>::new()));
    let errs_c = Arc::clone(&errs);
    let _u = r_hdr.on_error_typed(Arc::new(move |e| errs_c.lock().unwrap().push(e.error)));

    let mut init: ProviderHeaders = Default::default();
    init.insert("User-Agent".to_string(), Some("kimchi/1.0".to_string()));
    let updated = r_hdr.emit_before_provider_headers(init).await;
    assert_eq!(updated.get("X-Good").and_then(|o| o.as_deref()), Some("yes"));
    assert_eq!(updated.get("User-Agent").and_then(|o| o.as_deref()), Some("kimchi/1.0"));
    assert!(errs.lock().unwrap().iter().any(|m| m.contains("header handler boom")));

    // command context fork forwarding
    let seen_fork = Arc::new(Mutex::new(Vec::<(String, Option<ForkPosition>)>::new()));
    let seen_fork_c = Arc::clone(&seen_fork);
    r_hdr.bind_command_context(Some(ExtensionCommandContextActions {
        wait_for_idle: Arc::new(|| Box::pin(async { Ok(()) })),
        new_session: Arc::new(|_| {
            Box::pin(async { Ok(pi_coding_agent::core::extensions::CancelledResult { cancelled: false }) })
        }),
        fork: Arc::new(move |id, opts| {
            let id = id.to_string();
            let pos = opts.and_then(|o| o.position);
            seen_fork_c.lock().unwrap().push((id, pos));
            Box::pin(async { Ok(pi_coding_agent::core::extensions::CancelledResult { cancelled: false }) })
        }),
        navigate_tree: Arc::new(|_, _| {
            Box::pin(async { Ok(pi_coding_agent::core::extensions::CancelledResult { cancelled: false }) })
        }),
        switch_session: Arc::new(|_, _| {
            Box::pin(async { Ok(pi_coding_agent::core::extensions::CancelledResult { cancelled: false }) })
        }),
        reload: Arc::new(|| Box::pin(async { Ok(()) })),
    }));
    let cmd_ctx = r_hdr.create_command_context();
    cmd_ctx.fork("entry-1", None).await.unwrap();
    cmd_ctx
        .fork(
            "entry-2",
            Some(pi_coding_agent::core::extensions::ForkOptions {
                position: Some(ForkPosition::At),
                with_session: None,
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        seen_fork.lock().unwrap().as_slice(),
        &[
            ("entry-1".to_string(), None),
            ("entry-2".to_string(), Some(ForkPosition::At))
        ]
    );
}

#[tokio::test]
async fn provider_actions_flush_and_context_roundtrip() {
    let (_d, root, ext_dir) = setup_dirs();
    let rt = create_extension_runtime();
    let flushed = Arc::new(Mutex::new(Vec::<String>::new()));
    let flushed_c = Arc::clone(&flushed);
    (rt.inner.register_provider_fn.lock().unwrap().clone())(
        "queued-provider",
        ProviderConfig::default(),
        "/tmp/queued.ts",
    );
    let runner = ExtensionRunner::new(Vec::new(), rt, &root, Default::default(), Default::default());
    runner.bind_core(
        default_actions(),
        default_ctx_actions(),
        Some(ProviderActions {
            register_provider: Some(Arc::new(move |name, _| {
                flushed_c.lock().unwrap().push(name.to_string());
            })),
            ..Default::default()
        }),
    );
    assert_eq!(flushed.lock().unwrap().as_slice(), &["queued-provider"]);

    // emit_context strips system from visible messages and restores head
    fs::write(
        ext_dir.join("ctx.ts"),
        r#"
        export default function(pi) {
            pi.on("context", async (ev) => ({
                messages: ev.messages.slice(1),
            }));
        }
        "#,
    )
    .unwrap();
    let res = discover_and_load_extensions(&[], &root, Some(&root), None).await;
    let r = ExtensionRunner::new(
        res.extensions,
        res.runtime,
        &root,
        Default::default(),
        Default::default(),
    );
    let sys_msg = AgentMessage::Message(pi_ai::types::Message::System(pi_ai::types::SystemMessage {
        content: pi_ai::types::SystemMessageContent::Text("sys prompt".to_string()),
        sections: None,
        tools_added: None,
        tools_removed: None,
        timestamp: 0,
    }));
    let u1 = AgentMessage::custom(serde_json::json!({ "role": "user", "content": "one" }));
    let u2 = AgentMessage::custom(serde_json::json!({ "role": "user", "content": "two" }));
    let out = r.emit_context(vec![sys_msg, u1, u2]).await;
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].role(), Some("system"));
    assert_eq!(out[1].role(), Some("user"));
}
// WIRE(@mcp-w3): add queued invalid-provider error reporting and ModelRegistry::find checks once real ModelRegistry lands from @mcp.

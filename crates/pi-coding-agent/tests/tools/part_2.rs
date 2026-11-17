//! Port of packages/coding-agent/test/tools.test.ts#L485-975, #L994-1099 + HEAD bugfix specs

use std::sync::Arc;

use pi_agent_core::types::AgentToolResult;
use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::bash_executor::{BashExecutorOptions, execute_bash_with_operations};
use pi_coding_agent::core::extensions::types::{
    CustomMessageContent, CustomMessageDraft, ExtensionContext, ExtensionContextBackend, ExtensionMode,
    ExtensionToolContext, ExtensionUIContext, ModelRegistry, ReadonlySessionManager, ScopedModel,
};
use pi_coding_agent::core::tools::bash::testing::override_get_shell_config;
use pi_coding_agent::core::tools::bash::{
    BashExecOptions, BashExecResult, BashOperations, BashToolDetails, BashToolOptions, create_bash_tool,
    create_bash_tool_definition, create_bash_tool_definition_with_shell, create_local_bash_operations,
    create_local_bash_operations_with_shell,
};
use pi_coding_agent::core::tools::edit::create_edit_tool_definition;
use pi_coding_agent::core::tools::find::{
    FindGlobOptions, FindOperations, FindToolDetails, FindToolOptions, create_find_tool, create_find_tool_definition,
};
use pi_coding_agent::core::tools::grep::{GrepToolDetails, create_grep_tool, create_grep_tool_definition};
use pi_coding_agent::core::tools::ls::create_ls_tool_definition;
use pi_coding_agent::core::tools::read::create_read_tool_definition;
use pi_coding_agent::core::tools::truncate::{TruncationOptions, truncate_head};
use pi_coding_agent::core::tools::wrap_tool_definition;
use pi_coding_agent::core::tools::write::create_write_tool_definition;
use pi_coding_agent::utils::shell::{CommandTransport, ShellConfig, get_shell_config};
use pi_js::BoxFuture;
use pi_js::env::testing::EnvGuard;
use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;

fn get_text_output(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Minimal fake ExtensionToolContext returning a configurable `cwd`
struct CwdOnlyBackend {
    cwd: String,
}
struct NoopUi;
#[async_trait::async_trait]
impl ExtensionUIContext for NoopUi {
    async fn select(
        &self,
        _: &str,
        _: Vec<String>,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    async fn confirm(
        &self,
        _: &str,
        _: &str,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<bool> {
        Ok(false)
    }
    async fn input(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    fn notify(&self, _: &str, _: Option<pi_coding_agent::core::extensions::types::NotifyType>) {}
    fn on_terminal_input(
        &self,
        _: pi_coding_agent::core::extensions::types::TerminalInputHandler,
    ) -> pi_js::Unsubscribe {
        pi_js::Unsubscribe::noop()
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_working_message(&self, _: Option<&str>) {}
    fn set_working_visible(&self, _: bool) {}
    fn set_working_indicator(&self, _: Option<pi_coding_agent::core::extensions::types::WorkingIndicatorOptions>) {}
    fn set_hidden_thinking_label(&self, _: Option<&str>) {}
    fn set_widget_lines(
        &self,
        _: &str,
        _: Option<Vec<String>>,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }
    fn set_widget_factory(
        &self,
        _: &str,
        _: Option<pi_coding_agent::core::extensions::types::WidgetFactory>,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }
    fn set_footer(&self, _: Option<pi_coding_agent::core::extensions::types::FooterFactory>) {}
    fn set_header(&self, _: Option<pi_coding_agent::core::extensions::types::HeaderFactory>) {}
    fn set_title(&self, _: &str) {}
    async fn custom(
        &self,
        _: pi_coding_agent::core::extensions::types::CustomUiFactory,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionCustomOptions>,
    ) -> pi_js::Result<Value> {
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
    fn add_autocomplete_provider(&self, _: pi_coding_agent::core::extensions::types::AutocompleteProviderFactory) {}
    fn set_editor_component(&self, _: Option<pi_coding_agent::core::extensions::types::EditorFactory>) {}
    fn get_editor_component(&self) -> Option<pi_coding_agent::core::extensions::types::EditorFactory> {
        None
    }
    fn theme(&self) -> pi_coding_agent::core::extensions::types::Theme {
        Default::default()
    }
    fn get_all_themes(&self) -> Vec<pi_coding_agent::core::extensions::types::ThemeInfo> {
        Vec::new()
    }
    fn get_theme(&self, _: &str) -> Option<pi_coding_agent::core::extensions::types::Theme> {
        None
    }
    fn set_theme_by_name(&self, _: &str) -> pi_coding_agent::core::extensions::types::SetThemeResult {
        pi_coding_agent::core::extensions::types::SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn set_theme(
        &self,
        _: pi_coding_agent::core::extensions::types::Theme,
    ) -> pi_coding_agent::core::extensions::types::SetThemeResult {
        pi_coding_agent::core::extensions::types::SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn get_tools_expanded(&self) -> bool {
        false
    }
    fn set_tools_expanded(&self, _: bool) {}
}
impl ExtensionContextBackend for CwdOnlyBackend {
    fn assert_active(&self) -> pi_js::Result<()> {
        Ok(())
    }
    fn ui(&self) -> Arc<dyn ExtensionUIContext> {
        Arc::new(NoopUi)
    }
    fn mode(&self) -> ExtensionMode {
        ExtensionMode::Print
    }
    fn has_ui(&self) -> bool {
        false
    }
    fn cwd(&self) -> String {
        self.cwd.clone()
    }
    fn session_manager(&self) -> ReadonlySessionManager {
        Default::default()
    }
    fn model_registry(&self) -> ModelRegistry {
        Default::default()
    }
    fn model(&self) -> Option<pi_ai::types::Model> {
        None
    }
    fn scoped_models(&self) -> Vec<ScopedModel> {
        Vec::new()
    }
    fn thinking_level(&self) -> Option<pi_agent_core::types::ThinkingLevel> {
        None
    }
    fn is_idle(&self) -> bool {
        true
    }
    fn is_project_trusted(&self) -> bool {
        false
    }
    fn signal(&self) -> Option<pi_js::abort::AbortSignal> {
        None
    }
    fn abort(&self) {}
    fn has_pending_messages(&self) -> bool {
        false
    }
    fn shutdown(&self) {}
    fn get_context_usage(&self) -> Option<pi_coding_agent::core::extensions::types::ContextUsage> {
        None
    }
    fn compact(&self, _: Option<pi_coding_agent::core::extensions::types::CompactOptions>) {}
    fn get_system_prompt(&self) -> String {
        String::new()
    }
    fn get_system_prompt_options(&self) -> pi_coding_agent::core::extensions::types::BuildSystemPromptOptions {
        Default::default()
    }
    fn wait_for_idle(&self) -> BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn new_session(
        &self,
        _: Option<pi_coding_agent::core::extensions::types::NewSessionOptions>,
    ) -> BoxFuture<pi_js::Result<pi_coding_agent::core::extensions::types::CancelledResult>> {
        Box::pin(async { Ok(pi_coding_agent::core::extensions::types::CancelledResult { cancelled: false }) })
    }
    fn fork(
        &self,
        _: &str,
        _: Option<pi_coding_agent::core::extensions::types::ForkOptions>,
    ) -> BoxFuture<pi_js::Result<pi_coding_agent::core::extensions::types::CancelledResult>> {
        Box::pin(async { Ok(pi_coding_agent::core::extensions::types::CancelledResult { cancelled: false }) })
    }
    fn navigate_tree(
        &self,
        _: &str,
        _: Option<pi_coding_agent::core::extensions::types::NavigateTreeOptions>,
    ) -> BoxFuture<pi_js::Result<pi_coding_agent::core::extensions::types::CancelledResult>> {
        Box::pin(async { Ok(pi_coding_agent::core::extensions::types::CancelledResult { cancelled: false }) })
    }
    fn switch_session(
        &self,
        _: &str,
        _: Option<pi_coding_agent::core::extensions::types::SwitchSessionOptions>,
    ) -> BoxFuture<pi_js::Result<pi_coding_agent::core::extensions::types::CancelledResult>> {
        Box::pin(async { Ok(pi_coding_agent::core::extensions::types::CancelledResult { cancelled: false }) })
    }
    fn reload(&self) -> BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn callable_tools(&self) -> Vec<pi_agent_core::types::AgentTool> {
        Vec::new()
    }
    fn execute_tool(
        &self,
        _: &str,
        _: &str,
        _: Value,
        _: pi_coding_agent::core::extensions::types::ExecuteToolOptions,
    ) -> BoxFuture<pi_agent_core::types::AgentToolCallOutcome> {
        Box::pin(async {
            pi_agent_core::types::AgentToolCallOutcome {
                tool_call: pi_agent_core::types::AgentToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: Default::default(),
                },
                result: Default::default(),
                is_error: true,
            }
        })
    }
    fn send_message(
        &self,
        _: CustomMessageDraft,
        _: Option<pi_coding_agent::core::extensions::types::SendMessageOptions>,
    ) {
    }
    fn send_user_message(
        &self,
        _: CustomMessageContent,
        _: Option<pi_coding_agent::core::extensions::types::SendUserMessageOptions>,
    ) {
    }
}

fn fake_ctx(cwd: &str) -> ExtensionToolContext {
    ExtensionToolContext {
        base: ExtensionContext::from_backend(Arc::new(CwdOnlyBackend { cwd: cwd.to_string() })),
        tool_call_id: "fake-ctx-call".to_string(),
        default_signal: None,
    }
}

// =========================================================================
// Bash tool
// =========================================================================

#[tokio::test]
#[serial]
async fn bash_tool_simple_nonzero_empty_and_structured_content() {
    let dir = TempDir::new().unwrap();
    let bash = create_bash_tool(&dir.path().to_string_lossy(), None);

    let simple = (bash.execute)("b1".into(), json!({ "command": "echo 'test output'" }), None, None)
        .await
        .unwrap();
    assert!(get_text_output(&simple).contains("test output"));
    assert!(simple.details.is_none());

    let nonzero = (bash.execute)("b2".into(), json!({ "command": "echo out; exit 3" }), None, None)
        .await
        .unwrap();
    assert_eq!(nonzero.is_error, Some(true));
    assert_eq!(get_text_output(&nonzero), "out\n\n\nCommand exited with code 3");
    let sc = nonzero.structured_content.unwrap();
    assert_eq!(sc["output"], "out\n");
    assert_eq!(sc["truncated"], false);
    assert_eq!(sc["exit_code"], 3);
    assert!(sc["wall_time_seconds"].as_f64().is_some());

    let ok = (bash.execute)("b3".into(), json!({ "command": "echo fine" }), None, None)
        .await
        .unwrap();
    assert!(ok.is_error.is_none());
    let ok_sc = ok.structured_content.unwrap();
    assert_eq!(ok_sc["output"], "fine\n");
    assert_eq!(ok_sc["exit_code"], 0);

    let empty = (bash.execute)("b4".into(), json!({ "command": "true" }), None, None)
        .await
        .unwrap();
    assert_eq!(get_text_output(&empty), "(no output)");
    assert_eq!(empty.structured_content.unwrap()["output"], "");

    // Medium (3000 lines): truncated in model text, full in structuredContent
    let med = (bash.execute)("b5".into(), json!({ "command": "seq 1 3000" }), None, None)
        .await
        .unwrap();
    let med_txt = get_text_output(&med);
    assert!(!med_txt.contains("\n1\n2\n"));
    let med_details: BashToolDetails = serde_json::from_value(med.details.unwrap()).unwrap();
    assert!(med_details.truncation.unwrap().truncated);
    let med_sc = med.structured_content.unwrap();
    assert_eq!(med_sc["truncated"], false);
    let expected_med: String = (1..=3000).map(|i| format!("{i}\n")).collect();
    assert_eq!(med_sc["output"], expected_med);

    // Large (300000 lines ~ 2MB): 1 MiB middle truncation + fullOutputPath
    let large = (bash.execute)("b6".into(), json!({ "command": "seq 1 300000" }), None, None)
        .await
        .unwrap();
    let large_details: BashToolDetails = serde_json::from_value(large.details.unwrap()).unwrap();
    let large_sc = large.structured_content.unwrap();
    assert_eq!(large_sc["truncated"], true);
    let out_str = large_sc["output"].as_str().unwrap();
    assert!(out_str.starts_with("1\n2\n3\n"));
    assert!(out_str.ends_with("299999\n300000\n"));
    assert!(out_str.contains(" bytes omitted ..."));
    assert!(out_str.len() < 1024 * 1024 + 100);
    assert_eq!(
        large_sc["full_output_path"].as_str(),
        large_details.full_output_path.as_deref()
    );
    let full_path = large_details.full_output_path.unwrap();
    assert!(std::fs::read_to_string(&full_path).unwrap().ends_with("300000\n"));
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn bash_tool_signal_exit_codes_timeout_and_abort() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let bash = create_bash_tool(&cwd, None);
    let ops = create_local_bash_operations();

    for (sig, code) in [("KILL", 137), ("TERM", 143)] {
        let r = ops
            .exec(&format!("kill -{sig} $$"), &cwd, BashExecOptions::default())
            .await
            .unwrap();
        assert_eq!(r.exit_code, Some(code));

        let tool_res = (bash.execute)(
            format!("sig-{sig}"),
            json!({ "command": format!("printf 'before-kill\\n'; kill -{sig} $$") }),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(tool_res.is_error, Some(true));
        let txt = get_text_output(&tool_res);
        assert!(txt.contains("before-kill"));
        assert!(txt.ends_with(&format!("Command exited with code {code}")), "{txt}");
    }

    // Null exit code -> Err
    struct NullExitOps;
    impl BashOperations for NullExitOps {
        fn exec<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            options: BashExecOptions,
        ) -> BoxFuture<pi_js::Result<BashExecResult>> {
            Box::pin(async move {
                if let Some(cb) = &options.on_data {
                    cb(b"partial\n");
                }
                Ok(BashExecResult { exit_code: None })
            })
        }
    }
    let null_bash = create_bash_tool(
        &cwd,
        Some(BashToolOptions {
            operations: Some(Arc::new(NullExitOps)),
            ..Default::default()
        }),
    );
    let err_null = (null_bash.execute)("null-exit".into(), json!({ "command": "remote" }), None, None)
        .await
        .unwrap_err();
    assert!(err_null.to_string().contains("Command terminated without an exit code"));
    assert!(err_null.to_string().contains("partial"));

    // Timeout enforcement
    let err_to = (bash.execute)(
        "timeout-test".into(),
        json!({ "command": "sleep 5", "timeout": 0.05 }),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(err_to.to_string().to_lowercase().contains("timed out"), "{err_to}");

    // Full output path preserved on timeout and abort errors
    for (err_kind, expected_msg) in [
        ("timeout:5", "Command timed out after 5 seconds"),
        ("aborted", "Command aborted"),
    ] {
        struct ChattyFailOps(&'static str);
        impl BashOperations for ChattyFailOps {
            fn exec<'a>(
                &'a self,
                _: &'a str,
                _: &'a str,
                options: BashExecOptions,
            ) -> BoxFuture<pi_js::Result<BashExecResult>> {
                let kind = self.0;
                Box::pin(async move {
                    if let Some(cb) = &options.on_data {
                        for i in 1..=3000 {
                            cb(format!("{i}\n").as_bytes());
                        }
                    }
                    Err(pi_js::Error::msg(kind))
                })
            }
        }
        let tool = create_bash_tool(
            &cwd,
            Some(BashToolOptions {
                operations: Some(Arc::new(ChattyFailOps(err_kind))),
                ..Default::default()
            }),
        );
        let msg = (tool.execute)("chatty-fail".into(), json!({ "command": "x" }), None, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(msg.contains(expected_msg), "{msg}");
        assert!(msg.contains(". Full output: "), "{msg}");
        assert!(!msg.contains("Full output: undefined"));
        let path_start = msg.find("Full output: ").unwrap() + "Full output: ".len();
        let path_end = msg[path_start..].find(']').unwrap() + path_start;
        let full_path = &msg[path_start..path_end];
        let persisted = std::fs::read_to_string(full_path).unwrap();
        assert!(persisted.contains("1\n2\n3"));
        assert!(persisted.contains("2998\n2999\n3000"));
    }
}

#[tokio::test]
#[serial]
async fn bash_tool_spawn_errors_shell_path_stdin_prefix_utf8_and_long_line_n12062() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();

    // Missing cwd
    let bad_cwd = create_bash_tool("/this/directory/does/not/exist/12345", None);
    let err_cwd = (bad_cwd.execute)("bad-cwd".into(), json!({ "command": "echo hi" }), None, None)
        .await
        .unwrap_err();
    assert!(err_cwd.to_string().contains("Working directory does not exist"));

    // Spawn error ENOENT via shell config override
    {
        let _guard = override_get_shell_config(Arc::new(|_| {
            Ok(ShellConfig {
                shell: "/nonexistent-shell-path-xyz123".to_string(),
                args: vec!["-c".to_string()],
                command_transport: None,
            })
        }));
        let bash_bad_shell = create_bash_tool(&cwd, None);
        let err_spawn = (bash_bad_shell.execute)("bad-shell".into(), json!({ "command": "echo hi" }), None, None)
            .await
            .unwrap_err();
        assert!(err_spawn.to_string().contains("ENOENT"), "{err_spawn}");
    }

    // Custom shellPath rejects when non-existent
    let custom_ops = create_local_bash_operations_with_shell(Some("/custom/missing/bash".to_string()));
    let err_custom = custom_ops
        .exec("echo hi", &cwd, BashExecOptions::default())
        .await
        .unwrap_err();
    assert!(
        err_custom
            .to_string()
            .contains("Custom shell path not found: /custom/missing/bash")
    );

    // Stdin command transport (legacy WSL bash -s)
    {
        let _guard = override_get_shell_config(Arc::new(|_| {
            Ok(ShellConfig {
                shell: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "cat".to_string()],
                command_transport: Some(CommandTransport::Stdin),
            })
        }));
        let chunks = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let chunks_cb = Arc::clone(&chunks);
        let ops = create_local_bash_operations();
        let command = "name='World'; echo \"Hello, ${name}!\"";
        let res = ops
            .exec(
                command,
                &cwd,
                BashExecOptions {
                    on_data: Some(Arc::new(move |b: &[u8]| chunks_cb.lock().unwrap().extend_from_slice(b))),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(res.exit_code, Some(0));
        assert_eq!(String::from_utf8(chunks.lock().unwrap().clone()).unwrap(), command);
    }

    // Command prefix
    let prefixed = create_bash_tool(
        &cwd,
        Some(BashToolOptions {
            command_prefix: Some("export TEST_VAR=hello".to_string()),
            ..Default::default()
        }),
    );
    let pref_res = (prefixed.execute)("p1".into(), json!({ "command": "echo $TEST_VAR" }), None, None)
        .await
        .unwrap();
    assert_eq!(get_text_output(&pref_res).trim(), "hello");

    // Split UTF-8 chunks
    struct SplitUtf8Ops;
    impl BashOperations for SplitUtf8Ops {
        fn exec<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            options: BashExecOptions,
        ) -> BoxFuture<pi_js::Result<BashExecResult>> {
            Box::pin(async move {
                let bytes = "€\n".as_bytes();
                if let Some(cb) = &options.on_data {
                    cb(&bytes[..1]);
                    cb(&bytes[1..]);
                }
                Ok(BashExecResult { exit_code: Some(0) })
            })
        }
    }
    let utf8_bash = create_bash_tool(
        &cwd,
        Some(BashToolOptions {
            operations: Some(Arc::new(SplitUtf8Ops)),
            ..Default::default()
        }),
    );
    let utf8_res = (utf8_bash.execute)("utf8".into(), json!({ "command": "split" }), None, None)
        .await
        .unwrap();
    assert_eq!(get_text_output(&utf8_res).trim(), "€");

    // #12062: short line followed by over-long last line
    struct LongLastLineOps;
    impl BashOperations for LongLastLineOps {
        fn exec<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            options: BashExecOptions,
        ) -> BoxFuture<pi_js::Result<BashExecResult>> {
            Box::pin(async move {
                let long_line = format!("{}END", "y".repeat(219_000));
                let payload = format!("first line\n{long_line}\n");
                if let Some(cb) = &options.on_data {
                    for chunk in payload.as_bytes().chunks(8192) {
                        cb(chunk);
                    }
                }
                Ok(BashExecResult { exit_code: Some(0) })
            })
        }
    }
    let long_bash = create_bash_tool(
        &cwd,
        Some(BashToolOptions {
            operations: Some(Arc::new(LongLastLineOps)),
            ..Default::default()
        }),
    );
    let long_res = (long_bash.execute)("n12062".into(), json!({ "command": "bundle" }), None, None)
        .await
        .unwrap();
    let long_details: BashToolDetails = serde_json::from_value(long_res.details.clone().unwrap()).unwrap();
    assert!(long_details.truncation.unwrap().last_line_partial);
    let long_out = get_text_output(&long_res);
    assert!(long_out.contains("yyyyEND"));
    assert!(long_out.contains("[Showing last 50.0KB of line 2 "), "{long_out}");

    // Ensure create_bash_tool_definition_with_shell works too
    let _def = wrap_tool_definition(create_bash_tool_definition_with_shell(&cwd, None, None), None);
    let _ = get_shell_config(None).unwrap();
}

// =========================================================================
// Grep tool
// =========================================================================

#[tokio::test]
#[serial]
async fn grep_tool_core_and_regression_cases() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let grep = create_grep_tool(&cwd, None);

    // Single file search
    let f1 = dir.path().join("example.txt");
    std::fs::write(&f1, "first line\nmatch line\nlast line").unwrap();
    let r1 = (grep.execute)(
        "g1".into(),
        json!({ "pattern": "match", "path": f1.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(get_text_output(&r1).contains("example.txt:2: match line"));

    // #11398 lone CR keeps line numbers & text aligned with ripgrep
    let lone_cr = dir.path().join("lone-cr.txt");
    std::fs::write(&lone_cr, "before\rprogress\nneedle\nafter").unwrap();
    let r_cr = (grep.execute)(
        "g-cr".into(),
        json!({ "pattern": "needle", "path": lone_cr.to_string_lossy(), "context": 1 }),
        None,
        None,
    )
    .await
    .unwrap();
    let cr_out = get_text_output(&r_cr);
    assert!(cr_out.contains("lone-cr.txt-1- before\rprogress"), "{cr_out}");
    assert!(cr_out.contains("lone-cr.txt:2: needle"), "{cr_out}");
    assert!(cr_out.contains("lone-cr.txt-3- after"), "{cr_out}");

    // #9276 overlapping context and last match trailing context
    let rg_ctx = dir.path().join("rg-context.txt");
    std::fs::write(
        &rg_ctx,
        ["a", "match one", "b", "match two", "match three", "c", "match four"].join("\n"),
    )
    .unwrap();
    let r_rg_ctx = (grep.execute)(
        "g-9276".into(),
        json!({ "pattern": "match", "path": rg_ctx.to_string_lossy(), "context": 1, "limit": 2 }),
        None,
        None,
    )
    .await
    .unwrap();
    let first_section = get_text_output(&r_rg_ctx).split("\n\n").next().unwrap().to_string();
    assert_eq!(
        first_section,
        [
            "rg-context.txt-1- a",
            "rg-context.txt:2: match one",
            "rg-context.txt-3- b",
            "rg-context.txt:4: match two",
            "rg-context.txt-5- match three",
        ]
        .join("\n")
    );

    // #9276 keeps counting matches past byte cap
    let cap_file = dir.path().join("byte-cap.txt");
    let big_line = format!("match {}", "x".repeat(400));
    std::fs::write(&cap_file, vec![big_line; 200].join("\n")).unwrap();
    let r_cap = (grep.execute)(
        "g-cap".into(),
        json!({ "pattern": "match", "path": cap_file.to_string_lossy(), "limit": 150 }),
        None,
        None,
    )
    .await
    .unwrap();
    let cap_out = get_text_output(&r_cap);
    assert!(cap_out.contains("150 matches limit reached"));
    assert!(cap_out.contains("50.0KB limit reached"));
    let cap_details: GrepToolDetails = serde_json::from_value(r_cap.details.unwrap()).unwrap();
    assert_eq!(cap_details.match_limit_reached, Some(150));
    assert_eq!(cap_details.truncation.unwrap().total_lines, 150);

    // Huge context value (1e30)
    let huge_ctx = dir.path().join("huge-context.txt");
    std::fs::write(&huge_ctx, "before\nneedle\nafter").unwrap();
    let r_huge = (grep.execute)(
        "g-huge".into(),
        json!({ "pattern": "needle", "path": huge_ctx.to_string_lossy(), "context": 1e30 }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        get_text_output(&r_huge),
        "huge-context.txt-1- before\nhuge-context.txt:2: needle\nhuge-context.txt-3- after"
    );

    // #11423 UTF-16LE file with context
    let utf16_file = dir.path().join("utf16.txt");
    let text16 = "\u{FEFF}before\nneedle 中文\nafter\n";
    let mut le_bytes = Vec::new();
    for u in text16.encode_utf16() {
        le_bytes.extend_from_slice(&u.to_le_bytes());
    }
    std::fs::write(&utf16_file, &le_bytes).unwrap();
    let r_u16 = (grep.execute)(
        "g-u16".into(),
        json!({ "pattern": "needle", "path": utf16_file.to_string_lossy(), "context": 1 }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        get_text_output(&r_u16),
        "utf16.txt-1- before\nutf16.txt:2: needle 中文\nutf16.txt-3- after"
    );

    // #11203 ignores RIPGREP_CONFIG_PATH
    let cfg = dir.path().join("rg-config");
    let fix = dir.path().join("fixture.txt");
    std::fs::write(&cfg, "--fixed-strings\n").unwrap();
    std::fs::write(&fix, "aXb\na.b\n").unwrap();
    {
        let _env = EnvGuard::set("RIPGREP_CONFIG_PATH", Some(&cfg.to_string_lossy()));
        let r_cfg = (grep.execute)(
            "g-cfg".into(),
            json!({ "pattern": "a.b", "path": fix.to_string_lossy(), "literal": false }),
            None,
            None,
        )
        .await
        .unwrap();
        let cfg_out = get_text_output(&r_cfg);
        assert!(cfg_out.contains("fixture.txt:1: aXb"), "{cfg_out}");
        assert!(cfg_out.contains("fixture.txt:2: a.b"), "{cfg_out}");
    }

    // #11807 folder globs anchored to path (even through symlink) and `./` stripped
    #[cfg(unix)]
    {
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("migrations/main")).unwrap();
        std::fs::create_dir_all(repo.join("nested/migrations/main")).unwrap();
        std::fs::write(repo.join("migrations/main/a.js"), "hello\n").unwrap();
        std::fs::write(repo.join("nested/migrations/main/b.js"), "hello\n").unwrap();
        let link = dir.path().join("repo-link");
        std::os::unix::fs::symlink(&repo, &link).unwrap();

        for search_path in [&repo, &link] {
            let p_str = search_path.to_string_lossy();
            let anchored = (grep.execute)(
                "g-anc".into(),
                json!({ "pattern": "hello", "path": p_str, "glob": "migrations/main/*.js" }),
                None,
                None,
            )
            .await
            .unwrap();
            assert_eq!(get_text_output(&anchored), "migrations/main/a.js:1: hello");

            let dot_slash = (grep.execute)(
                "g-ds".into(),
                json!({ "pattern": "hello", "path": p_str, "glob": "./migrations/main/*.js" }),
                None,
                None,
            )
            .await
            .unwrap();
            assert_eq!(get_text_output(&dot_slash), "migrations/main/a.js:1: hello");

            let negated = (grep.execute)(
                "g-neg".into(),
                json!({ "pattern": "hello", "path": p_str, "glob": "!./migrations/**" }),
                None,
                None,
            )
            .await
            .unwrap();
            assert_eq!(get_text_output(&negated), "nested/migrations/main/b.js:1: hello");
        }
    }

    // Exact match limit vs fractional/over limit
    let exact_file = dir.path().join("exact-limit.txt");
    std::fs::write(&exact_file, "match one\nx\nmatch two\ny").unwrap();
    let r_exact = (grep.execute)(
        "g-exact".into(),
        json!({ "pattern": "match", "path": exact_file.to_string_lossy(), "limit": 2, "context": 1 }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(!get_text_output(&r_exact).contains("limit reached"));
    assert!(r_exact.details.is_none());

    let r_frac = (grep.execute)(
        "g-frac".into(),
        json!({ "pattern": "match", "path": exact_file.to_string_lossy(), "limit": 1.5 }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(get_text_output(&r_frac).contains("[1 matches limit reached. Use limit=2 for more, or refine pattern]"));

    // Flag injection prevention
    let marker = dir.path().join("grep-injection-marker");
    let payload = dir.path().join("payload.sh");
    std::fs::write(&payload, format!("#!/bin/sh\necho executed > {}\n", marker.display())).unwrap();
    let r_inj = (grep.execute)(
        "g-inj".into(),
        json!({ "pattern": format!("--pre={}", payload.display()), "path": cwd }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(get_text_output(&r_inj).contains("No matches found"));
    assert!(!marker.exists());
}

// =========================================================================
// Find tool
// =========================================================================

#[tokio::test]
async fn find_tool_core_and_regression_cases() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let find = create_find_tool(&cwd, None);

    std::fs::create_dir(dir.path().join(".secret")).unwrap();
    std::fs::write(dir.path().join(".secret/hidden.txt"), "hidden").unwrap();
    std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(dir.path().join("ignored.txt"), "ignored").unwrap();
    std::fs::write(dir.path().join("visible.txt"), "visible").unwrap();

    let res = (find.execute)("f1".into(), json!({ "pattern": "**/*.txt", "path": cwd }), None, None)
        .await
        .unwrap();
    let lines: Vec<String> = get_text_output(&res)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    assert!(lines.contains(&"visible.txt".to_string()));
    assert!(lines.contains(&".secret/hidden.txt".to_string()));
    assert!(!lines.contains(&"ignored.txt".to_string()));

    // Glob parse error surfaces cleanly
    let bad_glob = (find.execute)("f2".into(), json!({ "pattern": "[", "path": cwd }), None, None)
        .await
        .unwrap_err();
    assert!(bad_glob.to_string().to_lowercase().contains("glob") || bad_glob.to_string().contains("fd"));

    // Flag-like pattern treated safely
    let flag_pat = (find.execute)("f3".into(), json!({ "pattern": "--help", "path": cwd }), None, None)
        .await
        .unwrap();
    assert!(get_text_output(&flag_pat).contains("No files found matching pattern"));

    // #12020 leading `./` strip
    let dir2 = TempDir::new().unwrap();
    let cwd2 = dir2.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(dir2.path().join("src")).unwrap();
    std::fs::write(dir2.path().join("src/a.ts"), "a").unwrap();
    std::fs::write(dir2.path().join("package.json"), "{}").unwrap();
    std::fs::write(dir2.path().join("src/b.json"), "{}").unwrap();

    for pat in ["./src/*.ts", "./src/**/*.ts", "././src/*.ts"] {
        let r = (find.execute)("f-ds".into(), json!({ "pattern": pat, "path": cwd2 }), None, None)
            .await
            .unwrap();
        assert_eq!(get_text_output(&r), "src/a.ts");
    }
    let json_res = (find.execute)(
        "f-ds-json".into(),
        json!({ "pattern": "./*.json", "path": cwd2 }),
        None,
        None,
    )
    .await
    .unwrap();
    let mut json_lines: Vec<String> = get_text_output(&json_res).lines().map(ToString::to_string).collect();
    json_lines.sort();
    assert_eq!(json_lines, vec!["package.json", "src/b.json"]);

    // #7121 exact limit vs over limit + fractional/small limit
    let dir3 = TempDir::new().unwrap();
    let cwd3 = dir3.path().to_string_lossy().into_owned();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir3.path().join(name), name).unwrap();
    }
    let exact = (find.execute)(
        "f-ex".into(),
        json!({ "pattern": "*.txt", "path": cwd3, "limit": 3 }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(exact.details.is_none());

    let over = (find.execute)(
        "f-ov".into(),
        json!({ "pattern": "*.txt", "path": cwd3, "limit": 2 }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(get_text_output(&over).contains("[2 results limit reached. Use limit=4 for more, or refine pattern]"));

    for lim in [json!(0), json!(-5), json!(0.5), json!(1.9)] {
        let sm = (find.execute)(
            "f-sm".into(),
            json!({ "pattern": "*.txt", "path": cwd3, "limit": lim }),
            None,
            None,
        )
        .await
        .unwrap();
        assert!(get_text_output(&sm).contains("[1 results limit reached. Use limit=2 for more, or refine pattern]"));
    }

    // Byte cap totals
    let dir4 = TempDir::new().unwrap();
    let cwd4 = dir4.path().to_string_lossy().into_owned();
    let names: Vec<String> = (0..300).map(|i| format!("{i:03}{}.txt", "x".repeat(193))).collect();
    for n in &names {
        std::fs::write(dir4.path().join(n), "").unwrap();
    }
    let expected = truncate_head(
        &names.join("\n"),
        Some(TruncationOptions {
            max_lines: Some(usize::MAX),
            max_bytes: None,
        }),
    );
    let cap = (find.execute)("f-cap".into(), json!({ "pattern": "*.txt", "path": cwd4 }), None, None)
        .await
        .unwrap();
    let cap_details: FindToolDetails = serde_json::from_value(cap.details.clone().unwrap()).unwrap();
    assert_eq!(cap_details.truncation.unwrap().output_lines, expected.output_lines);
    assert!(get_text_output(&cap).ends_with("[50.0KB limit reached]"));

    // Custom FindOperations requests limit + 1
    let requested = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
    struct CustomGlobOps {
        items: Vec<String>,
        requested: Arc<std::sync::Mutex<Vec<usize>>>,
    }
    impl FindOperations for CustomGlobOps {
        fn exists(&self, _: &str) -> BoxFuture<bool> {
            Box::pin(async { true })
        }
        fn glob(&self, _: &str, _: &str, opts: FindGlobOptions) -> BoxFuture<pi_js::Result<Vec<String>>> {
            self.requested.lock().unwrap().push(opts.limit);
            let out: Vec<String> = self.items.iter().take(opts.limit).cloned().collect();
            Box::pin(async move { Ok(out) })
        }
    }

    let c1 = create_find_tool_definition(
        &cwd,
        Some(FindToolOptions {
            operations: Some(Arc::new(CustomGlobOps {
                items: vec!["a.txt".into(), "b.txt".into()],
                requested: Arc::clone(&requested),
            })),
        }),
    );
    let exact_c = (c1.execute)(
        "c1".into(),
        json!({ "pattern": "*.txt", "limit": 2 }),
        None,
        None,
        fake_ctx(&cwd),
    )
    .await
    .unwrap();
    assert_eq!(get_text_output(&exact_c), "a.txt\nb.txt");
    assert!(exact_c.details.is_none());

    let c2 = create_find_tool_definition(
        &cwd,
        Some(FindToolOptions {
            operations: Some(Arc::new(CustomGlobOps {
                items: vec!["a.txt".into(), "b.txt".into(), "c.txt".into()],
                requested: Arc::clone(&requested),
            })),
        }),
    );
    let over_c = (c2.execute)(
        "c2".into(),
        json!({ "pattern": "*.txt", "limit": 0 }),
        None,
        None,
        fake_ctx(&cwd),
    )
    .await
    .unwrap();
    assert_eq!(get_text_output(&over_c), "a.txt\n\n[1 results limit reached]");
    assert_eq!(*requested.lock().unwrap(), vec![3, 2]);
}

// =========================================================================
// Tool ctx.cwd resolution
// =========================================================================

#[tokio::test]
#[serial]
async fn built_in_tools_use_ctx_cwd_when_provided() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let ctx = fake_ctx(&cwd);

    // read
    std::fs::write(dir.path().join("ctx-read.txt"), "hello from ctx.cwd").unwrap();
    let r_read = (create_read_tool_definition("/", None).execute)(
        "c-read".into(),
        json!({ "path": "ctx-read.txt" }),
        None,
        None,
        ctx.clone(),
    )
    .await
    .unwrap();
    assert!(get_text_output(&r_read).contains("hello from ctx.cwd"));

    // write
    let _ = (create_write_tool_definition("/", None).execute)(
        "c-write".into(),
        json!({ "path": "ctx-write.txt", "content": "written via ctx.cwd" }),
        None,
        None,
        ctx.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ctx-write.txt")).unwrap(),
        "written via ctx.cwd"
    );

    // edit
    let edit_file = dir.path().join("ctx-edit.txt");
    std::fs::write(&edit_file, "old text").unwrap();
    let _ = (create_edit_tool_definition("/", None).execute)(
        "c-edit".into(),
        json!({ "path": "ctx-edit.txt", "edits": [{ "oldText": "old text", "newText": "new text" }] }),
        None,
        None,
        ctx.clone(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&edit_file).unwrap(), "new text");

    // grep
    std::fs::write(dir.path().join("ctx-grep.txt"), "match in ctx.cwd").unwrap();
    let r_grep = (create_grep_tool_definition("/", None).execute)(
        "c-grep".into(),
        json!({ "pattern": "match" }),
        None,
        None,
        ctx.clone(),
    )
    .await
    .unwrap();
    assert!(get_text_output(&r_grep).contains("ctx-grep.txt"));

    // find
    std::fs::write(dir.path().join("ctx-find.txt"), "find me").unwrap();
    let r_find = (create_find_tool_definition("/", None).execute)(
        "c-find".into(),
        json!({ "pattern": "ctx-find.txt" }),
        None,
        None,
        ctx.clone(),
    )
    .await
    .unwrap();
    assert!(get_text_output(&r_find).contains("ctx-find.txt"));

    // ls
    std::fs::write(dir.path().join("ctx-ls.txt"), "list me").unwrap();
    let r_ls = (create_ls_tool_definition("/", None).execute)("c-ls".into(), json!({}), None, None, ctx.clone())
        .await
        .unwrap();
    assert!(get_text_output(&r_ls).contains("ctx-ls.txt"));

    // bash
    let r_bash = (create_bash_tool_definition("/", None).execute)(
        "c-bash".into(),
        json!({ "command": "pwd -P" }),
        None,
        None,
        ctx,
    )
    .await
    .unwrap();
    let canon_dir = std::fs::canonicalize(dir.path()).unwrap();
    assert!(get_text_output(&r_bash).contains(canon_dir.to_string_lossy().as_ref()));
}

#[tokio::test]
#[serial]
async fn execute_bash_with_operations_sanitizes_ansi_and_persists_line_truncated_output() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let ops = create_local_bash_operations();

    // Preserves executeBash sanitization when using local bash operations
    let sanitized = execute_bash_with_operations(
        "printf '\\033[31mred\\033[0m\\r\\n'",
        &cwd,
        ops.as_ref(),
        Some(BashExecutorOptions::default()),
    )
    .await
    .unwrap();
    assert_eq!(sanitized.exit_code, Some(0));
    assert_eq!(sanitized.output, "red\n");

    // Persists full output when truncation happens by line count only
    let truncated = execute_bash_with_operations("seq 3000", &cwd, ops.as_ref(), None)
        .await
        .unwrap();
    assert!(truncated.truncated);
    let full_path = truncated.full_output_path.expect("full_output_path");
    assert!(std::path::Path::new(&full_path).exists(), "missing {full_path}");
    let full_output = std::fs::read_to_string(&full_path).unwrap();
    assert!(full_output.contains("1\n2\n3"));
    assert!(full_output.contains("2998\n2999\n3000"));
}

#[tokio::test]
#[serial]
async fn text_only_model_image_warning_stays_out_of_model_output() {
    use pi_coding_agent::modes::interactive::components::tool_execution::{ToolExecutionComponent, ToolRenderers};
    use pi_coding_agent::modes::interactive::theme::theme::init_theme;
    use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
    use pi_tui::{TUI, TuiMainScreen, strip_terminal_sequences};

    struct DummyTerminal;
    #[async_trait::async_trait]
    impl Terminal for DummyTerminal {
        fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
        fn stop(&self) {}
        async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
        fn write(&self, _data: &str) {}
        fn columns(&self) -> i64 {
            120
        }
        fn rows(&self) -> i64 {
            24
        }
        fn kitty_protocol_active(&self) -> bool {
            false
        }
        fn move_by(&self, _lines: i64) {}
        fn hide_cursor(&self) {}
        fn show_cursor(&self) {}
        fn clear_line(&self) {}
        fn clear_from_cursor(&self) {}
        fn clear_screen(&self) {}
        fn set_title(&self, _title: &str) {}
        fn set_progress(&self, _active: bool) {}
    }
    struct ThemeGuard;
    impl Drop for ThemeGuard {
        fn drop(&mut self) {
            init_theme(Some("dark"), None);
        }
    }

    init_theme(Some("dark"), Some(false));
    let _guard = ThemeGuard;

    let dir = TempDir::new().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let tiny_png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";
    let bytes = pi_js::b64::decode(tiny_png_b64);
    let img_path = dir.path().join("dot.png");
    std::fs::write(&img_path, bytes).unwrap();

    let ctx = fake_ctx(&cwd);
    let read_def = create_read_tool_definition(&cwd, None);
    let result = (read_def.execute)("call-img".into(), json!({ "path": "dot.png" }), None, None, ctx)
        .await
        .unwrap();
    let text = get_text_output(&result);
    assert!(text.contains("Read image file [image/png]"), "text output was: {text}");
    assert!(
        !text.contains("text-only model"),
        "model output leaked UI warning: {text}"
    );
    assert!(
        result
            .content
            .iter()
            .any(|b| matches!(b, UserContentBlock::Image(img) if img.mime_type == "image/png"))
    );

    let tui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(DummyTerminal), None, None));
    let component = ToolExecutionComponent::new(
        "read",
        "call-img",
        json!({ "path": "dot.png" }),
        None,
        Some(ToolRenderers::from_tool_definition(&read_def)),
        tui,
        &cwd,
    );
    component.update_result(
        pi_coding_agent::modes::interactive::components::tool_execution::ToolExecutionResult {
            content: result.content,
            details: result.details,
            is_error: false,
            duration_ms: None,
        },
        Some(false),
    );
    let rendered = strip_terminal_sequences(&component.render(120).join("\n"));
    assert!(rendered.contains("read"), "rendered: {rendered}");
    assert!(rendered.contains("dot.png"), "rendered: {rendered}");
}

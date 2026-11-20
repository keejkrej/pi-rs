//! Port of `packages/coding-agent/test/session-share.test.ts`.

use std::sync::{Arc, Mutex};

use indexmap::IndexSet;
use pi_agent_core::{AgentMessage, AgentState, ThinkingLevel};
use pi_ai::compat::get_model;
use pi_ai::{Message, UserMessage, UserMessageContent};
use pi_coding_agent::core::export_html::{ExportOptions, export_session_to_html};
use pi_coding_agent::core::session_manager::{NewSessionOptions, SessionManager};
use pi_coding_agent::modes::interactive::session_share::{
    SessionShareContext, SessionShareDestination, ShareSession, ShareToolSummary, export_session_for_share,
    share_session,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_js::BoxFuture;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{Component, Container, Spacer, TUI, TuiMainScreen};
use serde_json::Value;
use serial_test::serial;
use tempfile::TempDir;

fn make_session(dir: &std::path::Path, id: &str, text: &str) -> SessionManager {
    let cwd = dir.join(format!("cwd-{id}"));
    let sess_dir = dir.join(format!("sessions-{id}"));
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&sess_dir).unwrap();
    let sm = SessionManager::create(
        cwd.to_str().unwrap(),
        Some(sess_dir.to_str().unwrap()),
        Some(NewSessionOptions {
            id: Some(id.to_owned()),
            parent_session: None,
        }),
    )
    .unwrap();
    sm.append_message(AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: 1,
    })))
    .unwrap();
    sm
}

#[tokio::test]
async fn keeps_concurrent_session_exports_isolated() {
    let tmp = TempDir::new().unwrap();
    let sm_a = make_session(tmp.path(), "session-A", "marker-payload-A");
    let sm_b = make_session(tmp.path(), "session-B", "marker-payload-B");

    let out_a = tmp.path().join("share-A.html");
    let out_b = tmp.path().join("share-B.html");
    let model = get_model("anthropic", "claude-opus-4-6").unwrap();
    let state_a = AgentState {
        system_prompt: "prompt-A".to_owned(),
        model: model.clone(),
        thinking_level: ThinkingLevel::Off,
        tools: Vec::new(),
        messages: Vec::new(),
        is_streaming: false,
        stream_message: None,
        pending_tool_calls: IndexSet::new(),
        error: None,
    };
    let state_b = AgentState {
        system_prompt: "prompt-B".to_owned(),
        model,
        thinking_level: ThinkingLevel::Off,
        tools: Vec::new(),
        messages: Vec::new(),
        is_streaming: false,
        stream_message: None,
        pending_tool_calls: IndexSet::new(),
        error: None,
    };

    let (res_a, res_b) = tokio::join!(
        export_session_to_html(
            &sm_a,
            Some(&state_a),
            Some(ExportOptions {
                output_path: Some(out_a.to_string_lossy().into_owned()),
                theme_name: None,
                tool_renderer: None,
                theme_override: None,
            }),
        ),
        export_session_to_html(
            &sm_b,
            Some(&state_b),
            Some(ExportOptions {
                output_path: Some(out_b.to_string_lossy().into_owned()),
                theme_name: None,
                tool_renderer: None,
                theme_override: None,
            }),
        ),
    );
    res_a.unwrap();
    res_b.unwrap();

    let html_a = std::fs::read_to_string(&out_a).unwrap();
    let html_b = std::fs::read_to_string(&out_b).unwrap();
    let decode_data = |html: &str| -> String {
        let marker = "<script id=\"session-data\" type=\"application/json\">";
        let start = html.find(marker).unwrap() + marker.len();
        let end = html[start..].find("</script>").unwrap() + start;
        String::from_utf8(pi_js::b64::decode(&html[start..end])).unwrap()
    };
    let data_a = decode_data(&html_a);
    let data_b = decode_data(&html_b);
    assert!(data_a.contains("marker-payload-A"));
    assert!(!data_a.contains("marker-payload-B"));
    assert!(data_b.contains("marker-payload-B"));
    assert!(!data_b.contains("marker-payload-A"));
}

#[tokio::test]
#[serial]
async fn share_session_exports_for_share_and_reports_unconfigured_radius_offline() {
    struct ThemeGuard;
    impl Drop for ThemeGuard {
        fn drop(&mut self) {
            init_theme(Some("dark"), None);
        }
    }
    struct DummyTerminal;
    #[async_trait::async_trait]
    impl Terminal for DummyTerminal {
        fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
        fn stop(&self) {}
        async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
        fn write(&self, _data: &str) {}
        fn columns(&self) -> i64 {
            80
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
    struct StubShareSession {
        sm: SessionManager,
        exported_paths: Arc<Mutex<Vec<String>>>,
    }
    impl ShareSession for StubShareSession {
        fn system_prompt(&self) -> String {
            "share test system prompt".to_owned()
        }
        fn state_tools(&self) -> Vec<ShareToolSummary> {
            Vec::new()
        }
        fn export_session_jsonl(
            &self,
            file_path: &str,
            trailing_entries_fn: Arc<dyn Fn(Option<&str>, &str) -> Vec<Value> + Send + Sync>,
        ) -> pi_coding_agent::Result<()> {
            let cb = |parent_id: Option<&str>, ts: &str| trailing_entries_fn(parent_id, ts);
            pi_coding_agent::core::session_export::export_session_to_jsonl(&self.sm, Some(file_path), Some(&cb))?;
            self.exported_paths.lock().unwrap().push(file_path.to_owned());
            Ok(())
        }
        fn export_to_html(&self, html_path: &str, _theme_name: &str) -> BoxFuture<pi_coding_agent::Result<()>> {
            let sm = self.sm.clone();
            let path = html_path.to_owned();
            Box::pin(async move {
                export_session_to_html(
                    &sm,
                    None,
                    Some(ExportOptions {
                        output_path: Some(path),
                        theme_name: None,
                        tool_renderer: None,
                        theme_override: None,
                    }),
                )
                .await
                .map(|_| ())
            })
        }
        fn session_share_destination(&self) -> SessionShareDestination {
            SessionShareDestination::Radius
        }
        fn has_radius_provider(&self) -> bool {
            false
        }
        fn get_radius_auth_token(&self, _min_oauth_validity_ms: i64) -> BoxFuture<Option<String>> {
            Box::pin(async { None })
        }
    }

    init_theme(Some("dark"), Some(false));
    let _guard = ThemeGuard;

    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let sm = SessionManager::in_memory(Some(cwd), None, None).unwrap();
    sm.append_message(AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text("share html probe".to_owned()),
        timestamp: 1,
    })))
    .unwrap();

    let exported_paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let statuses = Arc::new(Mutex::new(Vec::<String>::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let st_ref = Arc::clone(&statuses);
    let err_ref = Arc::clone(&errors);
    let tui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(DummyTerminal), Some(false), None));
    let editor: Arc<dyn Component> = Arc::new(Spacer::new(Some(1)));

    let session = Arc::new(StubShareSession {
        sm,
        exported_paths: Arc::clone(&exported_paths),
    });
    let _ = export_session_for_share;
    share_session(&SessionShareContext {
        session,
        ui: tui,
        editor_container: Container::new(),
        editor,
        show_status: Arc::new(move |msg| st_ref.lock().unwrap().push(msg.to_owned())),
        show_error: Arc::new(move |msg| err_ref.lock().unwrap().push(msg.to_owned())),
    })
    .await;

    assert_eq!(exported_paths.lock().unwrap().len(), 1);
    let errs = errors.lock().unwrap().clone();
    assert!(
        errs.iter()
            .any(|e| e.contains("Radius authentication is not configured")),
        "errors: {errs:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn forces_gist_sharing_without_checking_radius_authentication_10870() {
    use pi_js::env::testing::EnvGuard;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let bin_dir = tempfile::TempDir::new().unwrap();
    let args_file = bin_dir.path().join("gh-args.txt");
    let html_file = bin_dir.path().join("gh-html.txt");
    let gh_path = bin_dir.path().join("gh");
    std::fs::write(
        &gh_path,
        "#!/bin/sh\nif [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then exit 0; fi\nif [ \"$1\" = \"gist\" ] && [ \"$2\" = \"create\" ]; then\n  printf '%s\\n' \"$@\" > \"$GH_ARGS_FILE\"\n  cat \"$4\" > \"$GH_HTML_FILE\"\n  printf 'https://gist.github.com/test/forced-gist\\n'\n  exit 0\nfi\nexit 1\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&gh_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&gh_path, perms).unwrap();

    let old_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{old_path}", bin_dir.path().to_string_lossy());
    let _path_guard = EnvGuard::set("PATH", Some(&new_path));
    let _args_guard = EnvGuard::set("GH_ARGS_FILE", Some(args_file.to_string_lossy().as_ref()));
    let _html_guard = EnvGuard::set("GH_HTML_FILE", Some(html_file.to_string_lossy().as_ref()));

    struct ForcedGistSession {
        radius_checks: Arc<AtomicUsize>,
    }

    impl ShareSession for ForcedGistSession {
        fn system_prompt(&self) -> String {
            String::new()
        }
        fn state_tools(&self) -> Vec<ShareToolSummary> {
            Vec::new()
        }
        fn export_session_jsonl(
            &self,
            file_path: &str,
            _trailing_entries_fn: Arc<dyn Fn(Option<&str>, &str) -> Vec<Value> + Send + Sync>,
        ) -> pi_js::Result<()> {
            std::fs::write(file_path, "{}\n").map_err(|e| pi_js::Error::msg(e.to_string()))
        }
        fn export_to_html(&self, html_path: &str, _theme_name: &str) -> pi_js::BoxFuture<pi_js::Result<()>> {
            let p = html_path.to_owned();
            Box::pin(async move { std::fs::write(&p, "html").map_err(|e| pi_js::Error::msg(e.to_string())) })
        }
        fn session_share_destination(&self) -> SessionShareDestination {
            SessionShareDestination::Gist
        }
        fn has_radius_provider(&self) -> bool {
            self.radius_checks.fetch_add(1, Ordering::SeqCst);
            true
        }
        fn get_radius_auth_token(&self, _min_oauth_validity_ms: i64) -> pi_js::BoxFuture<Option<String>> {
            self.radius_checks.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Some("tok".to_owned()) })
        }
    }

    struct LocalNoopTerminal;

    #[async_trait::async_trait]
    impl pi_tui::terminal::Terminal for LocalNoopTerminal {
        fn start(
            &self,
            _on_input: pi_tui::terminal::TerminalInputHandler,
            _on_resize: pi_tui::terminal::TerminalResizeHandler,
        ) {
        }
        fn stop(&self) {}
        async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
        fn write(&self, _data: &str) {}
        fn columns(&self) -> i64 {
            80
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

    init_theme(Some("dark"), Some(false));
    let radius_checks = Arc::new(AtomicUsize::new(0));
    let statuses = Arc::new(Mutex::new(Vec::<String>::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let status_c = Arc::clone(&statuses);
    let err_c = Arc::clone(&errors);
    let editor: Arc<dyn Component> = Arc::new(pi_tui::Text::new(None, None, None, None));
    let ui: Arc<dyn TUI + Send + Sync> = Arc::new(pi_tui::TuiMainScreen::new(Arc::new(LocalNoopTerminal), None, None));
    let editor_container = Container::new();
    editor_container.add_child(Arc::clone(&editor));

    share_session(&SessionShareContext {
        session: Arc::new(ForcedGistSession {
            radius_checks: Arc::clone(&radius_checks),
        }),
        ui,
        editor_container,
        editor,
        show_status: Arc::new(move |msg| status_c.lock().unwrap().push(msg.to_owned())),
        show_error: Arc::new(move |msg| err_c.lock().unwrap().push(msg.to_owned())),
    })
    .await;

    assert_eq!(radius_checks.load(Ordering::SeqCst), 0);
    assert!(
        errors.lock().unwrap().is_empty(),
        "errors: {:?}",
        *errors.lock().unwrap()
    );
    let recorded_args: Vec<String> = std::fs::read_to_string(&args_file)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(&recorded_args[..3], &["gist", "create", "--public=false"]);
    assert_eq!(std::fs::read_to_string(&html_file).unwrap(), "html");
    let status_joined = statuses.lock().unwrap().join("\n");
    assert!(
        status_joined.contains("forced-gist"),
        "expected forced-gist viewer URL in statuses: {status_joined}"
    );
}

use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use pi_ai::compat::register_faux_provider;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxResponseStep, RegisterFauxProviderOptions, faux_assistant_message,
};
use pi_gui::{
    BridgeCommand, ContentBlockView, GuiBridge, InspectorTab, LaunchConfig, MessageRoleView, PiGuiAction, PiGuiState,
    PluginKind, app_view, poll_state,
};
use rune::{Env, Runtime};
use tempfile::TempDir;

fn wait_until<F>(rt: &mut Runtime<PiGuiState>, signal: &(Mutex<bool>, Condvar), label: &str, mut pred: F)
where
    F: FnMut(&PiGuiState) -> bool,
{
    for _ in 0..80 {
        rt.frame(0.016);
        if pred(&rt.state) {
            return;
        }
        let (lock, cvar) = signal;
        if let Ok(mut guard) = lock.lock() {
            if !*guard {
                let (g, _) = cvar
                    .wait_timeout_while(guard, Duration::from_millis(100), |woken| !*woken)
                    .unwrap_or_else(|e| e.into_inner());
                guard = g;
            }
            *guard = false;
        }
        rt.frame(0.016);
        if pred(&rt.state) {
            return;
        }
    }
    let s = &rt.state;
    panic!(
        "timed out waiting for [{label}]: active_thread={:?}, status_banner={:?}, diffs={}, plugins={:?}, has_faux_model={}, msgs={}, active_stream={:?}, open_file={:?}, term_lines={:?}",
        s.active_thread_id,
        s.status_banner,
        s.diff.files.len(),
        s.plugins.iter().map(|p| &p.name).collect::<Vec<_>>(),
        s.models.iter().any(|m| m.provider == "faux-prov"),
        s.messages.len(),
        s.active_stream.as_ref().map(|a| &a.partial_text),
        s.open_file.as_ref().map(|f| &f.path),
        s.terminal.lines
    );
}

#[test]
fn bridge_initializes_temp_workspace_with_pi_session_quickjs_plugin_and_faux_stream() {
    let temp = TempDir::new().expect("create temp workspace");
    let root = temp.path();

    // Initialize git repository with a real modified file so DiffUpdated and FilesUpdated populate.
    let readme = root.join("README.md");
    fs::write(&readme, "line one\n").expect("write README.md");
    let _ = Command::new("git").args(["init", "-q"]).current_dir(root).status();
    let _ = Command::new("git")
        .args(["add", "README.md"])
        .current_dir(root)
        .status();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.name=Pi",
            "-c",
            "user.email=pi@example.com",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(root)
        .status();
    fs::write(&readme, "line one\nline two\n").expect("modify README.md");

    // Register an offline faux streaming provider and queue deterministic assistant text.
    let faux = register_faux_provider(Some(RegisterFauxProviderOptions {
        api: Some("faux-gui-test".to_string()),
        provider: Some("faux-prov".to_string()),
        models: None,
        deferred: None,
        tokens_per_second: None,
        token_size: None,
    }));
    faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("Faux streaming response complete!".to_string()),
        None,
    ))]);

    // Seed offline .pi v3 session JSONL and an unmodified QuickJS .ts extension in temp workspace.
    let sessions_dir = root.join(".pi").join("sessions");
    let extensions_dir = root.join(".pi").join("extensions");
    fs::create_dir_all(&sessions_dir).expect("create .pi/sessions");
    fs::create_dir_all(&extensions_dir).expect("create .pi/extensions");

    let session_file = sessions_dir.join("test-sess-1.jsonl");
    let cwd_escaped = root.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    let session_jsonl = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"test-sess-1\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"{cwd_escaped}\"}}\n\
         {{\"type\":\"message\",\"id\":\"m1\",\"parentId\":null,\"timestamp\":\"2026-01-01T00:00:01.000Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"Hello pi-gui\"}}],\"timestamp\":1767225601000}}}}\n"
    );
    fs::write(&session_file, session_jsonl).expect("write session jsonl");

    let ts_extension = r#"
export default function (pi) {
    pi.registerTool({
        name: "workspace_ping",
        description: "Offline QuickJS test tool",
        parameters: { type: "object", properties: {} },
        async execute() {
            return { content: [{ type: "text", text: "pong" }] };
        },
    });
    pi.registerCommand("ping", {
        description: "QuickJS test command",
        async handler() {},
    });
    pi.registerProvider("faux-prov", {
        api: "faux-gui-test",
        baseUrl: "http://127.0.0.1:0",
        apiKey: "faux-key",
        models: [{
            type: "chat",
            id: "faux-model",
            name: "Faux Model",
            reasoning: false,
            input: ["text"],
            cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
            contextWindow: 8192,
            maxTokens: 1024,
        }],
    });
}
"#;
    fs::write(extensions_dir.join("ping.ts"), ts_extension).expect("write QuickJS .ts extension");

    let config = LaunchConfig {
        cwd: root.to_string_lossy().to_string(),
        session: Some(session_file.to_string_lossy().to_string()),
        session_dir: Some(sessions_dir.to_string_lossy().to_string()),
        thinking: Some("high".to_string()),
        reduced_motion: true,
        ..LaunchConfig::default()
    };

    let wake_count = Arc::new(AtomicUsize::new(0));
    let signal = Arc::new((Mutex::new(false), Condvar::new()));
    let wake_clone = Arc::clone(&wake_count);
    let signal_clone = Arc::clone(&signal);
    let waker: rune::Waker = Arc::new(move || {
        wake_clone.fetch_add(1, Ordering::SeqCst);
        let (lock, cvar) = &*signal_clone;
        if let Ok(mut woken) = lock.lock() {
            *woken = true;
            cvar.notify_all();
        }
    });

    let bridge = GuiBridge::spawn(config.clone(), waker);
    let mut state = PiGuiState::new(config);
    state.bridge = Some(bridge.sender());
    state.inspector_pane.set_open(true);
    state.sidebar_pane.set_open(true);

    let mut rt = Runtime::new(state, app_view, (1280.0, 800.0), Env::new(true, 2.0));
    rt.poll = Some(poll_state);

    wait_until(&mut rt, &signal, "init_session_diff_quickjs_faux", |s| {
        s.active_thread_id.as_deref() == Some("test-sess-1")
            && !s.diff.files.is_empty()
            && !s.files.is_empty()
            && s.plugins.iter().any(|p| p.name == "ping.ts")
            && s.models
                .iter()
                .any(|m| m.provider == "faux-prov" && m.id == "faux-model")
    });

    assert!(
        wake_count.load(Ordering::SeqCst) >= 1,
        "expected bridge to invoke waker"
    );
    assert_eq!(rt.state.context_usage.thinking_level, "high");
    assert!(
        rt.state.messages.iter().any(|m| m.role == MessageRoleView::User
            && m.blocks
                .iter()
                .any(|b| matches!(b, ContentBlockView::Text(t) if t.contains("Hello pi-gui")))),
        "expected .pi session message hydrated: {:?}",
        rt.state.messages
    );
    assert!(
        rt.state
            .plugins
            .iter()
            .any(|p| p.name == "ping.ts" && p.kind == PluginKind::QuickJs),
        "expected QuickJS ping.ts plugin in {:?}",
        rt.state.plugins
    );
    assert!(rt.state.diff.files.iter().any(|f| f.path == "README.md"));
    assert!(rt.state.file_tree.visible_len() > 0);

    // Select faux provider model, submit a prompt, and wait for offline streaming assistant turn.
    rt.state.dispatch(
        PiGuiAction::SelectModel {
            provider: "faux-prov".to_string(),
            model_id: "faux-model".to_string(),
        },
        None,
    );
    rt.state.composer_edit.replace_text("Ping via faux provider");
    rt.state.dispatch(PiGuiAction::SubmitPrompt, None);

    wait_until(&mut rt, &signal, "faux_stream_complete", |s| {
        s.active_stream.is_none()
            && s.messages.iter().any(|m| {
                m.role == MessageRoleView::Assistant
                    && m.blocks.iter().any(
                        |b| matches!(b, ContentBlockView::Text(t) if t.contains("Faux streaming response complete!")),
                    )
            })
    });

    // Exercise live ReadFile and RunShellCommand through GuiBridge.
    rt.state.dispatch(PiGuiAction::OpenFile("README.md".to_string()), None);
    rt.state
        .dispatch(PiGuiAction::SelectInspectorTab(InspectorTab::Terminal), None);
    rt.state
        .dispatch(PiGuiAction::RunTerminalCommand("echo live-bridge-ok".to_string()), None);

    wait_until(&mut rt, &signal, "open_file_and_shell", |s| {
        s.open_file.as_ref().is_some_and(|f| f.content.contains("line two"))
            && !s.terminal.running
            && s.terminal.lines.iter().any(|l| l.text.contains("live-bridge-ok"))
    });

    for _ in 0..4 {
        rt.frame(0.016);
    }
    for key in [
        "workspace.sidebar",
        "workspace.main.transcript",
        "workspace.main.composer",
        "right_panel",
        "right_panel.terminal.surface",
    ] {
        let rect = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some(key))
            .map(|e| e.rect)
            .unwrap_or_else(|| panic!("expected layout rect for '{key}'"));
        assert!(rect[2] > 0.0 && rect[3] > 0.0, "non-zero rect required for '{key}'");
    }

    bridge.sender().send(BridgeCommand::Shutdown);
    faux.unregister();
}

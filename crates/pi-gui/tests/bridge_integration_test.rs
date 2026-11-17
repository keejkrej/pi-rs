use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use pi_gui::{BridgeCommand, GuiBridge, LaunchConfig, PiGuiState, app_view, poll_state};
use rune::{Env, Runtime};
use tempfile::TempDir;

#[test]
fn bridge_initializes_temp_workspace_with_pi_session_and_quickjs_plugin() {
    let temp = TempDir::new().expect("create temp workspace");
    let root = temp.path();

    // Seed offline .pi session JSONL and an unmodified QuickJS .ts extension in temp workspace.
    let sessions_dir = root.join(".pi").join("sessions");
    let extensions_dir = root.join(".pi").join("extensions");
    fs::create_dir_all(&sessions_dir).expect("create .pi/sessions");
    fs::create_dir_all(&extensions_dir).expect("create .pi/extensions");

    let session_jsonl = "{\"type\":\"session\",\"id\":\"test-sess-1\",\"cwd\":\"/tmp\"}\n\
         {\"type\":\"message\",\"id\":\"m1\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Hello pi-gui\"}]}}\n";
    fs::write(sessions_dir.join("test-sess-1.jsonl"), session_jsonl).expect("write session jsonl");

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
}
"#;
    fs::write(extensions_dir.join("ping.ts"), ts_extension).expect("write QuickJS .ts extension");

    let config = LaunchConfig {
        cwd: root.to_string_lossy().to_string(),
        session: Some("test-sess-1".to_string()),
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
        if let Ok(mut notified) = lock.lock() {
            *notified = true;
            cvar.notify_all();
        }
    });

    let bridge = GuiBridge::spawn(config.clone(), waker);
    let mut state = PiGuiState::new(config);
    state.bridge = Some(bridge.sender());

    let mut rt = Runtime::new(state, app_view, (1280.0, 800.0), Env::new(true, 2.0));
    rt.poll = Some(poll_state);

    let (lock, cvar) = &*signal;
    if let Ok(guard) = lock.lock() {
        let _ = cvar.wait_timeout_while(guard, Duration::from_secs(5), |notified| !*notified);
    }
    rt.frame(0.016);

    assert!(
        wake_count.load(Ordering::SeqCst) >= 1,
        "expected bridge to invoke waker"
    );
    assert_eq!(rt.state.active_thread_id.as_deref(), Some("test-sess-1"));
    assert_eq!(rt.state.context_usage.thinking_level, "high");

    // Verify rendered frames after bridge event hydration produce valid non-zero layout boxes.
    for _ in 0..4 {
        rt.frame(0.016);
    }
    let transcript_rect = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("transcript-scroll-host"))
        .map(|e| e.rect)
        .expect("transcript-scroll-host layout present");
    assert!(transcript_rect[2] > 0.0 && transcript_rect[3] > 0.0);

    bridge.sender().send(BridgeCommand::Shutdown);
}

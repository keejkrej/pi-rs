//! Port of packages/coding-agent/test/session-selector-rename.test.ts

use std::sync::{Arc, Mutex};

use pi_coding_agent::modes::interactive::components::session_selector::{
    RenameSessionFn, SessionListProgress, SessionSelectorComponent, SessionSelectorOptions, SessionsLoader,
};
use pi_coding_agent::modes::interactive::components::session_selector_search::SessionInfo;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result};
use pi_tui::keybindings::KeyIdOrList;
use pi_tui::{
    Component, KeybindingDefinition, KeybindingsConfig, KeybindingsManager, TUI_KEYBINDINGS, set_keybindings,
};
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn app_keybindings(user_bindings: Option<KeybindingsConfig>) -> Arc<KeybindingsManager> {
    let mut defs = TUI_KEYBINDINGS.clone();
    // WIRE(@tui-w3): Replace local app.session.* definitions with crate::core::keybindings::KeybindingsManager once @mcp lands.
    defs.insert(
        "app.session.togglePath".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+p".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.session.toggleSort".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+s".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.session.toggleNamedFilter".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+n".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.session.rename".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+r".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.session.delete".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+d".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.session.deleteNoninvasive".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+backspace".to_string()),
            description: None,
        },
    );
    Arc::new(KeybindingsManager::new(defs, user_bindings))
}

fn setup() -> (TestGuard, Arc<KeybindingsManager>) {
    init_theme(Some("dark"), None);
    let kb = app_keybindings(None);
    set_keybindings((*kb).clone());
    (TestGuard, kb)
}

async fn flush_promises() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

fn make_session(id: &str, name: Option<&str>, modified: i64) -> SessionInfo {
    SessionInfo {
        path: format!("/tmp/{id}.jsonl"),
        id: id.to_string(),
        cwd: String::new(),
        name: name.map(str::to_string),
        parent_session_path: None,
        created: 0,
        modified,
        message_count: 1,
        first_message: "hello".to_string(),
        all_messages_text: "hello".to_string(),
    }
}

fn loader_from_vec(sessions: Vec<SessionInfo>) -> SessionsLoader {
    Arc::new(
        move |_: Option<SessionListProgress>, _: AbortSignal| -> BoxFuture<Result<Vec<SessionInfo>>> {
            let s = sessions.clone();
            Box::pin(async move { Ok(s) })
        },
    )
}

const CTRL_R: &str = "\x1b[114;5u";

#[tokio::test]
#[serial]
async fn shows_rename_hint_in_interactive_resume_picker_configuration() {
    let (_g, kb) = setup();
    let sessions = vec![make_session("a", None, 0)];
    let selector = SessionSelectorComponent::new(
        loader_from_vec(sessions),
        loader_from_vec(Vec::new()),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            show_rename_hint: Some(true),
            keybindings: Some(kb),
            ..Default::default()
        },
        None,
    );
    flush_promises().await;

    let output = selector.render(120).join("\n");
    assert!(output.contains("ctrl+r"));
    assert!(output.contains("rename"));
}

#[tokio::test]
#[serial]
async fn does_not_show_rename_hint_in_cli_resume_picker_configuration() {
    let (_g, kb) = setup();
    let sessions = vec![make_session("a", None, 0)];
    let selector = SessionSelectorComponent::new(
        loader_from_vec(sessions),
        loader_from_vec(Vec::new()),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            show_rename_hint: Some(false),
            keybindings: Some(kb),
            ..Default::default()
        },
        None,
    );
    flush_promises().await;

    let output = selector.render(120).join("\n");
    assert!(!output.contains("ctrl+r"));
    assert!(!output.contains("rename"));
}

#[tokio::test]
#[serial]
async fn enters_rename_mode_on_ctrl_r_and_submits_with_enter() {
    let (_g, kb) = setup();
    let sessions = vec![make_session("a", Some("Old"), 0)];
    let calls: Arc<Mutex<Vec<(String, Option<String>)>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = Arc::clone(&calls);
    let rename_session: RenameSessionFn = Arc::new(move |path, name| {
        calls_clone.lock().unwrap().push((path, name));
        Box::pin(async { Ok(()) })
    });

    let selector = SessionSelectorComponent::new(
        loader_from_vec(sessions.clone()),
        loader_from_vec(Vec::new()),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            rename_session: Some(rename_session),
            show_rename_hint: Some(true),
            keybindings: Some(kb),
        },
        None,
    );
    flush_promises().await;

    selector.get_session_list().handle_input(CTRL_R);
    flush_promises().await;

    let output = selector.render(120).join("\n");
    assert!(output.contains("Rename Session"));
    assert!(!output.contains("Resume Session"));

    selector.handle_input("X");
    selector.handle_input("\r");
    flush_promises().await;

    let guard = calls.lock().unwrap();
    assert_eq!(guard.len(), 1);
    assert_eq!(guard[0], (sessions[0].path.clone(), Some("XOld".to_string())));
}

#[tokio::test]
#[serial]
async fn keeps_renamed_session_selected_when_reload_starts_with_another_session() {
    let (_g, kb) = setup();
    let sessions: Vec<SessionInfo> = (1..=5)
        .map(|i| make_session(&format!("s{i}"), None, 10_000 - i * 1000))
        .collect();
    let loads = Arc::new(Mutex::new(0usize));
    let loads_clone = Arc::clone(&loads);
    let loader_sessions = sessions.clone();

    let current_loader: SessionsLoader = Arc::new(move |on_progress, _| {
        let prev = {
            let mut l = loads_clone.lock().unwrap();
            let p = *l;
            *l += 1;
            p
        };
        if prev > 0
            && let Some(cb) = on_progress
        {
            cb(
                1,
                loader_sessions.len(),
                Some(std::slice::from_ref(&loader_sessions[4])),
            );
        }
        let s = loader_sessions.clone();
        Box::pin(async move { Ok(s) })
    });

    let rename_session: RenameSessionFn = Arc::new(|_, _| Box::pin(async { Ok(()) }));

    let selector = SessionSelectorComponent::new(
        current_loader,
        loader_from_vec(Vec::new()),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            rename_session: Some(rename_session),
            show_rename_hint: Some(true),
            keybindings: Some(kb),
        },
        None,
    );
    flush_promises().await;

    let list = selector.get_session_list();
    list.handle_input("\x1b[B");
    list.handle_input("\x1b[B");
    assert_eq!(list.get_selected_session_path(), Some(sessions[2].path.clone()));

    list.handle_input(CTRL_R);
    selector.handle_input("X");
    selector.handle_input("\r");
    flush_promises().await;

    assert_eq!(*loads.lock().unwrap(), 2);
    assert_eq!(
        selector.get_session_list().get_selected_session_path(),
        Some(sessions[2].path.clone())
    );
}

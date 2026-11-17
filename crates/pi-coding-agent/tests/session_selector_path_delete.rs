//! Port of packages/coding-agent/test/session-selector-path-delete.test.ts

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pi_coding_agent::modes::interactive::components::session_selector::{
    SessionListProgress, SessionSelectorComponent, SessionSelectorOptions, SessionsLoader,
};
use pi_coding_agent::modes::interactive::components::session_selector_search::SessionInfo;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result};
use pi_tui::keybindings::KeyIdOrList;
use pi_tui::{
    Component, KeybindingDefinition, KeybindingsConfig, KeybindingsManager, TUI_KEYBINDINGS, set_keybindings,
    strip_terminal_sequences,
};
use serial_test::serial;
use tempfile::TempDir;
use tokio::sync::oneshot;

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

fn make_session(id: &str, path: Option<&str>, parent: Option<&str>, name: Option<&str>, modified: i64) -> SessionInfo {
    SessionInfo {
        path: path.map(str::to_string).unwrap_or_else(|| format!("/tmp/{id}.jsonl")),
        id: id.to_string(),
        cwd: String::new(),
        name: name.map(str::to_string),
        parent_session_path: parent.map(str::to_string),
        created: 0,
        modified,
        message_count: 1,
        first_message: "hello".to_string(),
        all_messages_text: "hello".to_string(),
    }
}

struct SymlinkedSessionPaths {
    _dir: TempDir,
    parent_alias_a: String,
    parent_alias_b: String,
    child_alias_b: String,
}

fn create_symlinked_session_paths() -> SymlinkedSessionPaths {
    let dir = TempDir::new().unwrap();
    let base: PathBuf = dir.path().to_path_buf();
    let real_dir = base.join("real");
    let alias_a_dir = base.join("alias-a");
    let alias_b_dir = base.join("alias-b");
    fs::create_dir_all(&real_dir).unwrap();
    fs::create_dir_all(&alias_a_dir).unwrap();
    fs::create_dir_all(&alias_b_dir).unwrap();

    let shared_dir = real_dir.join("sessions");
    fs::create_dir_all(&shared_dir).unwrap();
    let alias_a_sessions = alias_a_dir.join("sessions");
    let alias_b_sessions = alias_b_dir.join("sessions");

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&shared_dir, &alias_a_sessions).unwrap();
        std::os::unix::fs::symlink(&shared_dir, &alias_b_sessions).unwrap();
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(&shared_dir, &alias_a_sessions).unwrap();
        std::os::windows::fs::symlink_dir(&shared_dir, &alias_b_sessions).unwrap();
    }

    let parent_real = shared_dir.join("parent.jsonl");
    let child_real = shared_dir.join("child.jsonl");
    fs::write(&parent_real, "parent\n").unwrap();
    fs::write(&child_real, "child\n").unwrap();

    SymlinkedSessionPaths {
        _dir: dir,
        parent_alias_a: alias_a_sessions.join("parent.jsonl").to_string_lossy().into_owned(),
        parent_alias_b: alias_b_sessions.join("parent.jsonl").to_string_lossy().into_owned(),
        child_alias_b: alias_b_sessions.join("child.jsonl").to_string_lossy().into_owned(),
    }
}

const CTRL_D: &str = "\x04";
const CTRL_BACKSPACE: &str = "\x1b[127;5u";

fn loader_from_vec(sessions: Vec<SessionInfo>) -> SessionsLoader {
    Arc::new(
        move |_: Option<SessionListProgress>, _: AbortSignal| -> BoxFuture<Result<Vec<SessionInfo>>> {
            let s = sessions.clone();
            Box::pin(async move { Ok(s) })
        },
    )
}

#[tokio::test]
#[serial]
async fn does_not_treat_ctrl_backspace_as_delete_when_search_query_is_non_empty() {
    let (_g, kb) = setup();
    let sessions = vec![
        make_session("a", None, None, None, 0),
        make_session("b", None, None, None, 0),
    ];

    let selector = SessionSelectorComponent::new(
        loader_from_vec(sessions),
        loader_from_vec(Vec::new()),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            keybindings: Some(kb),
            ..Default::default()
        },
        None,
    );
    flush_promises().await;

    let list = selector.get_session_list();
    let confirmations: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let c_clone = Arc::clone(&confirmations);
    list.set_on_delete_confirmation_change(Some(Arc::new(move |p| {
        c_clone.lock().unwrap().push(p.map(str::to_string));
    })));

    list.handle_input("a");
    list.handle_input(CTRL_BACKSPACE);

    assert!(confirmations.lock().unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn enters_confirmation_mode_on_ctrl_d_even_with_non_empty_search_query() {
    let (_g, kb) = setup();
    let sessions = vec![
        make_session("a", None, None, None, 0),
        make_session("b", None, None, None, 0),
    ];

    let selector = SessionSelectorComponent::new(
        loader_from_vec(sessions.clone()),
        loader_from_vec(Vec::new()),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            keybindings: Some(kb),
            ..Default::default()
        },
        None,
    );
    flush_promises().await;

    let list = selector.get_session_list();
    let confirmations: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let c_clone = Arc::clone(&confirmations);
    list.set_on_delete_confirmation_change(Some(Arc::new(move |p| {
        c_clone.lock().unwrap().push(p.map(str::to_string));
    })));

    list.handle_input("a");
    list.handle_input(CTRL_D);

    assert_eq!(*confirmations.lock().unwrap(), vec![Some(sessions[0].path.clone())]);
}

#[tokio::test]
#[serial]
async fn enters_confirmation_mode_on_ctrl_backspace_when_search_query_is_empty() {
    let (_g, kb) = setup();
    let sessions = vec![
        make_session("a", None, None, None, 0),
        make_session("b", None, None, None, 0),
    ];

    let selector = SessionSelectorComponent::new(
        loader_from_vec(sessions.clone()),
        loader_from_vec(Vec::new()),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            keybindings: Some(kb),
            ..Default::default()
        },
        None,
    );
    flush_promises().await;

    let list = selector.get_session_list();
    let confirmations: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let c_clone = Arc::clone(&confirmations);
    list.set_on_delete_confirmation_change(Some(Arc::new(move |p| {
        c_clone.lock().unwrap().push(p.map(str::to_string));
    })));

    let deleted: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let d_clone = Arc::clone(&deleted);
    list.set_on_delete_session(Some(Arc::new(move |p| {
        let owned = p.to_string();
        let d = Arc::clone(&d_clone);
        Box::pin(async move {
            *d.lock().unwrap() = Some(owned);
        })
    })));

    list.handle_input(CTRL_BACKSPACE);
    assert_eq!(*confirmations.lock().unwrap(), vec![Some(sessions[0].path.clone())]);

    list.handle_input("\r");
    flush_promises().await;
    assert_eq!(
        *confirmations.lock().unwrap(),
        vec![Some(sessions[0].path.clone()), None]
    );
    assert_eq!(*deleted.lock().unwrap(), Some(sessions[0].path.clone()));
}

#[tokio::test]
#[serial]
async fn does_not_switch_scope_back_to_all_when_all_load_resolves_after_toggling_back_to_current() {
    let (_g, kb) = setup();
    let current_sessions = vec![make_session("current", None, None, None, 0)];
    let (tx, rx) = oneshot::channel::<Vec<SessionInfo>>();
    let rx_slot = Arc::new(Mutex::new(Some(rx)));
    let all_load_calls = Arc::new(Mutex::new(0usize));
    let calls_clone = Arc::clone(&all_load_calls);

    let all_loader: SessionsLoader = Arc::new(move |_, _| {
        *calls_clone.lock().unwrap() += 1;
        let rx = rx_slot.lock().unwrap().take();
        Box::pin(async move { Ok(rx.unwrap().await.unwrap()) })
    });

    let selector = SessionSelectorComponent::new(
        loader_from_vec(current_sessions),
        all_loader,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            keybindings: Some(kb),
            ..Default::default()
        },
        None,
    );
    flush_promises().await;

    let list = selector.get_session_list();
    list.handle_input("\t");
    list.handle_input("\t");

    let _ = tx.send(vec![make_session("all", None, None, None, 0)]);
    flush_promises().await;

    assert_eq!(*all_load_calls.lock().unwrap(), 1);
    let output = selector.render(120).join("\n");
    assert!(output.contains("Resume Session (Current Folder)"));
    assert!(!output.contains("Resume Session (All)"));
}

#[tokio::test]
#[serial]
async fn does_not_start_redundant_all_loads_when_toggling_scopes_while_all_is_already_loading() {
    let (_g, kb) = setup();
    let current_sessions = vec![make_session("current", None, None, None, 0)];
    let all_sessions = vec![make_session("all", None, None, None, 0)];
    let (tx, rx) = oneshot::channel::<Vec<SessionInfo>>();
    let rx_slot = Arc::new(Mutex::new(Some(rx)));
    let all_load_calls = Arc::new(Mutex::new(0usize));
    let calls_clone = Arc::clone(&all_load_calls);
    let partial = all_sessions.clone();

    let all_loader: SessionsLoader = Arc::new(move |on_progress, _| {
        *calls_clone.lock().unwrap() += 1;
        if let Some(cb) = on_progress {
            cb(1, 2, Some(&partial));
        }
        let rx = rx_slot.lock().unwrap().take();
        Box::pin(async move { Ok(rx.unwrap().await.unwrap()) })
    });

    let selector = SessionSelectorComponent::new(
        loader_from_vec(current_sessions),
        all_loader,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            keybindings: Some(kb),
            ..Default::default()
        },
        None,
    );
    flush_promises().await;

    let list = selector.get_session_list();
    list.handle_input("\t");
    list.handle_input("\t");
    list.handle_input("\t");

    assert_eq!(*all_load_calls.lock().unwrap(), 1);
    assert_eq!(
        selector.get_session_list().get_selected_session_path(),
        Some(all_sessions[0].path.clone())
    );
    assert!(selector.render(120).join("\n").contains("Loading"));

    let _ = tx.send(all_sessions);
    flush_promises().await;
}

#[tokio::test]
#[serial]
async fn threads_sessions_when_parent_and_child_paths_use_different_symlink_aliases() {
    let (_g, kb) = setup();
    let paths = create_symlinked_session_paths();
    let sessions = vec![
        make_session(
            "parent",
            Some(&paths.parent_alias_b),
            None,
            Some("Parent"),
            pi_js::time::parse_date("2026-01-01T00:00:00.000Z").unwrap(),
        ),
        make_session(
            "child",
            Some(&paths.child_alias_b),
            Some(&paths.parent_alias_a),
            Some("Child"),
            pi_js::time::parse_date("2025-12-31T00:00:00.000Z").unwrap(),
        ),
    ];

    let selector = SessionSelectorComponent::new(
        loader_from_vec(sessions),
        loader_from_vec(Vec::new()),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            keybindings: Some(kb),
            ..Default::default()
        },
        None,
    );
    flush_promises().await;

    let output = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(output.contains("Parent"));
    assert!(output.contains("└─ Child"));
}

#[tokio::test]
#[serial]
async fn sorts_threaded_sessions_by_latest_activity_in_their_subtree() {
    let (_g, kb) = setup();
    let parent_one = make_session(
        "parent-one",
        None,
        None,
        Some("Parent one"),
        pi_js::time::parse_date("2026-01-02T00:00:00.000Z").unwrap(),
    );
    let parent_two = make_session(
        "parent-two",
        None,
        None,
        Some("Parent two"),
        pi_js::time::parse_date("2026-01-01T00:00:00.000Z").unwrap(),
    );
    let child_two = make_session(
        "child-two",
        None,
        Some(&parent_two.path),
        Some("Child two"),
        pi_js::time::parse_date("2026-01-03T00:00:00.000Z").unwrap(),
    );

    let selector = SessionSelectorComponent::new(
        loader_from_vec(vec![parent_one, parent_two, child_two]),
        loader_from_vec(Vec::new()),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            keybindings: Some(kb),
            ..Default::default()
        },
        None,
    );
    flush_promises().await;

    let output = strip_terminal_sequences(&selector.render(120).join("\n"));
    let parent_two_idx = output.find("Parent two").unwrap();
    let child_two_idx = output.find("└─ Child two").unwrap();
    let parent_one_idx = output.find("Parent one").unwrap();
    assert!(child_two_idx > parent_two_idx);
    assert!(parent_one_idx > child_two_idx);
}

#[tokio::test]
#[serial]
async fn treats_current_session_as_active_across_symlink_aliases() {
    let (_g, kb) = setup();
    let paths = create_symlinked_session_paths();
    let sessions = vec![make_session(
        "parent",
        Some(&paths.parent_alias_b),
        None,
        Some("Parent"),
        0,
    )];

    let selector = SessionSelectorComponent::new(
        loader_from_vec(sessions),
        loader_from_vec(Vec::new()),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            keybindings: Some(kb),
            ..Default::default()
        },
        Some(&paths.parent_alias_a),
    );
    flush_promises().await;

    let list = selector.get_session_list();
    let confirmations: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let c_clone = Arc::clone(&confirmations);
    let error_msg: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let e_clone = Arc::clone(&error_msg);
    list.set_on_delete_confirmation_change(Some(Arc::new(move |p| {
        c_clone.lock().unwrap().push(p.map(str::to_string));
    })));
    list.set_on_error(Some(Arc::new(move |msg| {
        *e_clone.lock().unwrap() = Some(msg.to_string());
    })));

    list.handle_input(CTRL_D);

    assert!(confirmations.lock().unwrap().is_empty());
    assert_eq!(
        error_msg.lock().unwrap().as_deref(),
        Some("Cannot delete the currently active session")
    );
}

#[tokio::test]
#[serial]
async fn keeps_next_session_selected_when_progressive_reload_after_delete_starts_with_another_session() {
    let (_g, kb) = setup();
    let dir = TempDir::new().unwrap();
    let sessions: Vec<SessionInfo> = (1..=5)
        .map(|i| {
            let p = dir.path().join(format!("s{i}.jsonl"));
            fs::write(&p, "session\n").unwrap();
            make_session(
                &format!("s{i}"),
                Some(&p.to_string_lossy()),
                None,
                None,
                10_000 - i * 1000,
            )
        })
        .collect();

    let loader_sessions = sessions.clone();
    let current_loader: SessionsLoader = Arc::new(move |on_progress, _| {
        let remaining: Vec<SessionInfo> = loader_sessions
            .iter()
            .filter(|s| std::path::Path::new(&s.path).exists())
            .cloned()
            .collect();
        if let Some(cb) = on_progress
            && let Some(last) = remaining.last()
        {
            cb(1, remaining.len(), Some(std::slice::from_ref(last)));
            cb(remaining.len(), remaining.len(), Some(&remaining));
        }
        Box::pin(async move { Ok(remaining) })
    });

    let selector = SessionSelectorComponent::new(
        current_loader,
        loader_from_vec(Vec::new()),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        SessionSelectorOptions {
            keybindings: Some(kb),
            ..Default::default()
        },
        None,
    );
    flush_promises().await;

    let list = selector.get_session_list();
    list.handle_input("\x1b[B");
    list.handle_input("\x1b[A");
    assert_eq!(list.get_selected_session_path(), Some(sessions[0].path.clone()));

    list.handle_input(CTRL_D);
    list.handle_input("\r");
    for _ in 0..20 {
        if !std::path::Path::new(&sessions[0].path).exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    flush_promises().await;

    assert!(!std::path::Path::new(&sessions[0].path).exists());
    assert_eq!(list.get_selected_session_path(), Some(sessions[1].path.clone()));
}

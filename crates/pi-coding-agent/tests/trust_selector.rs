//! Port of packages/coding-agent/test/trust-selector.test.ts

use std::sync::{Arc, Mutex};

use pi_coding_agent::modes::interactive::components::trust_selector::{
    ProjectTrustStoreEntry, ProjectTrustUpdate, TrustSelection, TrustSelectorComponent, TrustSelectorOptions,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::{KeybindingsManager, TUI_KEYBINDINGS, set_keybindings, strip_terminal_sequences};
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn setup() -> TestGuard {
    init_theme(Some("dark"), None);
    set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    TestGuard
}

#[test]
#[serial]
fn keeps_the_saved_trusted_decision_marked_while_browsing() {
    let _g = setup();
    let selector = TrustSelectorComponent::new(TrustSelectorOptions {
        cwd: "/project".to_string(),
        saved_decision: Some(ProjectTrustStoreEntry {
            path: "/project".to_string(),
            decision: true,
        }),
        project_trusted: true,
        on_select: Arc::new(|_| {}),
        on_cancel: Arc::new(|| {}),
    });

    let output = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(output.contains("Saved decision: trusted (/project)"), "output={output}");
    assert!(output.contains("Current session: trusted"), "output={output}");
    assert!(output.contains("→ ✓ Trust"), "output={output}");

    selector.handle_input("\x1b[B");
    let moved = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(moved.contains("✓ Trust"), "moved={moved}");
    assert!(moved.contains("→   Trust parent folder (/)"), "moved={moved}");
    assert!(!moved.contains("✓ Do not trust"), "moved={moved}");
}

#[test]
#[serial]
fn selects_a_trust_decision() {
    let _g = setup();
    let selected: Arc<Mutex<Option<TrustSelection>>> = Arc::new(Mutex::new(None));
    let sel_c = Arc::clone(&selected);

    let selector = TrustSelectorComponent::new(TrustSelectorOptions {
        cwd: "/project".to_string(),
        saved_decision: None,
        project_trusted: false,
        on_select: Arc::new(move |selection| {
            *sel_c.lock().unwrap() = Some(selection);
        }),
        on_cancel: Arc::new(|| {}),
    });

    selector.handle_input("\n");
    assert_eq!(
        *selected.lock().unwrap(),
        Some(TrustSelection {
            trusted: true,
            updates: vec![ProjectTrustUpdate {
                path: "/project".to_string(),
                decision: Some(true),
            }],
        })
    );
}

#[test]
#[serial]
fn labels_saved_ancestor_decisions_as_inherited() {
    let _g = setup();
    let selector = TrustSelectorComponent::new(TrustSelectorOptions {
        cwd: "/parent/project/nested".to_string(),
        saved_decision: Some(ProjectTrustStoreEntry {
            path: "/parent".to_string(),
            decision: true,
        }),
        project_trusted: true,
        on_select: Arc::new(|_| {}),
        on_cancel: Arc::new(|| {}),
    });

    let output = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(
        output.contains("Saved decision: trusted (inherited from /parent)"),
        "output={output}"
    );
}

#[test]
#[serial]
fn adds_a_trust_parent_option() {
    let _g = setup();
    let selected: Arc<Mutex<Option<TrustSelection>>> = Arc::new(Mutex::new(None));
    let sel_c = Arc::clone(&selected);

    let selector = TrustSelectorComponent::new(TrustSelectorOptions {
        cwd: "/parent/project".to_string(),
        saved_decision: Some(ProjectTrustStoreEntry {
            path: "/parent".to_string(),
            decision: true,
        }),
        project_trusted: true,
        on_select: Arc::new(move |selection| {
            *sel_c.lock().unwrap() = Some(selection);
        }),
        on_cancel: Arc::new(|| {}),
    });

    let output = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(
        output.contains("Saved decision: trusted (inherited from /parent)"),
        "output={output}"
    );
    assert!(output.contains("✓ Trust parent folder (/parent)"), "output={output}");

    selector.handle_input("\n");
    assert_eq!(
        *selected.lock().unwrap(),
        Some(TrustSelection {
            trusted: true,
            updates: vec![
                ProjectTrustUpdate {
                    path: "/parent".to_string(),
                    decision: Some(true),
                },
                ProjectTrustUpdate {
                    path: "/parent/project".to_string(),
                    decision: None,
                },
            ],
        })
    );
}

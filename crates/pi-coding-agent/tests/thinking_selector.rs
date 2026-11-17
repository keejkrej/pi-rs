//! Port of packages/coding-agent/test/thinking-selector.test.ts

use std::sync::{Arc, Mutex};

use pi_agent_core::ThinkingLevel;
use pi_coding_agent::core::keybindings::KEYBINDINGS;
use pi_coding_agent::modes::interactive::components::thinking_selector::ThinkingSelectorComponent;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::keybindings::KeyIdOrList;
use pi_tui::{KeybindingsConfig, KeybindingsManager, TUI_KEYBINDINGS, set_keybindings, strip_terminal_sequences};
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn app_keybindings(user_bindings: Option<KeybindingsConfig>) -> KeybindingsManager {
    KeybindingsManager::new(KEYBINDINGS.clone(), user_bindings)
}

fn setup(user_bindings: Option<KeybindingsConfig>) -> TestGuard {
    init_theme(Some("dark"), None);
    set_keybindings(app_keybindings(user_bindings));
    TestGuard
}

#[test]
#[serial]
fn keeps_the_current_thinking_level_marked_while_browsing() {
    let _g = setup(None);
    let selector = ThinkingSelectorComponent::new(
        ThinkingLevel::Medium,
        vec![ThinkingLevel::Medium, ThinkingLevel::High],
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
    );

    let get_level_row = |level: &str| -> Option<String> {
        selector
            .get_select_list()
            .render(80)
            .into_iter()
            .map(|line| strip_terminal_sequences(&line))
            .find(|line| line.contains(level))
    };

    assert_eq!(
        selector.get_select_list().get_selected_item().map(|item| item.label),
        Some("✓ medium".to_string())
    );
    assert!(
        get_level_row("medium")
            .as_deref()
            .is_some_and(|l| l.starts_with("→ ✓ medium"))
    );

    selector.handle_input("\x1b[B");
    assert!(
        get_level_row("medium")
            .as_deref()
            .is_some_and(|l| l.starts_with("  ✓ medium"))
    );
    assert!(
        get_level_row("high")
            .as_deref()
            .is_some_and(|l| l.starts_with("→   high"))
    );
}

#[test]
#[serial]
fn uses_the_configured_save_binding() {
    let mut custom = KeybindingsConfig::new();
    custom.insert(
        "app.thinking.save".to_string(),
        Some(KeyIdOrList::One("ctrl+r".to_string())),
    );
    let _g = setup(Some(custom));

    let saved: Arc<Mutex<Vec<ThinkingLevel>>> = Arc::new(Mutex::new(Vec::new()));
    let saved_cb = Arc::clone(&saved);
    let selector = ThinkingSelectorComponent::new(
        ThinkingLevel::Medium,
        vec![ThinkingLevel::Medium, ThinkingLevel::High],
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Some(Arc::new(move |level| {
            saved_cb.lock().unwrap().push(level);
        })),
        None,
    );

    let output = strip_terminal_sequences(&selector.render(80).join("\n"));
    assert!(output.contains("Ctrl+R to set as default"), "output={output}");

    selector.handle_input("\x13");
    assert!(saved.lock().unwrap().is_empty());

    selector.handle_input("\x12");
    assert_eq!(*saved.lock().unwrap(), vec![ThinkingLevel::Medium]);
}

//! Port of packages/coding-agent/test/extension-selector.test.ts

use std::sync::Arc;

use pi_coding_agent::core::keybindings::{KEYBINDINGS, KeybindingsManager};
use pi_coding_agent::modes::interactive::components::extension_selector::ExtensionSelectorComponent;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_coding_agent::utils::ansi::strip_ansi;
use pi_tui::{KeybindingsManager as TuiKeybindingsManager, TUI_KEYBINDINGS, set_keybindings};
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(TuiKeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn setup() -> TestGuard {
    init_theme(Some("dark"), Some(false));
    let kb = KeybindingsManager::new(None, None);
    set_keybindings(TuiKeybindingsManager::new(
        KEYBINDINGS.clone(),
        Some(kb.get_user_bindings()),
    ));
    TestGuard
}

fn render(selector: &ExtensionSelectorComponent) -> String {
    strip_ansi(&selector.render(80).join("\n"))
}

fn visible_options(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| line.contains("option-"))
        .map(|line| {
            let trimmed = line.trim();
            trimmed.strip_prefix("→ ").unwrap_or(trimmed).to_string()
        })
        .collect()
}

// #6688: long extension selectors must keep the selected option visible.
#[test]
#[serial]
fn windows_long_option_lists_around_selection() {
    let _guard = setup();
    let options: Vec<String> = (1..=15).map(|i| format!("option-{i}")).collect();
    let selector = ExtensionSelectorComponent::new("Choose", options.clone(), Arc::new(|_| {}), Arc::new(|| {}), None);

    let output = render(&selector);
    assert_eq!(visible_options(&output), options[0..10]);
    assert!(output.contains("→ option-1"));
    assert!(output.contains("  (1/15)"));

    for _ in 0..7 {
        selector.handle_input("\x1b[B");
    }
    let output = render(&selector);
    assert_eq!(visible_options(&output), options[2..12]);
    assert!(output.contains("→ option-8"));
    assert!(output.contains("  (8/15)"));

    for _ in 0..7 {
        selector.handle_input("\x1b[B");
    }
    let output = render(&selector);
    assert_eq!(visible_options(&output), options[5..15]);
    assert!(output.contains("→ option-15"));
    assert!(output.contains("  (15/15)"));
}

#[test]
#[serial]
fn renders_short_and_empty_option_lists_without_scroll_indicator() {
    let _guard = setup();
    let short_options = vec!["option-1".to_string(), "option-2".to_string(), "option-3".to_string()];
    let short_selector =
        ExtensionSelectorComponent::new("Choose", short_options.clone(), Arc::new(|_| {}), Arc::new(|| {}), None);
    let short_output = render(&short_selector);
    assert_eq!(visible_options(&short_output), short_options);
    assert!(short_output.contains("→ option-1"));
    assert!(!short_output.contains("(1/3)"));

    let empty_selector = ExtensionSelectorComponent::new("Choose", Vec::new(), Arc::new(|_| {}), Arc::new(|| {}), None);
    let empty_output = render(&empty_selector);
    assert!(empty_output.contains("Choose"));
    assert!(empty_output.contains("↑↓ navigate"));
    assert!(!empty_output.contains('→'));
}

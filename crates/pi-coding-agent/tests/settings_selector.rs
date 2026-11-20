//! Port of packages/coding-agent/test/settings-selector.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::{Model, ModelThinkingLevel};
use pi_coding_agent::modes::interactive::components::settings_selector::{
    FullscreenExitOutput, SettingsCallbacks, SettingsConfig, SettingsSelectorComponent,
};
use pi_coding_agent::modes::interactive::theme::theme::{TerminalTheme, init_theme};
use pi_tui::{
    KeybindingsManager, ScrollViewScrollbar, TUI_KEYBINDINGS, WheelScrollLines, set_keybindings,
    strip_terminal_sequences,
};
use serde_json::json;
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
fn cycles_through_fullscreen_settings() {
    let _g = setup();

    let exit_changes: Arc<Mutex<Vec<FullscreenExitOutput>>> = Arc::new(Mutex::new(Vec::new()));
    let scrollbar_changes: Arc<Mutex<Vec<ScrollViewScrollbar>>> = Arc::new(Mutex::new(Vec::new()));
    let copy_changes: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let wheel_changes: Arc<Mutex<Vec<WheelScrollLines>>> = Arc::new(Mutex::new(Vec::new()));

    let config = SettingsConfig {
        fullscreen_exit_output: FullscreenExitOutput::Transcript,
        fullscreen_scrollbar: ScrollViewScrollbar::Auto,
        fullscreen_copy_on_select: true,
        fullscreen_wheel_scroll_lines: WheelScrollLines::Lines(7.0),
        ..Default::default()
    };

    let callbacks = SettingsCallbacks {
        on_fullscreen_exit_output_change: Some(Arc::new({
            let log = Arc::clone(&exit_changes);
            move |v| log.lock().unwrap().push(v)
        })),
        on_fullscreen_scrollbar_change: Some(Arc::new({
            let log = Arc::clone(&scrollbar_changes);
            move |v| log.lock().unwrap().push(v)
        })),
        on_fullscreen_copy_on_select_change: Some(Arc::new({
            let log = Arc::clone(&copy_changes);
            move |v| log.lock().unwrap().push(v)
        })),
        on_fullscreen_wheel_scroll_lines_change: Some(Arc::new({
            let log = Arc::clone(&wheel_changes);
            move |v| log.lock().unwrap().push(v)
        })),
        ..Default::default()
    };

    let cycle = |label: &str, count: usize| {
        let comp = SettingsSelectorComponent::new(config.clone(), callbacks.clone());
        let list = comp.get_settings_list();
        for ch in label.chars() {
            list.handle_input(&ch.to_string());
        }
        for _ in 0..count {
            list.handle_input("\r");
        }
    };

    cycle("Fullscreen exit output", 2);
    assert_eq!(
        *exit_changes.lock().unwrap(),
        vec![FullscreenExitOutput::ResumeHint, FullscreenExitOutput::Transcript]
    );

    cycle("Fullscreen scrollbar", 3);
    assert_eq!(
        *scrollbar_changes.lock().unwrap(),
        vec![
            ScrollViewScrollbar::Always,
            ScrollViewScrollbar::Hidden,
            ScrollViewScrollbar::Auto,
        ]
    );

    cycle("Fullscreen copy on select", 2);
    assert_eq!(*copy_changes.lock().unwrap(), vec![false, true]);

    cycle("Fullscreen wheel scrolling", 3);
    assert_eq!(
        *wheel_changes.lock().unwrap(),
        vec![
            WheelScrollLines::Lines(10.0),
            WheelScrollLines::AUTO,
            WheelScrollLines::Lines(1.0),
        ]
    );
}

#[test]
#[serial]
fn keeps_the_configured_fixed_theme_marked_while_browsing() {
    let _g = setup();
    let config = SettingsConfig {
        current_theme: "dark".to_string(),
        terminal_theme: TerminalTheme::Dark,
        available_themes: vec!["system".to_string(), "dark".to_string(), "light".to_string()],
        ..Default::default()
    };
    let callbacks = SettingsCallbacks {
        on_theme_preview: Some(Arc::new(|_| {})),
        on_cancel: Some(Arc::new(|| {})),
        ..Default::default()
    };
    let comp = SettingsSelectorComponent::new(config, callbacks);
    let list = comp.get_settings_list();

    list.select_item("theme");
    list.handle_input("\r");

    let output = strip_terminal_sequences(&list.render(120).join("\n"));
    let has_system = output
        .lines()
        .any(|l| l.contains("    system") && l.contains("Theme created from your terminal's colors"));
    let has_auto = output
        .lines()
        .any(|l| l.contains("    automatic") && l.contains("Use separate themes"));
    assert!(has_system && has_auto, "output={output}");
    assert!(output.contains("→ ✓ dark"), "output={output}");

    list.handle_input("\x1b[B");
    let moved = strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(moved.contains("  ✓ dark"), "moved={moved}");
    assert!(moved.contains("→   light"), "moved={moved}");
}

#[test]
#[serial]
fn keeps_a_configured_automatic_theme_marked_while_browsing() {
    let _g = setup();
    let config = SettingsConfig {
        current_theme: "light/dark".to_string(),
        terminal_theme: TerminalTheme::Dark,
        available_themes: vec!["dark".to_string(), "light".to_string(), "other".to_string()],
        ..Default::default()
    };
    let callbacks = SettingsCallbacks {
        on_theme_preview: Some(Arc::new(|_| {})),
        on_cancel: Some(Arc::new(|| {})),
        ..Default::default()
    };
    let comp = SettingsSelectorComponent::new(config, callbacks);
    let list = comp.get_settings_list();

    list.select_item("theme");
    list.handle_input("\r");
    list.handle_input("\r");

    let output = strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(output.contains("→ ✓ light"), "output={output}");

    list.handle_input("\x1b[B");
    let moved = strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(moved.contains("  ✓ light"), "moved={moved}");
    assert!(moved.contains("→   other"), "moved={moved}");
}

#[test]
#[serial]
fn keeps_the_configured_per_model_thinking_level_marked_while_browsing() {
    let _g = setup();
    let model: Model = serde_json::from_value(json!({
        "id": "thinking-model",
        "name": "thinking-model",
        "api": "openai-responses",
        "provider": "openai",
        "baseUrl": "https://api.openai.com/v1",
        "reasoning": true,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 200_000,
        "maxTokens": 16_384,
    }))
    .expect("test model parses");

    let model_key = format!("{}/{}", model.provider, model.id);
    let mut overrides = IndexMap::new();
    overrides.insert(model_key.clone(), ModelThinkingLevel::Medium);

    let config = SettingsConfig {
        default_model: model_key,
        available_default_models: vec![model],
        thinking_level: ModelThinkingLevel::High,
        model_thinking_levels: overrides,
        ..Default::default()
    };
    let callbacks = SettingsCallbacks {
        on_cancel: Some(Arc::new(|| {})),
        ..Default::default()
    };
    let comp = SettingsSelectorComponent::new(config, callbacks);
    let list = comp.get_settings_list();

    list.select_item("model-thinking");
    list.handle_input("\r");
    list.handle_input("\r");

    let output = strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(output.contains("→ ✓ medium"), "output={output}");
    assert!(output.contains("    (clear override)"), "output={output}");

    list.handle_input("\x1b[B");
    let moved = strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(moved.contains("  ✓ medium"), "moved={moved}");
    assert!(moved.contains("→   high"), "moved={moved}");
}

#[test]
#[serial]
fn cycles_through_share_destinations() {
    use pi_coding_agent::modes::interactive::session_share::SessionShareDestination;

    let _g = setup();
    let changes = Arc::new(Mutex::new(Vec::new()));
    let changes_c = Arc::clone(&changes);
    let config = SettingsConfig {
        session_share_destination: SessionShareDestination::Auto,
        ..Default::default()
    };
    let callbacks = SettingsCallbacks {
        on_session_share_destination_change: Some(Arc::new(move |dest| {
            changes_c.lock().unwrap().push(dest);
        })),
        ..Default::default()
    };
    let selector = SettingsSelectorComponent::new(config, callbacks);
    let list = selector.get_settings_list();
    list.select_item("share-destination");
    for _ in 0..3 {
        list.handle_input("\r");
    }
    assert_eq!(
        *changes.lock().unwrap(),
        vec![
            SessionShareDestination::Gist,
            SessionShareDestination::Radius,
            SessionShareDestination::Auto,
        ]
    );
}

#[test]
#[serial]
fn pages_a_searchable_submenu_with_selection_page_keys_7629() {
    use pi_coding_agent::modes::interactive::components::settings_submenu::{SelectSubmenu, SelectSubmenuOptions};
    use pi_tui::SelectItem;

    let _g = setup();
    let selected = Arc::new(Mutex::new(None::<String>));
    let selected_c = Arc::clone(&selected);
    let items: Vec<SelectItem> = (0..20)
        .map(|index| SelectItem {
            value: format!("theme-{index}"),
            label: format!("theme-{index}"),
            description: None,
        })
        .collect();
    let submenu = SelectSubmenu::new(
        "Theme",
        "",
        items,
        "theme-0",
        Arc::new(move |val| {
            *selected_c.lock().unwrap() = Some(val.to_owned());
        }),
        Arc::new(|| {}),
        None,
        Some(SelectSubmenuOptions {
            searchable: true,
            layout: None,
        }),
    );

    submenu.handle_input("\x1b[6~");
    submenu.handle_input("\r");
    assert_eq!(selected.lock().unwrap().as_deref(), Some("theme-10"));
}

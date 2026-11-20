//! Port of packages/coding-agent/test/config-selector-keybindings.test.ts

use std::sync::Arc;

use pi_coding_agent::core::keybindings::KeybindingsManager;
use pi_coding_agent::core::package_manager::ResolvedPaths;
use pi_coding_agent::core::settings_manager::{InMemorySettingsStorage, SettingsManager};
use pi_coding_agent::modes::interactive::components::config_selector::{
    ConfigSelectorComponent, ConfigWriteScope, ScopedResolvedPaths,
};
use pi_coding_agent::modes::interactive::interactive_mode::sync_tui_keybindings;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::{
    Component, KeybindingsManager as TuiKeybindingsManager, TUI_KEYBINDINGS, get_keybindings, set_keybindings,
};
use serde_json::json;
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(TuiKeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

#[test]
#[serial]
fn config_selector_uses_user_keybindings_json() {
    let _guard = TestGuard;
    init_theme(Some("dark"), Some(false));
    let temp = tempfile::tempdir().expect("tempdir");
    let agent_dir = temp.path().to_str().expect("utf8");
    std::fs::write(
        temp.path().join("keybindings.json"),
        serde_json::to_string(&json!({
            "tui.select.up": ["up", "ctrl+p"],
            "tui.select.down": ["down", "ctrl+n"]
        }))
        .expect("json"),
    )
    .expect("write keybindings.json");

    let kb = KeybindingsManager::create(Some(agent_dir));
    sync_tui_keybindings(&kb);

    let tui_kb = get_keybindings();
    assert!(tui_kb.get_keys("tui.select.down").contains(&"ctrl+n".to_string()));
    assert!(tui_kb.get_keys("tui.select.up").contains(&"ctrl+p".to_string()));

    let settings = SettingsManager::from_storage(Arc::new(InMemorySettingsStorage::default()), None);
    let resolved = ScopedResolvedPaths {
        global: ResolvedPaths::default(),
        project: ResolvedPaths::default(),
    };
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let closed_c = Arc::clone(&closed);
    let selector = ConfigSelectorComponent::new(
        &resolved,
        Arc::new(settings),
        agent_dir,
        agent_dir,
        Arc::new(move || {
            closed_c.store(true, std::sync::atomic::Ordering::SeqCst);
        }),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Some(24),
        ConfigWriteScope::Global,
        false,
    );
    selector.handle_input("\x0e"); // ctrl+n
    selector.handle_input("\x10"); // ctrl+p
    selector.handle_input("\x1b");
    assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
}

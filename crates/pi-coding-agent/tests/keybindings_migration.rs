//! Port of packages/coding-agent/test/keybindings-migration.test.ts (unit coding-agent-keybindings-footer-http)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "keybindings_migration/part_2.rs"]
mod part_2;
// @generated-mods end

use pi_coding_agent::config::ENV_AGENT_DIR;
use pi_coding_agent::core::keybindings::{KeyIdOrList, KeybindingsConfig, KeybindingsManager};
use pi_coding_agent::migrations::run_migrations;
use pi_js::env::testing::EnvGuard;
use serde_json::{Value, json};
use tempfile::TempDir;

fn create_agent_dir(config: &Value) -> (TempDir, String) {
    let temp = TempDir::new().expect("temp dir");
    let agent_dir = temp.path().to_str().expect("utf8").to_owned();
    let content = format!("{}\n", pi_js::json::stringify_pretty(config, "  "));
    std::fs::write(temp.path().join("keybindings.json"), content).expect("write keybindings.json");
    (temp, agent_dir)
}

#[test]
fn rewrites_old_key_names_to_namespaced_ids() {
    let (_dir, agent_dir) = create_agent_dir(&json!({
        "cursorUp": ["up", "ctrl+p"],
        "expandTools": "ctrl+x"
    }));
    {
        let _guard = EnvGuard::set(&ENV_AGENT_DIR, Some(&agent_dir));
        let _ = run_migrations(&agent_dir);
    }

    let raw = std::fs::read_to_string(format!("{agent_dir}/keybindings.json")).expect("read keybindings.json");
    let migrated: Value = pi_js::json::parse(&raw).expect("parse migrated keybindings");
    pretty_assertions::assert_eq!(
        migrated,
        json!({
            "tui.editor.cursorUp": ["up", "ctrl+p"],
            "app.tools.expand": "ctrl+x"
        })
    );
}

#[test]
fn keeps_the_namespaced_value_when_old_and_new_names_both_exist() {
    let (_dir, agent_dir) = create_agent_dir(&json!({
        "expandTools": "ctrl+x",
        "app.tools.expand": "ctrl+y"
    }));
    {
        let _guard = EnvGuard::set(&ENV_AGENT_DIR, Some(&agent_dir));
        let _ = run_migrations(&agent_dir);
    }

    let raw = std::fs::read_to_string(format!("{agent_dir}/keybindings.json")).expect("read keybindings.json");
    let migrated: Value = pi_js::json::parse(&raw).expect("parse migrated keybindings");
    pretty_assertions::assert_eq!(
        migrated,
        json!({
            "app.tools.expand": "ctrl+y"
        })
    );
}

#[test]
fn loads_old_key_names_in_memory_before_the_file_is_rewritten() {
    let (_dir, agent_dir) = create_agent_dir(&json!({
        "selectConfirm": "enter",
        "interrupt": "ctrl+x"
    }));

    let keybindings = KeybindingsManager::create(Some(&agent_dir));
    let expected_user = KeybindingsConfig::from([
        ("app.interrupt".to_owned(), Some(KeyIdOrList::One("ctrl+x".to_owned()))),
        (
            "tui.select.confirm".to_owned(),
            Some(KeyIdOrList::One("enter".to_owned())),
        ),
    ]);
    pretty_assertions::assert_eq!(keybindings.get_user_bindings(), expected_user);

    let effective = keybindings.get_effective_config();
    assert_eq!(
        effective.get("tui.select.confirm"),
        Some(&Some(KeyIdOrList::One("enter".to_owned())))
    );
    assert_eq!(
        effective.get("app.interrupt"),
        Some(&Some(KeyIdOrList::One("ctrl+x".to_owned())))
    );
}

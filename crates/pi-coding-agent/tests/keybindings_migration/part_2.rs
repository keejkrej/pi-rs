//! Port of packages/coding-agent/test/keybindings-migration.test.ts (unit coding-agent-cli-args-auth-migrations)

use pi_coding_agent::config::ENV_AGENT_DIR;
use pi_coding_agent::migrations::run_migrations;
use pretty_assertions::assert_eq;
use serde_json::json;
use serial_test::serial;

#[test]
#[serial]
fn rewrites_old_key_names_to_namespaced_ids() {
    let agent_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-keybindings-test-"])).unwrap();
    let kb_path = pi_js::path::join(&[&agent_dir, "keybindings.json"]);
    let content = pi_js::json::stringify_pretty(
        &json!({
            "cursorUp": ["up", "ctrl+p"],
            "expandTools": "ctrl+x",
        }),
        2,
    );
    pi_js::fs::write_file(&kb_path, format!("{content}\n").as_bytes(), None).unwrap();

    let _guard = pi_js::env::testing::set(ENV_AGENT_DIR, Some(&agent_dir));
    run_migrations(&agent_dir);
    drop(_guard);

    let raw = pi_js::fs::read_to_string(&kb_path).unwrap();
    let migrated = pi_js::json::parse(&raw).unwrap();
    let _ = pi_js::fs::rm(&agent_dir, true, true);

    assert_eq!(
        migrated,
        json!({
            "tui.editor.cursorUp": ["up", "ctrl+p"],
            "app.tools.expand": "ctrl+x",
        })
    );
}

#[test]
#[serial]
fn keeps_the_namespaced_value_when_old_and_new_names_both_exist() {
    let agent_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-keybindings-test-"])).unwrap();
    let kb_path = pi_js::path::join(&[&agent_dir, "keybindings.json"]);
    let content = pi_js::json::stringify_pretty(
        &json!({
            "expandTools": "ctrl+x",
            "app.tools.expand": "ctrl+y",
        }),
        2,
    );
    pi_js::fs::write_file(&kb_path, format!("{content}\n").as_bytes(), None).unwrap();

    let _guard = pi_js::env::testing::set(ENV_AGENT_DIR, Some(&agent_dir));
    run_migrations(&agent_dir);
    drop(_guard);

    let raw = pi_js::fs::read_to_string(&kb_path).unwrap();
    let migrated = pi_js::json::parse(&raw).unwrap();
    let _ = pi_js::fs::rm(&agent_dir, true, true);

    assert_eq!(
        migrated,
        json!({
            "app.tools.expand": "ctrl+y",
        })
    );
}

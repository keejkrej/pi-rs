//! Port of packages/coding-agent/test/settings-manager-bug.test.ts

use pi_ai::ModelThinkingLevel;
use pi_coding_agent::core::settings_manager::SettingsManager;
use serde_json::{Value, json};
use tempfile::TempDir;

fn setup_dirs() -> (TempDir, String, String) {
    let temp = TempDir::new().expect("temp dir");
    let agent_dir = temp.path().join("agent");
    let project_dir = temp.path().join("project");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent_dir");
    std::fs::create_dir_all(project_dir.join(".pi")).expect("mkdir .pi");
    (
        temp,
        agent_dir.to_str().expect("utf8").to_owned(),
        project_dir.to_str().expect("utf8").to_owned(),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn should_preserve_file_changes_to_packages_array_when_changing_unrelated_setting() {
    let (_temp, agent_dir, project_dir) = setup_dirs();
    let settings_path = format!("{agent_dir}/settings.json");

    std::fs::write(
        &settings_path,
        pi_js::json::stringify(&json!({
            "theme": "dark",
            "packages": ["npm:pi-mcp-adapter"]
        })),
    )
    .expect("write initial settings");

    let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
    assert_eq!(
        manager.get_packages(),
        vec![pi_coding_agent::core::settings_manager::PackageSource::String(
            "npm:pi-mcp-adapter".to_owned()
        )]
    );

    let mut current: Value =
        pi_js::json::parse(&std::fs::read_to_string(&settings_path).expect("read settings")).expect("parse");
    current["packages"] = json!([]);
    std::fs::write(&settings_path, pi_js::json::stringify_pretty(&current, "  ")).expect("write edited settings");

    let edited: Value =
        pi_js::json::parse(&std::fs::read_to_string(&settings_path).expect("read edited")).expect("parse");
    assert_eq!(edited["packages"], json!([]));

    manager.set_theme("light");
    manager.flush().await;

    let saved: Value =
        pi_js::json::parse(&std::fs::read_to_string(&settings_path).expect("read saved")).expect("parse");
    assert_eq!(saved["packages"], json!([]));
    assert_eq!(saved["theme"], json!("light"));
}

#[tokio::test(flavor = "current_thread")]
async fn should_preserve_file_changes_to_extensions_array_when_changing_unrelated_setting() {
    let (_temp, agent_dir, project_dir) = setup_dirs();
    let settings_path = format!("{agent_dir}/settings.json");

    std::fs::write(
        &settings_path,
        pi_js::json::stringify(&json!({
            "theme": "dark",
            "extensions": ["/old/extension.ts"]
        })),
    )
    .expect("write initial settings");

    let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);

    let mut current: Value =
        pi_js::json::parse(&std::fs::read_to_string(&settings_path).expect("read settings")).expect("parse");
    current["extensions"] = json!(["/new/extension.ts"]);
    std::fs::write(&settings_path, pi_js::json::stringify_pretty(&current, "  ")).expect("write edited settings");

    manager.set_default_thinking_level(ModelThinkingLevel::High);
    manager.flush().await;

    let saved: Value =
        pi_js::json::parse(&std::fs::read_to_string(&settings_path).expect("read saved")).expect("parse");
    assert_eq!(saved["extensions"], json!(["/new/extension.ts"]));
}

#[tokio::test(flavor = "current_thread")]
async fn should_preserve_external_project_settings_changes_when_updating_unrelated_project_field() {
    let (_temp, agent_dir, project_dir) = setup_dirs();
    let project_settings_path = format!("{project_dir}/.pi/settings.json");

    std::fs::write(
        &project_settings_path,
        pi_js::json::stringify(&json!({
            "extensions": ["./old-extension.ts"],
            "prompts": ["./old-prompt.md"]
        })),
    )
    .expect("write project settings");

    let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);

    let mut current: Value =
        pi_js::json::parse(&std::fs::read_to_string(&project_settings_path).expect("read project settings"))
            .expect("parse");
    current["prompts"] = json!(["./new-prompt.md"]);
    std::fs::write(&project_settings_path, pi_js::json::stringify_pretty(&current, "  ")).expect("write edited");

    manager
        .set_project_extension_paths(vec!["./updated-extension.ts".to_owned()])
        .expect("set project extension paths");
    manager.flush().await;

    let saved: Value =
        pi_js::json::parse(&std::fs::read_to_string(&project_settings_path).expect("read saved")).expect("parse");
    assert_eq!(saved["prompts"], json!(["./new-prompt.md"]));
    assert_eq!(saved["extensions"], json!(["./updated-extension.ts"]));
}

#[tokio::test(flavor = "current_thread")]
async fn should_let_in_memory_project_changes_override_external_changes_for_the_same_project_field() {
    let (_temp, agent_dir, project_dir) = setup_dirs();
    let project_settings_path = format!("{project_dir}/.pi/settings.json");

    std::fs::write(
        &project_settings_path,
        pi_js::json::stringify(&json!({
            "extensions": ["./initial-extension.ts"]
        })),
    )
    .expect("write initial project settings");

    let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);

    let mut current: Value =
        pi_js::json::parse(&std::fs::read_to_string(&project_settings_path).expect("read project settings"))
            .expect("parse");
    current["extensions"] = json!(["./external-extension.ts"]);
    std::fs::write(&project_settings_path, pi_js::json::stringify_pretty(&current, "  ")).expect("write edited");

    manager
        .set_project_extension_paths(vec!["./in-memory-extension.ts".to_owned()])
        .expect("set project extension paths");
    manager.flush().await;

    let saved: Value =
        pi_js::json::parse(&std::fs::read_to_string(&project_settings_path).expect("read saved")).expect("parse");
    assert_eq!(saved["extensions"], json!(["./in-memory-extension.ts"]));
}

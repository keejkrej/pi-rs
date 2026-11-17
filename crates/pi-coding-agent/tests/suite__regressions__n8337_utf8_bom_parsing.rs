//! Port of packages/coding-agent/test/suite/regressions/8337-utf8-bom-parsing.test.ts (unit coding-agent-settings-manager)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "suite__regressions__n8337_utf8_bom_parsing/part_2.rs"]
mod part_2;
// @generated-mods end

use indexmap::IndexMap;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_coding_agent::utils::frontmatter::parse_frontmatter;
use pi_coding_agent::utils::text::split_bom;
use serde_json::{Value, json};
use tempfile::TempDir;

#[tokio::test(flavor = "current_thread")]
async fn loads_frontmatter_and_settings_with_a_leading_bom() {
    assert_eq!(split_bom("\u{FEFF}content"), ("\u{FEFF}", "content"));

    let document = "---\nname: demo\ndescription: Test\n---\nBody";
    let parsed = parse_frontmatter(&format!("\u{FEFF}{document}"));
    pretty_assertions::assert_eq!(
        parsed.frontmatter,
        IndexMap::from([
            ("name".to_owned(), json!("demo")),
            ("description".to_owned(), json!("Test")),
        ])
    );
    assert_eq!(parsed.body, "Body");

    let temp = TempDir::new().expect("temp dir");
    let agent_dir = temp.path().join("agent");
    let project_dir = temp.path().join("project");
    std::fs::create_dir_all(project_dir.join(".pi")).expect("mkdir .pi");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent_dir");

    let global_settings_path = agent_dir.join("settings.json");
    std::fs::write(
        &global_settings_path,
        format!(
            "\u{FEFF}{}",
            pi_js::json::stringify(&json!({ "defaultModel": "global-model" }))
        ),
    )
    .expect("write global settings");
    std::fs::write(
        project_dir.join(".pi").join("settings.json"),
        format!(
            "\u{FEFF}{}",
            pi_js::json::stringify(&json!({ "defaultProvider": "project-provider" }))
        ),
    )
    .expect("write project settings");

    let settings = SettingsManager::create(
        project_dir.to_str().expect("utf8"),
        Some(agent_dir.to_str().expect("utf8")),
        None,
    );
    assert_eq!(settings.get_default_model().as_deref(), Some("global-model"));
    assert_eq!(settings.get_default_provider().as_deref(), Some("project-provider"));

    settings.set_theme("dark");
    settings.flush().await;
    let saved_raw = std::fs::read_to_string(&global_settings_path).expect("read saved settings");
    assert!(!saved_raw.starts_with('\u{FEFF}'));
    let saved: Value = pi_js::json::parse(&saved_raw).expect("parse saved");
    assert_eq!(saved["theme"], json!("dark"));
}

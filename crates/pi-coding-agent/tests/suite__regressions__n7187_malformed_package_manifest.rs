//! Port of packages/coding-agent/test/suite/regressions/7187-malformed-package-manifest.test.ts

mod support;

use pi_coding_agent::core::pi_manifest::{PiManifest, read_pi_manifest};
use serde_json::json;
use tempfile::TempDir;

use support::test_harness::{HarnessOptions, create_harness};

#[tokio::test(flavor = "current_thread")]
async fn ignores_invalid_resource_fields_without_dropping_valid_fields() {
    let harness = create_harness(Some(HarnessOptions::default())).await.expect("harness");
    let temp = TempDir::new().expect("temp dir");
    let agent_dir = temp.path().join("agent");
    let package_dir = agent_dir.join("npm").join("node_modules").join("bad-package");
    let skill_path = package_dir.join("skills").join("bad").join("SKILL.md");
    let prompt_path = package_dir.join("prompts").join("valid.md");

    std::fs::create_dir_all(skill_path.parent().unwrap()).expect("mkdir skills/bad");
    std::fs::create_dir_all(prompt_path.parent().unwrap()).expect("mkdir prompts");
    std::fs::write(&skill_path, "---\nname: bad\ndescription: Must not load\n---\n").expect("write skill");
    std::fs::write(&prompt_path, "Valid prompt\n").expect("write prompt");
    let manifest_path = package_dir.join("package.json");
    std::fs::write(
        &manifest_path,
        pi_js::json::stringify(&json!({
            "name": "bad-package",
            "version": "1.0.0",
            "pi": {
                "skills": "./skills",
                "prompts": ["./prompts"]
            }
        })),
    )
    .expect("write package.json");

    let manifest = read_pi_manifest(manifest_path.to_str().expect("utf8")).expect("read manifest");
    pretty_assertions::assert_eq!(
        manifest,
        PiManifest {
            extensions: None,
            skills: None,
            prompts: Some(vec!["./prompts".to_owned()]),
            themes: None,
        }
    );

    // WIRE(@mcp-helper2): once `pi_coding_agent::core::package_manager::DefaultPackageManager` lands,
    // also assert `DefaultPackageManager::resolve()` excludes `skill_path` and includes `prompt_path`.
    harness.cleanup();
}

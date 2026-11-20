//! Port of packages/coding-agent/test/suite/regressions/7187-malformed-package-manifest.test.ts

mod support;

use pi_coding_agent::core::package_manager::{DefaultPackageManager, PackageManagerOptions};
use pi_coding_agent::core::pi_manifest::{PiManifest, read_pi_manifest};
use pi_coding_agent::core::settings_manager::{PackageSource, SettingsManager};
use serde_json::json;
use tempfile::TempDir;

use support::test_harness::{HarnessOptions, create_harness};

#[tokio::test(flavor = "current_thread")]
async fn ignores_invalid_resource_fields_without_dropping_valid_fields() {
    let harness = create_harness(Some(HarnessOptions::default())).await.expect("harness");
    let temp = TempDir::new().expect("temp dir");
    let agent_dir = temp.path().join("agent");
    let project_dir = temp.path().join("project");
    let package_dir = agent_dir.join("npm").join("node_modules").join("bad-package");
    let skill_path = package_dir.join("skills").join("bad").join("SKILL.md");
    let prompt_path = package_dir.join("prompts").join("valid.md");

    std::fs::create_dir_all(&project_dir).expect("mkdir project");
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
            changelog_path: None,
        }
    );

    let settings_manager = SettingsManager::in_memory(None, None);
    settings_manager.set_packages(vec![PackageSource::String("npm:bad-package".to_owned())]);
    let package_manager = DefaultPackageManager::new(PackageManagerOptions {
        cwd: project_dir.to_string_lossy().into_owned(),
        agent_dir: agent_dir.to_string_lossy().into_owned(),
        settings_manager,
        builtin_extensions: Vec::new(),
    });
    let resolved = package_manager.resolve(None).await.expect("resolve package manager");
    let expected_skill = std::fs::canonicalize(&skill_path)
        .unwrap_or_else(|_| skill_path.clone())
        .to_string_lossy()
        .into_owned();
    let expected_prompt = std::fs::canonicalize(&prompt_path)
        .unwrap_or_else(|_| prompt_path.clone())
        .to_string_lossy()
        .into_owned();
    let norm = |p: &str| {
        std::fs::canonicalize(p)
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.to_owned())
    };
    assert!(!resolved.skills.iter().any(|r| norm(&r.path) == expected_skill));
    assert!(resolved.prompts.iter().any(|r| norm(&r.path) == expected_prompt));

    harness.cleanup();
}

//! Port of packages/coding-agent/test/sdk-skills.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::skills::Skill;
use pi_coding_agent::core::source_info::{SyntheticSourceOptions, create_synthetic_source_info};
use pretty_assertions::assert_eq;
use support::suite::harness::create_suite_resource_loader;

fn setup_skill_dir() -> String {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-sdk-test-"])).unwrap();
    let skills_dir = pi_js::path::join(&[&temp_dir, "skills", "test-skill"]);
    pi_js::fs::mkdir(&skills_dir, true, None).unwrap();
    pi_js::fs::write_file(
        &pi_js::path::join(&[&skills_dir, "SKILL.md"]),
        b"---\nname: test-skill\ndescription: A test skill for SDK tests.\n---\n\n# Test Skill\n\nThis is a test skill.\n",
        None,
    )
    .unwrap();
    temp_dir
}

#[tokio::test]
async fn should_discover_skills_by_default_and_expose_them_on_session() {
    let temp_dir = setup_skill_dir();
    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(temp_dir.clone()),
        agent_dir: Some(temp_dir.clone()),
        session_manager: Some(SessionManager::in_memory(Some(&temp_dir), None, None).unwrap()),
        ..Default::default()
    }))
    .await
    .unwrap();

    let (skills, _diagnostics) = created.session.resource_loader().get_skills();
    assert!(!skills.is_empty());
    assert!(skills.iter().any(|s| s.name == "test-skill"));

    created.session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_have_empty_skills_when_resource_loader_returns_none() {
    let temp_dir = setup_skill_dir();
    let resource_loader: Arc<dyn ResourceLoader> = create_suite_resource_loader(None);

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(temp_dir.clone()),
        agent_dir: Some(temp_dir.clone()),
        session_manager: Some(SessionManager::in_memory(Some(&temp_dir), None, None).unwrap()),
        resource_loader: Some(resource_loader),
        ..Default::default()
    }))
    .await
    .unwrap();

    let (skills, diagnostics) = created.session.resource_loader().get_skills();
    assert!(skills.is_empty());
    assert!(diagnostics.is_empty());

    created.session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_use_provided_skills_when_resource_loader_supplies_them() {
    let temp_dir = setup_skill_dir();
    let custom_skill = Skill {
        name: "custom-skill".to_string(),
        description: "A custom skill".to_string(),
        file_path: "/fake/path/SKILL.md".to_string(),
        base_dir: "/fake/path".to_string(),
        source_info: create_synthetic_source_info(
            "/fake/path/SKILL.md",
            SyntheticSourceOptions {
                source: "sdk".to_string(),
                ..Default::default()
            },
        ),
        disable_model_invocation: false,
    };

    let expected = custom_skill.clone();
    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: temp_dir.clone(),
        agent_dir: temp_dir.clone(),
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_themes: true,
        skills_override: Some(Arc::new(move |_| {
            pi_coding_agent::core::resource_loader::SkillsResult {
                skills: vec![custom_skill.clone()],
                diagnostics: Vec::new(),
            }
        })),
        ..Default::default()
    }));
    resource_loader.reload(None).await.unwrap();

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(temp_dir.clone()),
        agent_dir: Some(temp_dir.clone()),
        session_manager: Some(SessionManager::in_memory(Some(&temp_dir), None, None).unwrap()),
        resource_loader: Some(resource_loader),
        ..Default::default()
    }))
    .await
    .unwrap();

    let (skills, diagnostics) = created.session.resource_loader().get_skills();
    assert_eq!(skills, vec![expected]);
    assert!(diagnostics.is_empty());

    created.session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

//! Port of `packages/coding-agent/test/suite/regressions/2781-skill-collision-precedence.test.ts`.
// WIRE(@mcp-helper2): full `DefaultResourceLoader` package-vs-user-vs-project precedence awaits `core::resource_loader`.

use std::path::Path;

use indexmap::IndexMap;
use pi_coding_agent::core::diagnostics::{ResourceCollision, ResourceDiagnostic, ResourceDiagnosticType, ResourceType};
use pi_coding_agent::core::skills::{LoadSkillsOptions, Skill, load_skills};
use tempfile::TempDir;

fn create_skill_md(dir: &Path, name: &str, description: &str, body: &str) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let skill_path = dir.join("SKILL.md");
    std::fs::write(
        &skill_path,
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .unwrap();
    skill_path.to_string_lossy().into_owned()
}

fn merge_with_collision_precedence(
    user_and_project: &[Skill],
    package_skills: &[Skill],
) -> (Vec<Skill>, Vec<ResourceDiagnostic>) {
    let mut by_name: IndexMap<String, Skill> = IndexMap::new();
    let mut diagnostics = Vec::new();
    for s in user_and_project.iter().chain(package_skills.iter()) {
        if let Some(winner) = by_name.get(&s.name) {
            diagnostics.push(ResourceDiagnostic {
                r#type: ResourceDiagnosticType::Collision,
                message: format!("name \"{}\" collision", s.name),
                path: Some(s.file_path.clone()),
                collision: Some(ResourceCollision {
                    resource_type: ResourceType::Skill,
                    name: s.name.clone(),
                    winner_path: winner.file_path.clone(),
                    loser_path: s.file_path.clone(),
                    winner_source: Some(winner.source_info.source.clone()),
                    loser_source: Some(s.source_info.source.clone()),
                }),
            });
        } else {
            by_name.insert(s.name.clone(), s.clone());
        }
    }
    (by_name.into_values().collect(), diagnostics)
}

#[test]
fn user_and_project_skills_override_package_skills_and_report_collision() {
    let tmp = TempDir::new().unwrap();
    let agent_dir = tmp.path().join("agent");
    let cwd = tmp.path().join("project");
    let pkg_dir = tmp.path().join("fake-package-web-fetch");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let pkg_skill_path = create_skill_md(
        &pkg_dir.join("skills/web-fetch"),
        "web-fetch",
        "Package web-fetch skill",
        "Package skill content",
    );
    let user_skill_path = create_skill_md(
        &agent_dir.join("skills/web-fetch"),
        "web-fetch",
        "User web-fetch override",
        "User skill content",
    );

    let local_loaded = load_skills(LoadSkillsOptions {
        cwd: cwd.to_string_lossy().into_owned(),
        agent_dir: Some(agent_dir.to_string_lossy().into_owned()),
        skill_paths: Vec::new(),
        include_defaults: true,
    });
    let pkg_loaded = load_skills(LoadSkillsOptions {
        cwd: cwd.to_string_lossy().into_owned(),
        agent_dir: Some(agent_dir.to_string_lossy().into_owned()),
        skill_paths: vec![pkg_skill_path.clone()],
        include_defaults: false,
    });

    let (merged, diags) = merge_with_collision_precedence(&local_loaded.skills, &pkg_loaded.skills);
    let web_fetch = merged.iter().find(|s| s.name == "web-fetch").unwrap();
    assert_eq!(web_fetch.file_path, user_skill_path);
    assert_eq!(web_fetch.description, "User web-fetch override");

    let collision = diags
        .iter()
        .find(|d| {
            d.r#type == ResourceDiagnosticType::Collision && d.collision.as_ref().is_some_and(|c| c.name == "web-fetch")
        })
        .unwrap();
    assert!(
        collision
            .collision
            .as_ref()
            .unwrap()
            .loser_path
            .contains("fake-package")
    );

    // Project overrides both user and package (in load_skills user -> project or project explicit precedence)
    let project_skill_path = create_skill_md(
        &cwd.join(".pi/skills/web-fetch"),
        "web-fetch",
        "Project web-fetch override",
        "Project skill content",
    );
    let project_only = load_skills(LoadSkillsOptions {
        cwd: cwd.to_string_lossy().into_owned(),
        agent_dir: Some(agent_dir.to_string_lossy().into_owned()),
        skill_paths: vec![project_skill_path.clone()],
        include_defaults: false,
    });
    let (merged_proj, _) = merge_with_collision_precedence(&project_only.skills, &local_loaded.skills);
    let proj_winner = merged_proj.iter().find(|s| s.name == "web-fetch").unwrap();
    assert_eq!(proj_winner.file_path, project_skill_path);
    assert_eq!(proj_winner.description, "Project web-fetch override");
}

//! Port of `packages/coding-agent/test/skills.test.ts`.

use std::path::PathBuf;

use indexmap::IndexMap;
use pi_coding_agent::core::skills::{
    LoadSkillsFromDirOptions, LoadSkillsOptions, Skill, SkillFileReadTool, format_skills_for_prompt, load_skills,
    load_skills_from_dir,
};
use pi_coding_agent::core::source_info::{SyntheticSourceOptions, create_synthetic_source_info};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skills")
}

fn collision_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skills-collision")
}

fn create_test_skill(name: &str, description: &str, file_path: &str, base_dir: &str, disable: bool) -> Skill {
    Skill {
        name: name.to_owned(),
        description: description.to_owned(),
        file_path: file_path.to_owned(),
        base_dir: base_dir.to_owned(),
        source_info: create_synthetic_source_info(
            file_path,
            SyntheticSourceOptions {
                source: "test".to_owned(),
                scope: None,
                origin: None,
                base_dir: None,
                changelog_path: None,
            },
        ),
        disable_model_invocation: disable,
    }
}

#[test]
fn load_skills_from_dir_cases() {
    let fdir = fixtures_dir();

    let valid = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("valid-skill").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert_eq!(valid.skills.len(), 1);
    assert_eq!(valid.skills[0].name, "valid-skill");
    assert_eq!(valid.skills[0].description, "A valid skill for testing purposes.");
    assert_eq!(valid.skills[0].source_info.source, "test");
    assert!(valid.diagnostics.is_empty());

    let mismatch = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("name-mismatch").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert_eq!(mismatch.skills.len(), 1);
    assert_eq!(mismatch.skills[0].name, "different-name");
    assert!(
        !mismatch
            .diagnostics
            .iter()
            .any(|d| d.message.contains("does not match parent directory"))
    );

    let invalid_chars = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("invalid-name-chars").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert_eq!(invalid_chars.skills.len(), 1);
    assert!(
        invalid_chars
            .diagnostics
            .iter()
            .any(|d| d.message.contains("invalid characters"))
    );

    let long_name = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("long-name").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert_eq!(long_name.skills.len(), 1);
    assert!(
        long_name
            .diagnostics
            .iter()
            .any(|d| d.message.contains("exceeds 64 characters"))
    );

    let missing_desc = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("missing-description").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert!(missing_desc.skills.is_empty());
    assert!(
        missing_desc
            .diagnostics
            .iter()
            .any(|d| d.message.contains("description is required"))
    );

    let unknown = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("unknown-field").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert_eq!(unknown.skills.len(), 1);
    assert!(unknown.diagnostics.is_empty());

    let nested = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("nested").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert_eq!(nested.skills.len(), 1);
    assert_eq!(nested.skills[0].name, "child-skill");
    assert!(nested.diagnostics.is_empty());

    let root_pref = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("root-skill-preferred").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert_eq!(root_pref.skills.len(), 1);
    assert_eq!(root_pref.skills[0].name, "root-skill-preferred");
    assert_eq!(root_pref.skills[0].description, "Root skill should win.");

    let no_fm = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("no-frontmatter").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert!(no_fm.skills.is_empty());
    assert!(
        no_fm
            .diagnostics
            .iter()
            .any(|d| d.message.contains("description is required"))
    );

    let invalid_yaml = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("invalid-yaml").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert!(invalid_yaml.skills.is_empty());
    assert!(invalid_yaml.diagnostics.iter().any(|d| d.message.contains("line")));

    let multiline = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("multiline-description").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert_eq!(multiline.skills.len(), 1);
    assert!(multiline.skills[0].description.contains('\n'));
    assert!(
        multiline.skills[0]
            .description
            .contains("This is a multiline description.")
    );

    let hyphens = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("consecutive-hyphens").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert_eq!(hyphens.skills.len(), 1);
    assert!(
        hyphens
            .diagnostics
            .iter()
            .any(|d| d.message.contains("consecutive hyphens"))
    );

    let all = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert!(all.skills.len() >= 6);

    let missing_dir = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: "/non/existent/path".to_owned(),
        source: "test".to_owned(),
    });
    assert!(missing_dir.skills.is_empty());
    assert!(missing_dir.diagnostics.is_empty());

    let disable = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: fdir.join("disable-model-invocation").to_string_lossy().into_owned(),
        source: "test".to_owned(),
    });
    assert_eq!(disable.skills.len(), 1);
    assert_eq!(disable.skills[0].name, "disable-model-invocation");
    assert!(disable.skills[0].disable_model_invocation);
    assert!(
        !disable
            .diagnostics
            .iter()
            .any(|d| d.message.contains("unknown frontmatter field"))
    );
}

#[test]
fn format_skills_for_prompt_cases() {
    assert_eq!(format_skills_for_prompt(&[], SkillFileReadTool::Read), "");

    let one = vec![create_test_skill(
        "test-skill",
        "A test skill.",
        "/path/to/skill/SKILL.md",
        "/path/to/skill",
        false,
    )];
    let xml = format_skills_for_prompt(&one, SkillFileReadTool::Read);
    assert!(xml.contains("<available_skills>"));
    assert!(xml.contains("</available_skills>"));
    assert!(xml.contains("<skill>"));
    assert!(xml.contains("<name>test-skill</name>"));
    assert!(xml.contains("<description>A test skill.</description>"));
    assert!(xml.contains("<location>/path/to/skill/SKILL.md</location>"));

    let xml_start = xml.find("<available_skills>").unwrap();
    let intro = &xml[..xml_start];
    assert!(intro.contains("The following skills provide specialized instructions"));
    assert!(intro.contains("Use the read tool to load a skill's file"));

    let special = vec![create_test_skill(
        "test-skill",
        "A skill with <special> & \"characters\".",
        "/path/to/skill/SKILL.md",
        "/path/to/skill",
        false,
    )];
    let escaped = format_skills_for_prompt(&special, SkillFileReadTool::Read);
    assert!(escaped.contains("&lt;special&gt;"));
    assert!(escaped.contains("&amp;"));
    assert!(escaped.contains("&quot;characters&quot;"));

    let multi = vec![
        create_test_skill("skill-one", "First skill.", "/path/one/SKILL.md", "/path/one", false),
        create_test_skill("skill-two", "Second skill.", "/path/two/SKILL.md", "/path/two", false),
    ];
    let multi_xml = format_skills_for_prompt(&multi, SkillFileReadTool::Read);
    assert!(multi_xml.contains("<name>skill-one</name>"));
    assert!(multi_xml.contains("<name>skill-two</name>"));
    assert_eq!(multi_xml.matches("<skill>").count(), 2);

    let mixed = vec![
        create_test_skill(
            "visible-skill",
            "A visible skill.",
            "/path/v/SKILL.md",
            "/path/v",
            false,
        ),
        create_test_skill("hidden-skill", "A hidden skill.", "/path/h/SKILL.md", "/path/h", true),
    ];
    let mixed_xml = format_skills_for_prompt(&mixed, SkillFileReadTool::Read);
    assert!(mixed_xml.contains("<name>visible-skill</name>"));
    assert!(!mixed_xml.contains("<name>hidden-skill</name>"));
    assert_eq!(mixed_xml.matches("<skill>").count(), 1);

    let all_hidden = vec![create_test_skill(
        "hidden-skill",
        "A hidden skill.",
        "/path/h/SKILL.md",
        "/path/h",
        true,
    )];
    assert_eq!(format_skills_for_prompt(&all_hidden, SkillFileReadTool::Read), "");
}

#[test]
fn load_skills_options_and_collision_handling() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let empty_agent = manifest_dir
        .join("tests/fixtures/empty-agent")
        .to_string_lossy()
        .into_owned();
    let empty_cwd = manifest_dir
        .join("tests/fixtures/empty-cwd")
        .to_string_lossy()
        .into_owned();

    let explicit = load_skills(LoadSkillsOptions {
        agent_dir: Some(empty_agent.clone()),
        cwd: empty_cwd.clone(),
        skill_paths: vec![fixtures_dir().join("valid-skill").to_string_lossy().into_owned()],
        include_defaults: true,
    });
    assert_eq!(explicit.skills.len(), 1);
    assert_eq!(
        format!("{:?}", explicit.skills[0].source_info.scope).to_lowercase(),
        "temporary"
    );
    assert!(explicit.diagnostics.is_empty());

    let missing = load_skills(LoadSkillsOptions {
        agent_dir: Some(empty_agent.clone()),
        cwd: empty_cwd.clone(),
        skill_paths: vec!["/non/existent/path".to_owned()],
        include_defaults: true,
    });
    assert!(missing.skills.is_empty());
    assert!(missing.diagnostics.iter().any(|d| d.message.contains("does not exist")));

    let home_skills_dir = format!("{}/.pi/agent/skills", pi_js::env::home_dir());
    let with_tilde = load_skills(LoadSkillsOptions {
        agent_dir: Some(empty_agent.clone()),
        cwd: empty_cwd.clone(),
        skill_paths: vec!["~/.pi/agent/skills".to_owned()],
        include_defaults: true,
    });
    let without_tilde = load_skills(LoadSkillsOptions {
        agent_dir: Some(empty_agent),
        cwd: empty_cwd,
        skill_paths: vec![home_skills_dir],
        include_defaults: true,
    });
    assert_eq!(with_tilde.skills.len(), without_tilde.skills.len());

    // Collision handling simulation
    let cdir = collision_fixtures_dir();
    let first = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: cdir.join("first").to_string_lossy().into_owned(),
        source: "first".to_owned(),
    });
    let second = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: cdir.join("second").to_string_lossy().into_owned(),
        source: "second".to_owned(),
    });
    let mut map: IndexMap<String, Skill> = IndexMap::new();
    let mut warnings = Vec::new();
    for s in first.skills {
        map.insert(s.name.clone(), s);
    }
    for s in second.skills {
        if let Some(existing) = map.get(&s.name) {
            warnings.push(format!(
                "name collision: \"{}\" already loaded from {}",
                s.name, existing.file_path
            ));
        } else {
            map.insert(s.name.clone(), s);
        }
    }
    assert_eq!(map.len(), 1);
    assert_eq!(map.get("calendar").unwrap().source_info.source, "first");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("name collision"));
}

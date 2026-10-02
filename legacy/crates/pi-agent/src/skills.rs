use crate::config::{SettingsManager, agent_dir};
use anyhow::Result;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub path: PathBuf,
    pub content: String,
    pub description: String,
    pub disable_model_invocation: bool,
}

pub fn load_agent_docs(cwd: &Path) -> Vec<(PathBuf, String)> {
    let mut docs = Vec::new();
    if let Ok(global_dir) = agent_dir() {
        if let Some(doc) = load_context_file_from_dir(&global_dir) {
            docs.push(doc);
        }
    }

    let mut ancestor_docs = Vec::new();
    let mut current = Some(cwd);
    while let Some(dir) = current {
        if let Some(doc) = load_context_file_from_dir(dir) {
            ancestor_docs.push(doc);
        }
        current = dir.parent();
    }
    ancestor_docs.reverse();
    docs.extend(ancestor_docs);
    docs
}

fn load_context_file_from_dir(dir: &Path) -> Option<(PathBuf, String)> {
    for filename in ["AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"] {
        let path = dir.join(filename);
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                return Some((path, content));
            }
        }
    }
    None
}

pub fn load_skills(cwd: &Path) -> Result<Vec<Skill>> {
    let mut skills = Vec::new();
    let mut bases = vec![
        cwd.join(".pi").join("skills"),
        cwd.join(".agents").join("skills"),
    ];
    if let Ok(manager) = SettingsManager::new(cwd) {
        if let Ok(project) = manager.load_project() {
            bases.extend(project.skills.into_iter().map(PathBuf::from));
        }
        if let Ok(global) = manager.load_global() {
            bases.extend(global.skills.into_iter().map(PathBuf::from));
        }
    }
    if let Ok(global_dir) = agent_dir() {
        bases.push(global_dir.join("skills"));
    }
    for base in bases {
        if !base.exists() {
            continue;
        }
        skills.extend(load_skills_from_dir(&base)?);
    }
    dedupe_skills(&mut skills);
    Ok(skills)
}

fn load_skills_from_dir(base: &Path) -> Result<Vec<Skill>> {
    if base.is_file() {
        return Ok(load_skill_file(base).into_iter().collect());
    }

    let root_skill = base.join("SKILL.md");
    if root_skill.is_file() {
        return Ok(load_skill_file(&root_skill).into_iter().collect());
    }

    let mut skills = Vec::new();
    let mut entries = fs::read_dir(base)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            if let Some(skill) = load_skill_file(&path) {
                push_unique_skill(&mut skills, skill);
            }
        }
    }

    let walker = WalkDir::new(base).into_iter().filter_entry(|entry| {
        let name = entry.file_name().to_string_lossy();
        !(entry.depth() > 0 && (name.starts_with('.') || name == "node_modules"))
    });
    let mut skill_files = walker
        .filter_map(Result::ok)
        .filter(|entry| entry.depth() > 0 && entry.file_type().is_file() && entry.file_name() == "SKILL.md")
        .map(|entry| entry.path().to_path_buf())
        .collect::<Vec<_>>();
    skill_files.sort();
    for skill_file in skill_files {
        if let Some(skill) = load_skill_file(&skill_file) {
            push_unique_skill(&mut skills, skill);
        }
    }
    Ok(skills)
}

fn push_unique_skill(skills: &mut Vec<Skill>, skill: Skill) {
    let duplicate_name = skills.iter().any(|existing| existing.name == skill.name);
    let duplicate_path = skills
        .iter()
        .any(|existing| existing.path.canonicalize().ok() == skill.path.canonicalize().ok());
    if !duplicate_name && !duplicate_path {
        skills.push(skill);
    }
}

fn dedupe_skills(skills: &mut Vec<Skill>) {
    let mut names = HashSet::new();
    let mut paths = HashSet::new();
    skills.retain(|skill| {
        let path = skill.path.canonicalize().unwrap_or_else(|_| skill.path.clone());
        names.insert(skill.name.clone()) && paths.insert(path)
    });
}

fn load_skill_file(path: &Path) -> Option<Skill> {
    let content = fs::read_to_string(path).ok()?;
    let parent_name = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "skill".to_string());
    let description = skill_description(&content)?;
    let name = skill_name(&content).unwrap_or(parent_name);
    let disable_model_invocation = skill_disable_model_invocation(&content);
    Some(Skill {
        name,
        path: path.to_path_buf(),
        content,
        description,
        disable_model_invocation,
    })
}

pub fn build_system_prompt(cwd: &Path) -> Result<String> {
    let mut parts = vec![String::from(
        "You are pi, a headless Rust coding agent. Use tools precisely, prefer minimal diffs, and explain concrete results.",
    )];
    for (path, content) in load_agent_docs(cwd) {
        parts.push(format!(
            "<agents path=\"{}\">\n{}\n</agents>",
            path.display(),
            content
        ));
    }
    let skills = load_skills(cwd)?;
    if !skills.is_empty() {
        parts.push(format_skills_for_prompt(&skills));
    }
    Ok(parts.join("\n\n"))
}

pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    let visible_skills = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect::<Vec<_>>();
    if visible_skills.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        "Use the read tool to load a skill's file when the task matches its description.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];
    for skill in visible_skills {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.path.display().to_string())
        ));
        lines.push("  </skill>".to_string());
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

pub fn skill_description(content: &str) -> Option<String> {
    skill_frontmatter_value(content, "description")
}

pub fn skill_name(content: &str) -> Option<String> {
    skill_frontmatter_value(content, "name")
}

pub fn skill_disable_model_invocation(content: &str) -> bool {
    skill_frontmatter_value(content, "disable-model-invocation")
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn skill_frontmatter_value(content: &str, key: &str) -> Option<String> {
    let rest = content.strip_prefix("---")?;
    let (frontmatter, _) = rest.split_once("\n---")?;
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        let Some((candidate, value)) = trimmed.split_once(':') else {
            continue;
        };
        if candidate.trim() == key {
            return Some(value.trim().trim_matches(['\"', '\'']).to_string())
                .filter(|value| !value.is_empty());
        }
    }
    None
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_require_description_and_support_frontmatter_metadata() {
        let root = std::env::temp_dir().join(format!("pi-rs-skills-{}", uuid::Uuid::new_v4()));
        let visible = root.join("visible");
        let hidden = root.join("hidden");
        let missing = root.join("missing");
        fs::create_dir_all(&visible).unwrap();
        fs::create_dir_all(&hidden).unwrap();
        fs::create_dir_all(&missing).unwrap();
        fs::write(
            visible.join("SKILL.md"),
            "---\nname: custom\ndescription: Visible <desc>\n---\nVisible body\n",
        )
        .unwrap();
        fs::write(
            hidden.join("SKILL.md"),
            "---\ndescription: Hidden desc\ndisable-model-invocation: true\n---\nHidden body\n",
        )
        .unwrap();
        fs::write(missing.join("SKILL.md"), "No description\n").unwrap();

        let skills = load_skills_from_dir(&root).unwrap();
        assert_eq!(skills.len(), 2);
        assert!(skills.iter().any(|skill| skill.name == "custom"));
        assert!(!skills.iter().any(|skill| skill.name == "missing"));

        let prompt = format_skills_for_prompt(&skills);
        assert!(prompt.contains("<name>custom</name>"));
        assert!(prompt.contains("Visible &lt;desc&gt;"));
        assert!(!prompt.contains("Hidden desc"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn duplicate_skill_names_keep_first_loaded_skill() {
        let root = std::env::temp_dir().join(format!("pi-rs-skills-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("one")).unwrap();
        fs::create_dir_all(root.join("two")).unwrap();
        fs::write(
            root.join("one").join("SKILL.md"),
            "---\nname: same\ndescription: First desc\n---\nFirst\n",
        )
        .unwrap();
        fs::write(
            root.join("two").join("SKILL.md"),
            "---\nname: same\ndescription: Second desc\n---\nSecond\n",
        )
        .unwrap();

        let skills = load_skills_from_dir(&root).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "First desc");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn root_skill_file_stops_recursive_skill_discovery() {
        let root = std::env::temp_dir().join(format!("pi-rs-skills-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("SKILL.md"), "---\ndescription: Root desc\n---\nRoot\n").unwrap();
        fs::write(
            root.join("nested").join("SKILL.md"),
            "---\ndescription: Nested desc\n---\nNested\n",
        )
        .unwrap();

        let skills = load_skills_from_dir(&root).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "Root desc");

        let _ = fs::remove_dir_all(root);
    }
}

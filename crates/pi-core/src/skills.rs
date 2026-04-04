use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub path: PathBuf,
    pub content: String,
}

pub fn load_agent_docs(cwd: &Path) -> Vec<(PathBuf, String)> {
    let mut docs = Vec::new();
    let mut current = Some(cwd);
    while let Some(dir) = current {
        let path = dir.join("AGENTS.md");
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                docs.push((path, content));
            }
        }
        current = dir.parent();
    }
    docs.reverse();
    docs
}

pub fn load_skills(cwd: &Path) -> Result<Vec<Skill>> {
    let mut skills = Vec::new();
    for base in [
        cwd.join(".pi").join("skills"),
        cwd.join(".agents").join("skills"),
    ] {
        if !base.exists() {
            continue;
        }
        for entry in WalkDir::new(&base).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() || entry.file_name() != "SKILL.md" {
                continue;
            }
            let content = fs::read_to_string(entry.path())?;
            let name = entry
                .path()
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "skill".to_string());
            skills.push(Skill {
                name,
                path: entry.path().to_path_buf(),
                content,
            });
        }
    }
    Ok(skills)
}

pub fn build_system_prompt(cwd: &Path) -> Result<String> {
    let mut parts = vec![String::from(
        "You are pi-rs, a headless Rust coding agent. Use tools precisely, prefer minimal diffs, and explain concrete results.",
    )];
    for (path, content) in load_agent_docs(cwd) {
        parts.push(format!(
            "<agents path=\"{}\">\n{}\n</agents>",
            path.display(),
            content
        ));
    }
    for skill in load_skills(cwd)? {
        parts.push(format!(
            "<skill name=\"{}\" path=\"{}\">\n{}\n</skill>",
            skill.name,
            skill.path.display(),
            skill.content
        ));
    }
    Ok(parts.join("\n\n"))
}

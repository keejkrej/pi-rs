//! Port of packages/coding-agent/src/core/skills.ts

use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{CONFIG_DIR_NAME, get_agent_dir};
use crate::core::diagnostics::{ResourceCollision, ResourceDiagnostic, ResourceDiagnosticType, ResourceType};
use crate::core::source_info::{
    SourceInfo, SyntheticSourceOptions, create_synthetic_source_info, get_resource_path_key,
};
use crate::utils::frontmatter::try_parse_frontmatter;
use crate::utils::paths::{PathInputOptions, is_path_within, resolve_path};

/// Max name length per Agent Skills spec.
const MAX_NAME_LENGTH: usize = 64;

/// Max description length per Agent Skills spec.
const MAX_DESCRIPTION_LENGTH: usize = 1024;

const IGNORE_FILE_NAMES: &[&str] = &[".gitignore", ".ignore", ".fdignore"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub base_dir: String,
    pub source_info: SourceInfo,
    pub disable_model_invocation: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadSkillsResult {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

#[derive(Clone, Debug)]
pub struct LoadSkillsFromDirOptions {
    /// Directory to scan for skills.
    pub dir: String,
    /// Source identifier for these skills.
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct LoadSkillsOptions {
    /// Working directory for project-local skills.
    pub cwd: String,
    /// Agent config directory for global skills.
    pub agent_dir: Option<String>,
    /// Explicit skill paths (files or directories).
    pub skill_paths: Vec<String>,
    /// Include default skills directories.
    pub include_defaults: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillFileReadTool {
    Read,
    Bash,
    Indirect,
}

impl Default for SkillFileReadTool {
    fn default() -> Self {
        Self::Read
    }
}

fn to_posix_path(p: &str) -> String {
    let sep = pi_js::path::sep();
    if sep == "/" {
        p.to_owned()
    } else {
        p.split(sep).collect::<Vec<_>>().join("/")
    }
}

fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = pi_js::str16::trim(line);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }

    let mut pattern = line;
    let mut negated = false;

    if let Some(rest) = pattern.strip_prefix('!') {
        negated = true;
        pattern = rest;
    } else if let Some(rest) = pattern.strip_prefix("\\!") {
        pattern = rest;
    }

    if let Some(rest) = pattern.strip_prefix('/') {
        pattern = rest;
    }

    let prefixed = if prefix.is_empty() {
        pattern.to_owned()
    } else {
        format!("{prefix}{pattern}")
    };
    Some(if negated { format!("!{prefixed}") } else { prefixed })
}

fn add_ignore_rules(lines: &mut Vec<String>, dir: &str, root_dir: &str) {
    let relative_dir = pi_js::path::relative(root_dir, dir);
    let prefix = if relative_dir.is_empty() {
        String::new()
    } else {
        format!("{}/", to_posix_path(&relative_dir))
    };

    for &filename in IGNORE_FILE_NAMES {
        let ignore_path = pi_js::path::join(&[dir, filename]);
        if !pi_js::fs::exists(&ignore_path) {
            continue;
        }
        if let Ok(content) = pi_js::fs::read_to_string(&ignore_path) {
            for raw_line in content.split('\n') {
                let clean = raw_line.strip_suffix('\r').unwrap_or(raw_line);
                if let Some(pattern) = prefix_ignore_pattern(clean, &prefix) {
                    lines.push(pattern);
                }
            }
        }
    }
}

fn build_gitignore_matcher(root: &str, lines: &[String]) -> Gitignore {
    if lines.is_empty() {
        return Gitignore::empty();
    }
    let mut builder = GitignoreBuilder::new(Path::new(root));
    for line in lines {
        let _ = builder.add_line(None, line);
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

fn is_ignored(matcher: &Gitignore, rel_path: &str, is_dir: bool) -> bool {
    if matcher.is_empty() {
        return false;
    }
    let check_path = rel_path.trim_end_matches('/');
    matcher.matched(Path::new(check_path), is_dir).is_ignore()
}

fn validate_name(name: &str) -> Vec<String> {
    let mut errors = Vec::new();

    if name.encode_utf16().count() > MAX_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({})",
            name.encode_utf16().count()
        ));
    }

    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        errors.push("name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)".to_owned());
    }

    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_owned());
    }

    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_owned());
    }

    errors
}

fn validate_description(description: Option<&str>) -> Vec<String> {
    let mut errors = Vec::new();
    match description {
        None => errors.push("description is required".to_owned()),
        Some(desc) if pi_js::str16::trim(desc).is_empty() => {
            errors.push("description is required".to_owned());
        }
        Some(desc) if desc.encode_utf16().count() > MAX_DESCRIPTION_LENGTH => {
            errors.push(format!(
                "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
                desc.encode_utf16().count()
            ));
        }
        Some(_) => {}
    }
    errors
}

fn create_skill_source_info(file_path: &str, base_dir: &str, source: &str) -> SourceInfo {
    match source {
        "user" => create_synthetic_source_info(
            file_path,
            SyntheticSourceOptions {
                source: "local".to_owned(),
                scope: Some(crate::core::source_info::SourceScope::User),
                origin: None,
                base_dir: Some(base_dir.to_owned()),
                changelog_path: None,
            },
        ),
        "project" => create_synthetic_source_info(
            file_path,
            SyntheticSourceOptions {
                source: "local".to_owned(),
                scope: Some(crate::core::source_info::SourceScope::Project),
                origin: None,
                base_dir: Some(base_dir.to_owned()),
                changelog_path: None,
            },
        ),
        "path" => create_synthetic_source_info(
            file_path,
            SyntheticSourceOptions {
                source: "local".to_owned(),
                scope: None,
                origin: None,
                base_dir: Some(base_dir.to_owned()),
                changelog_path: None,
            },
        ),
        _ => create_synthetic_source_info(
            file_path,
            SyntheticSourceOptions {
                source: source.to_owned(),
                scope: None,
                origin: None,
                base_dir: Some(base_dir.to_owned()),
                changelog_path: None,
            },
        ),
    }
}

pub fn load_skills_from_dir(options: LoadSkillsFromDirOptions) -> LoadSkillsResult {
    let mut ignore_lines = Vec::new();
    load_skills_from_dir_internal(&options.dir, &options.source, true, &mut ignore_lines, &options.dir)
}

fn load_skills_from_dir_internal(
    dir: &str,
    source: &str,
    include_root_files: bool,
    ignore_lines: &mut Vec<String>,
    root_dir: &str,
) -> LoadSkillsResult {
    let mut skills = Vec::new();
    let mut diagnostics = Vec::new();

    if !pi_js::fs::exists(dir) {
        return LoadSkillsResult { skills, diagnostics };
    }

    let saved_len = ignore_lines.len();
    add_ignore_rules(ignore_lines, dir, root_dir);
    let matcher = build_gitignore_matcher(root_dir, ignore_lines);

    if let Ok(entries) = pi_js::fs::readdir_with_file_types(dir) {
        for entry in &entries {
            if entry.name != "SKILL.md" {
                continue;
            }
            let full_path = pi_js::path::join(&[dir, &entry.name]);
            let is_file = if entry.is_symbolic_link() {
                pi_js::fs::stat(&full_path).is_ok_and(|s| s.is_file())
            } else {
                entry.is_file()
            };
            let rel_path = to_posix_path(&pi_js::path::relative(root_dir, &full_path));
            if !is_file || is_ignored(&matcher, &rel_path, false) {
                continue;
            }

            let res = load_skill_from_file(&full_path, source);
            if let Some(skill) = res.skill {
                skills.push(skill);
            }
            diagnostics.extend(res.diagnostics);
            ignore_lines.truncate(saved_len);
            return LoadSkillsResult { skills, diagnostics };
        }

        for entry in &entries {
            if entry.name.starts_with('.') || entry.name == "node_modules" {
                continue;
            }
            let full_path = pi_js::path::join(&[dir, &entry.name]);
            let (is_dir, is_file) = if entry.is_symbolic_link() {
                match pi_js::fs::stat(&full_path) {
                    Ok(stats) => (stats.is_directory(), stats.is_file()),
                    Err(_) => continue,
                }
            } else {
                (entry.is_directory(), entry.is_file())
            };

            let rel_path = to_posix_path(&pi_js::path::relative(root_dir, &full_path));
            if is_ignored(&matcher, &rel_path, is_dir) {
                continue;
            }

            if is_dir {
                let sub = load_skills_from_dir_internal(&full_path, source, false, ignore_lines, root_dir);
                skills.extend(sub.skills);
                diagnostics.extend(sub.diagnostics);
                continue;
            }

            if !is_file || !include_root_files || !entry.name.ends_with(".md") {
                continue;
            }

            let res = load_skill_from_file(&full_path, source);
            if let Some(skill) = res.skill {
                skills.push(skill);
            }
            diagnostics.extend(res.diagnostics);
        }
    }

    ignore_lines.truncate(saved_len);
    LoadSkillsResult { skills, diagnostics }
}

struct SingleSkillResult {
    skill: Option<Skill>,
    diagnostics: Vec<ResourceDiagnostic>,
}

fn load_skill_from_file(file_path: &str, source: &str) -> SingleSkillResult {
    let mut diagnostics = Vec::new();
    let is_declared_skill = pi_js::path::basename(file_path, None) == "SKILL.md";

    let raw_content = match pi_js::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(error) => {
            diagnostics.push(ResourceDiagnostic {
                r#type: ResourceDiagnosticType::Warning,
                message: error.message().to_owned(),
                path: Some(file_path.to_owned()),
                collision: None,
            });
            return SingleSkillResult {
                skill: None,
                diagnostics,
            };
        }
    };

    let frontmatter = match try_parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed.frontmatter,
        Err(error) => {
            if is_declared_skill {
                diagnostics.push(ResourceDiagnostic {
                    r#type: ResourceDiagnosticType::Warning,
                    message: error.message().to_owned(),
                    path: Some(file_path.to_owned()),
                    collision: None,
                });
            }
            return SingleSkillResult {
                skill: None,
                diagnostics,
            };
        }
    };

    let raw_description = frontmatter.get("description");
    let description_str = raw_description.and_then(Value::as_str);
    let has_description = description_str.is_some_and(|s| !pi_js::str16::trim(s).is_empty());
    if !is_declared_skill && !has_description {
        return SingleSkillResult {
            skill: None,
            diagnostics,
        };
    }

    let skill_dir = pi_js::path::dirname(file_path);
    let parent_dir_name = pi_js::path::basename(&skill_dir, None);

    for err_msg in validate_description(description_str) {
        diagnostics.push(ResourceDiagnostic {
            r#type: ResourceDiagnosticType::Warning,
            message: err_msg,
            path: Some(file_path.to_owned()),
            collision: None,
        });
    }

    let frontmatter_name = frontmatter
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let name = frontmatter_name.unwrap_or(&parent_dir_name).to_owned();

    for err_msg in validate_name(&name) {
        diagnostics.push(ResourceDiagnostic {
            r#type: ResourceDiagnosticType::Warning,
            message: err_msg,
            path: Some(file_path.to_owned()),
            collision: None,
        });
    }

    let Some(valid_description) = description_str.filter(|s| !pi_js::str16::trim(s).is_empty()) else {
        return SingleSkillResult {
            skill: None,
            diagnostics,
        };
    };

    let disable_model_invocation = frontmatter.get("disable-model-invocation").and_then(Value::as_bool) == Some(true);

    SingleSkillResult {
        skill: Some(Skill {
            name,
            description: valid_description.to_owned(),
            file_path: file_path.to_owned(),
            base_dir: skill_dir.clone(),
            source_info: create_skill_source_info(file_path, &skill_dir, source),
            disable_model_invocation,
        }),
        diagnostics,
    }
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Format skills for inclusion in a system prompt using the Agent Skills XML standard.
pub fn format_skills_for_prompt(skills: &[Skill], file_read_tool: SkillFileReadTool) -> String {
    let visible_skills: Vec<&Skill> = skills.iter().filter(|s| !s.disable_model_invocation).collect();
    if visible_skills.is_empty() {
        return String::new();
    }

    let tool_instruction = match file_read_tool {
        SkillFileReadTool::Read => "Use the read tool to load a skill's file when the task matches its description.",
        SkillFileReadTool::Bash => "Use bash to load a skill's file when the task matches its description.",
        SkillFileReadTool::Indirect => "Load a skill's file when the task matches its description.",
    };

    let mut lines = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_owned(),
        tool_instruction.to_owned(),
        "Resolve relative paths against the skill's directory (parent of SKILL.md). Use absolute paths in tool calls."
            .to_owned(),
        String::new(),
        "<available_skills>".to_owned(),
    ];

    for skill in visible_skills {
        lines.push("  <skill>".to_owned());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!("    <location>{}</location>", escape_xml(&skill.file_path)));
        lines.push("  </skill>".to_owned());
    }

    lines.push("</available_skills>".to_owned());
    lines.join("\n")
}

/// Load skills from all configured locations.
pub fn load_skills(options: LoadSkillsOptions) -> LoadSkillsResult {
    let resolved_cwd = resolve_path(&options.cwd, None, PathInputOptions::default());
    let agent_dir_raw = options.agent_dir.unwrap_or_else(get_agent_dir);
    let resolved_agent_dir = resolve_path(&agent_dir_raw, None, PathInputOptions::default());

    let mut skill_map: IndexMap<String, Skill> = IndexMap::new();
    let mut real_path_set: IndexSet<String> = IndexSet::new();
    let mut all_diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let mut collision_diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    fn collect_skills(
        result: LoadSkillsResult,
        skill_map: &mut IndexMap<String, Skill>,
        real_path_set: &mut IndexSet<String>,
        all_diagnostics: &mut Vec<ResourceDiagnostic>,
        collision_diagnostics: &mut Vec<ResourceDiagnostic>,
    ) {
        all_diagnostics.extend(result.diagnostics);
        for skill in result.skills {
            let real_path = get_resource_path_key(&skill.file_path);
            if real_path_set.contains(&real_path) {
                continue;
            }
            if let Some(existing) = skill_map.get(&skill.name) {
                collision_diagnostics.push(ResourceDiagnostic {
                    r#type: ResourceDiagnosticType::Collision,
                    message: format!("name \"{}\" collision", skill.name),
                    path: Some(skill.file_path.clone()),
                    collision: Some(ResourceCollision {
                        resource_type: ResourceType::Skill,
                        name: skill.name.clone(),
                        winner_path: existing.file_path.clone(),
                        loser_path: skill.file_path.clone(),
                        winner_source: None,
                        loser_source: None,
                    }),
                });
            } else {
                skill_map.insert(skill.name.clone(), skill);
                real_path_set.insert(real_path);
            }
        }
    }

    let user_skills_dir = pi_js::path::join(&[&resolved_agent_dir, "skills"]);
    let project_skills_dir = pi_js::path::resolve(&[&resolved_cwd, *CONFIG_DIR_NAME, "skills"]);

    if options.include_defaults {
        collect_skills(
            load_skills_from_dir(LoadSkillsFromDirOptions {
                dir: user_skills_dir.clone(),
                source: "user".to_owned(),
            }),
            &mut skill_map,
            &mut real_path_set,
            &mut all_diagnostics,
            &mut collision_diagnostics,
        );
        collect_skills(
            load_skills_from_dir(LoadSkillsFromDirOptions {
                dir: project_skills_dir.clone(),
                source: "project".to_owned(),
            }),
            &mut skill_map,
            &mut real_path_set,
            &mut all_diagnostics,
            &mut collision_diagnostics,
        );
    }

    let get_source = |resolved: &str| -> &'static str {
        if !options.include_defaults {
            if is_path_within(&user_skills_dir, resolved) {
                return "user";
            }
            if is_path_within(&project_skills_dir, resolved) {
                return "project";
            }
        }
        "path"
    };

    for raw_path in &options.skill_paths {
        let resolved = resolve_path(
            raw_path,
            Some(&resolved_cwd),
            PathInputOptions {
                trim: Some(true),
                ..Default::default()
            },
        );
        if !pi_js::fs::exists(&resolved) {
            all_diagnostics.push(ResourceDiagnostic {
                r#type: ResourceDiagnosticType::Warning,
                message: "skill path does not exist".to_owned(),
                path: Some(resolved),
                collision: None,
            });
            continue;
        }

        match pi_js::fs::stat(&resolved) {
            Ok(stats) => {
                let source = get_source(&resolved);
                if stats.is_directory() {
                    collect_skills(
                        load_skills_from_dir(LoadSkillsFromDirOptions {
                            dir: resolved,
                            source: source.to_owned(),
                        }),
                        &mut skill_map,
                        &mut real_path_set,
                        &mut all_diagnostics,
                        &mut collision_diagnostics,
                    );
                } else if stats.is_file() && resolved.ends_with(".md") {
                    let res = load_skill_from_file(&resolved, source);
                    if let Some(skill) = res.skill {
                        collect_skills(
                            LoadSkillsResult {
                                skills: vec![skill],
                                diagnostics: res.diagnostics,
                            },
                            &mut skill_map,
                            &mut real_path_set,
                            &mut all_diagnostics,
                            &mut collision_diagnostics,
                        );
                    } else {
                        all_diagnostics.extend(res.diagnostics);
                    }
                } else {
                    all_diagnostics.push(ResourceDiagnostic {
                        r#type: ResourceDiagnosticType::Warning,
                        message: "skill path is not a markdown file".to_owned(),
                        path: Some(resolved),
                        collision: None,
                    });
                }
            }
            Err(error) => {
                all_diagnostics.push(ResourceDiagnostic {
                    r#type: ResourceDiagnosticType::Warning,
                    message: error.message().to_owned(),
                    path: Some(resolved),
                    collision: None,
                });
            }
        }
    }

    all_diagnostics.extend(collision_diagnostics);
    LoadSkillsResult {
        skills: skill_map.into_values().collect(),
        diagnostics: all_diagnostics,
    }
}

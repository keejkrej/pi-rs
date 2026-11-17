//! Prompt template loading, argument parsing, and expansion.
//!
//! Port of `packages/coding-agent/src/core/prompt-templates.ts`.

use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::CONFIG_DIR_NAME;
use crate::core::diagnostics::{ResourceDiagnostic, ResourceDiagnosticType};
use crate::core::source_info::{SourceInfo, SourceScope, SyntheticSourceOptions, create_synthetic_source_info};
use crate::utils::frontmatter::try_parse_frontmatter;
use crate::utils::paths::{PathInputOptions, resolve_path};

/// Represents a prompt template loaded from a markdown file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    pub content: String,
    pub source_info: SourceInfo,
    pub file_path: String,
}

static SUBSTITUTION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{(\d+|ARGUMENTS|@):-([^}]*)\}|\$\{@:(\d+)(?::(\d+))?\}|\$(ARGUMENTS|@|\d+)")
        .expect("valid prompt substitution regex")
});

static COMMAND_EXPAND_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^/([^\s]+)(?:\s+([\s\S]*))?$").expect("valid command expand regex"));

/// Parse command arguments respecting quoted strings (bash-style).
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for ch in args_string.chars() {
        if let Some(q) = in_quote {
            if ch == q {
                in_quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
        } else if pi_js::str16::is_js_whitespace(ch) {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

/// Substitute argument placeholders in template content.
///
/// Supports `$1`, `$2`, ..., `$@`, `$ARGUMENTS`, `${N:-default}`, `${@:-default}`,
/// `${ARGUMENTS:-default}`, `${@:N}`, and `${@:N:L}` without recursive substitution.
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");

    SUBSTITUTION_REGEX
        .replace_all(content, |caps: &Captures| -> String {
            if let Some(default_target) = caps.get(1) {
                let target = default_target.as_str();
                let default_value = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                let value = if target == "@" || target == "ARGUMENTS" {
                    Some(all_args.as_str())
                } else {
                    target
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| n.checked_sub(1))
                        .and_then(|idx| args.get(idx))
                        .map(String::as_str)
                };
                return match value {
                    Some(v) if !v.is_empty() => v.to_owned(),
                    _ => default_value.to_owned(),
                };
            }

            if let Some(slice_start) = caps.get(3) {
                let start = slice_start.as_str().parse::<usize>().unwrap_or(1).saturating_sub(1);
                if start >= args.len() {
                    return String::new();
                }
                if let Some(slice_len) = caps.get(4) {
                    let length = slice_len.as_str().parse::<usize>().unwrap_or(0);
                    let end = start.saturating_add(length).min(args.len());
                    return args[start..end].join(" ");
                }
                return args[start..].join(" ");
            }

            if let Some(simple) = caps.get(5) {
                let sym = simple.as_str();
                if sym == "ARGUMENTS" || sym == "@" {
                    return all_args.clone();
                }
                return sym
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|idx| args.get(idx))
                    .cloned()
                    .unwrap_or_default();
            }

            String::new()
        })
        .into_owned()
}

fn load_template_from_file(
    file_path: &str,
    source_info: SourceInfo,
) -> (Option<PromptTemplate>, Vec<ResourceDiagnostic>) {
    let mut diagnostics = Vec::new();
    let raw_content = match fs::read_to_string(file_path) {
        Ok(text) => text,
        Err(err) => {
            diagnostics.push(ResourceDiagnostic {
                r#type: ResourceDiagnosticType::Warning,
                message: err.to_string(),
                path: Some(file_path.to_owned()),
                collision: None,
            });
            return (None, diagnostics);
        }
    };

    let parsed = match try_parse_frontmatter(&raw_content) {
        Ok(p) => p,
        Err(err) => {
            diagnostics.push(ResourceDiagnostic {
                r#type: ResourceDiagnosticType::Warning,
                message: err.to_string(),
                path: Some(file_path.to_owned()),
                collision: None,
            });
            return (None, diagnostics);
        }
    };

    let file_name = Path::new(file_path).file_name().and_then(|s| s.to_str()).unwrap_or("");
    let name = file_name.strip_suffix(".md").unwrap_or(file_name).to_owned();

    let mut description = parsed
        .frontmatter
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if description.is_empty()
        && let Some(first_line) = parsed
            .body
            .split('\n')
            .find(|line| !pi_js::str16::trim(line).is_empty())
    {
        if pi_js::str16::len(first_line) > 60 {
            let cut = pi_js::str16::utf16_to_byte(first_line, 60);
            description = format!("{}...", &first_line[..cut]);
        } else {
            description = first_line.to_owned();
        }
    }

    let argument_hint = parsed
        .frontmatter
        .get("argument-hint")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let template = PromptTemplate {
        name,
        description,
        argument_hint,
        content: parsed.body,
        source_info,
        file_path: file_path.to_owned(),
    };
    (Some(template), diagnostics)
}

fn load_templates_from_dir<F>(dir: &str, get_source_info: &F) -> LoadPromptTemplatesResult
where
    F: Fn(&str) -> SourceInfo,
{
    let mut templates = Vec::new();
    let mut diagnostics = Vec::new();

    if !Path::new(dir).exists() {
        return LoadPromptTemplatesResult { templates, diagnostics };
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return LoadPromptTemplatesResult { templates, diagnostics };
    };

    for entry in entries.flatten() {
        let full_path = entry.path();
        let Some(full_path_str) = full_path.to_str() else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_file = if file_type.is_symlink() {
            fs::metadata(&full_path).is_ok_and(|meta| meta.is_file())
        } else {
            file_type.is_file()
        };

        if is_file && full_path_str.ends_with(".md") {
            let (template, diags) = load_template_from_file(full_path_str, get_source_info(full_path_str));
            if let Some(t) = template {
                templates.push(t);
            }
            diagnostics.extend(diags);
        }
    }

    LoadPromptTemplatesResult { templates, diagnostics }
}

/// Options for [`load_prompt_templates`].
#[derive(Clone, Debug, Default)]
pub struct LoadPromptTemplatesOptions {
    /// Working directory for project-local templates.
    pub cwd: String,
    /// Agent config directory for global templates.
    pub agent_dir: String,
    /// Explicit prompt template paths (files or directories).
    pub prompt_paths: Vec<String>,
    /// Include default prompt directories.
    pub include_defaults: bool,
}

/// Templates and warning diagnostics produced by [`load_prompt_templates`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadPromptTemplatesResult {
    pub templates: Vec<PromptTemplate>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

fn is_under_path(target: &Path, root: &Path) -> bool {
    target.starts_with(root)
}

/// Load all prompt templates from:
/// 1. Global: `agent_dir/prompts/`
/// 2. Project: `cwd/{CONFIG_DIR_NAME}/prompts/`
/// 3. Explicit prompt paths
pub fn load_prompt_templates(options: LoadPromptTemplatesOptions) -> LoadPromptTemplatesResult {
    let resolved_cwd = resolve_path(&options.cwd, None, PathInputOptions::default());
    let resolved_agent_dir = resolve_path(&options.agent_dir, None, PathInputOptions::default());

    let mut templates = Vec::new();
    let mut diagnostics = Vec::new();

    let global_prompts_dir = Path::new(&resolved_agent_dir).join("prompts");
    let project_prompts_dir = Path::new(&resolved_cwd).join(*CONFIG_DIR_NAME).join("prompts");
    let global_dir_str = global_prompts_dir.to_string_lossy().into_owned();
    let project_dir_str = project_prompts_dir.to_string_lossy().into_owned();

    let get_source_info = |resolved: &str| -> SourceInfo {
        let p = Path::new(resolved);
        if is_under_path(p, &global_prompts_dir) {
            return create_synthetic_source_info(
                resolved,
                SyntheticSourceOptions {
                    source: "local".to_owned(),
                    scope: Some(SourceScope::User),
                    origin: None,
                    base_dir: Some(global_dir_str.clone()),
                    changelog_path: None,
                },
            );
        }
        if is_under_path(p, &project_prompts_dir) {
            return create_synthetic_source_info(
                resolved,
                SyntheticSourceOptions {
                    source: "local".to_owned(),
                    scope: Some(SourceScope::Project),
                    origin: None,
                    base_dir: Some(project_dir_str.clone()),
                    changelog_path: None,
                },
            );
        }
        let base_dir = if fs::metadata(p).is_ok_and(|m| m.is_dir()) {
            resolved.to_owned()
        } else {
            p.parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        create_synthetic_source_info(
            resolved,
            SyntheticSourceOptions {
                source: "local".to_owned(),
                scope: None,
                origin: None,
                base_dir: Some(base_dir),
                changelog_path: None,
            },
        )
    };

    if options.include_defaults {
        let g = load_templates_from_dir(&global_dir_str, &get_source_info);
        templates.extend(g.templates);
        diagnostics.extend(g.diagnostics);

        let p = load_templates_from_dir(&project_dir_str, &get_source_info);
        templates.extend(p.templates);
        diagnostics.extend(p.diagnostics);
    }

    for raw_path in &options.prompt_paths {
        let resolved_str = resolve_path(
            raw_path,
            Some(&resolved_cwd),
            PathInputOptions {
                trim: Some(true),
                ..PathInputOptions::default()
            },
        );
        let p = Path::new(&resolved_str);
        if !p.exists() {
            continue;
        }
        match fs::metadata(p) {
            Ok(meta) if meta.is_dir() => {
                let res = load_templates_from_dir(&resolved_str, &get_source_info);
                templates.extend(res.templates);
                diagnostics.extend(res.diagnostics);
            }
            Ok(meta) if meta.is_file() && resolved_str.ends_with(".md") => {
                let (template, diags) = load_template_from_file(&resolved_str, get_source_info(&resolved_str));
                if let Some(t) = template {
                    templates.push(t);
                }
                diagnostics.extend(diags);
            }
            Ok(_) => {}
            Err(err) => {
                diagnostics.push(ResourceDiagnostic {
                    r#type: ResourceDiagnosticType::Warning,
                    message: err.to_string(),
                    path: Some(resolved_str),
                    collision: None,
                });
            }
        }
    }

    LoadPromptTemplatesResult { templates, diagnostics }
}

/// Expand a prompt template if it matches a template name.
/// Returns the expanded content or the original text if not a template.
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    if !text.starts_with('/') {
        return text.to_owned();
    }

    let Some(caps) = COMMAND_EXPAND_REGEX.captures(text) else {
        return text.to_owned();
    };
    let template_name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let args_string = caps.get(2).map(|m| m.as_str()).unwrap_or("");

    if let Some(template) = templates.iter().find(|t| t.name == template_name) {
        let args = parse_command_args(args_string);
        return substitute_args(&template.content, &args);
    }

    text.to_owned()
}

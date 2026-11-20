//! System prompt construction and project context loading.
//!
//! Port of `packages/coding-agent/src/core/system-prompt.ts`.

use std::sync::LazyLock;

use indexmap::{IndexMap, IndexSet};
use pi_ai::{SystemMessage, SystemMessageContent, get_system_message_text};
use pi_js::error::{Error, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::{get_docs_path, get_examples_path, get_readme_path};
use crate::core::skills::{Skill, SkillFileReadTool, format_skills_for_prompt};

/// Pre-loaded project context file entry (`AGENTS.md` / `CLAUDE.md`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextFile {
    pub path: String,
    pub content: String,
}

/// Options accepted by [`build_system_prompt_sections`] and [`build_system_prompt`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildSystemPromptOptions {
    /// Custom system prompt (replaces the default prefix).
    pub custom_prompt: Option<String>,
    /// Exact full prompt replacement set by a `before_agent_start` handler.
    pub force_system_prompt: Option<String>,
    /// Tools to include in prompt. Default: `["read", "bash", "edit", "write"]`.
    pub selected_tools: Option<Vec<String>>,
    /// Selected tools hidden from declarations (reachable only indirectly through another tool).
    pub hidden_tools: Option<Vec<String>>,
    /// Optional one-line tool snippets keyed by tool name.
    pub tool_snippets: Option<IndexMap<String, String>>,
    /// Guideline bullets contributed by each tool, keyed by tool name.
    pub tool_guidelines: Option<IndexMap<String, Vec<String>>>,
    /// Additional guideline bullets appended to the default system prompt rules.
    pub prompt_guidelines: Option<Vec<String>>,
    /// Text appended from user configuration before project context, skills, and cwd.
    pub append_system_prompt: Option<String>,
    /// Additional XML-wrapped prompt sections keyed by tag name.
    pub sections: Option<IndexMap<String, String>>,
    /// Working directory.
    pub cwd: String,
    /// Pre-loaded context files.
    pub context_files: Option<Vec<ContextFile>>,
    /// Pre-loaded skills.
    pub skills: Option<Vec<Skill>>,
}

/// Normalized collection-complete representation of [`BuildSystemPromptOptions`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedBuildSystemPromptOptions {
    pub custom_prompt: Option<String>,
    pub force_system_prompt: Option<String>,
    pub selected_tools: Vec<String>,
    pub hidden_tools: Vec<String>,
    pub tool_snippets: IndexMap<String, String>,
    pub tool_guidelines: IndexMap<String, Vec<String>>,
    pub prompt_guidelines: Vec<String>,
    pub append_system_prompt: String,
    pub sections: IndexMap<String, String>,
    pub cwd: String,
    pub context_files: Vec<ContextFile>,
    pub skills: Vec<Skill>,
}

/// Ordered system prompt sections, keyed by name.
pub type SystemPromptSections = IndexMap<String, String>;

/// Rendered system prompt state corresponding to a `SystemMessage`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemPromptState {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<SystemPromptSections>,
}

static SYSTEM_PROMPT_SECTION_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_-]*$").expect("valid section name regex"));

/// Normalize prompt input into the mutable, collection-complete shape exposed to extensions.
pub fn normalize_build_system_prompt_options(input: &BuildSystemPromptOptions) -> NormalizedBuildSystemPromptOptions {
    NormalizedBuildSystemPromptOptions {
        custom_prompt: input.custom_prompt.clone(),
        force_system_prompt: input.force_system_prompt.clone(),
        selected_tools: input.selected_tools.clone().unwrap_or_else(|| {
            vec![
                "read".to_owned(),
                "bash".to_owned(),
                "edit".to_owned(),
                "write".to_owned(),
            ]
        }),
        hidden_tools: input.hidden_tools.clone().unwrap_or_default(),
        tool_snippets: input.tool_snippets.clone().unwrap_or_default(),
        tool_guidelines: input.tool_guidelines.clone().unwrap_or_default(),
        prompt_guidelines: input.prompt_guidelines.clone().unwrap_or_default(),
        append_system_prompt: input.append_system_prompt.clone().unwrap_or_default(),
        sections: input.sections.clone().unwrap_or_default(),
        cwd: input.cwd.clone(),
        context_files: input.context_files.clone().unwrap_or_default(),
        skills: input.skills.clone().unwrap_or_default(),
    }
}

fn render_project_context(context_files: &[ContextFile]) -> String {
    let mut parts = vec!["Project-specific instructions and guidelines:".to_owned()];
    for ContextFile { path, content } in context_files {
        parts.push(format!(
            "<project_instructions path=\"{path}\">\n{content}\n</project_instructions>"
        ));
    }
    parts.join("\n\n")
}

fn build_rules(
    selected_tools: &[String],
    tool_guidelines: &IndexMap<String, Vec<String>>,
    prompt_guidelines: &[String],
) -> String {
    let mut rules: Vec<String> = Vec::new();
    let mut seen: IndexSet<String> = IndexSet::new();
    let mut add_rule = |rule: &str| {
        let normalized = pi_js::str16::trim(rule);
        if normalized.is_empty() || seen.contains(normalized) {
            return;
        }
        seen.insert(normalized.to_owned());
        rules.push(normalized.to_owned());
    };

    let has_tool = |name: &str| selected_tools.iter().any(|t| t == name);
    let has_bash = has_tool("bash");
    let has_powershell = has_tool("powershell");
    let has_grep = has_tool("grep");
    let has_find = has_tool("find");
    let has_ls = has_tool("ls");

    if (has_bash || has_powershell) && !has_grep && !has_find && !has_ls {
        if has_bash && has_powershell {
            add_rule("Use bash or PowerShell for file operations like listing, searching, and finding files");
        } else if has_powershell {
            add_rule("Use PowerShell for file operations like listing, searching, and finding files");
        } else {
            add_rule("Use bash for file operations like ls, rg, find");
        }
    }

    for name in selected_tools {
        if let Some(guidelines) = tool_guidelines.get(name) {
            for rule in guidelines {
                add_rule(rule);
            }
        }
    }
    for rule in prompt_guidelines {
        add_rule(rule);
    }
    add_rule("Be concise in your responses");
    add_rule("Show file paths clearly when working with files");

    rules
        .into_iter()
        .map(|rule| format!("- {rule}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the ordered, independently replaceable sections of the structured system prompt.
pub fn build_system_prompt_sections(input: &BuildSystemPromptOptions) -> Result<SystemPromptSections> {
    let options = normalize_build_system_prompt_options(input);

    for name in options.sections.keys() {
        if !SYSTEM_PROMPT_SECTION_NAME.is_match(name) || name == "preamble" {
            return Err(Error::msg(format!("Invalid system prompt section name: {name}")));
        }
    }

    let declared_tools: Vec<String> = options
        .selected_tools
        .iter()
        .filter(|name| !options.hidden_tools.contains(name))
        .cloned()
        .collect();

    let mut prompt_sections: IndexMap<String, String> = IndexMap::new();
    if let Some(custom) = &options.custom_prompt
        && !custom.is_empty()
    {
        prompt_sections.insert("preamble".to_owned(), custom.clone());
    } else {
        prompt_sections.insert(
            "preamble".to_owned(),
            "You are a coding assistant running inside pi, a coding agent harness. Help users inspect files, run commands, and modify code.".to_owned(),
        );
        let visible_tools: Vec<String> = declared_tools
            .iter()
            .filter_map(|name| {
                options
                    .tool_snippets
                    .get(name)
                    .filter(|s| !s.is_empty())
                    .map(|snippet| format!("- {name}: {snippet}"))
            })
            .collect();
        let tools_body = if visible_tools.is_empty() {
            "(none)".to_owned()
        } else {
            visible_tools.join("\n")
        };
        prompt_sections.insert(
            "tools".to_owned(),
            format!("{tools_body}\n\nAdditional tools may be available."),
        );
        prompt_sections.insert(
            "rules".to_owned(),
            build_rules(&declared_tools, &options.tool_guidelines, &options.prompt_guidelines),
        );
        let docs = format!(
            "Pi documentation (read only when the user asks about the harness itself, its SDK, extensions, themes, skills, or TUI):
- Main documentation: {}
- Additional docs: {}
- Examples: {} (extensions, custom tools, SDK)
- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory
- When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), packages (docs/packages.md), environment variables (docs/environment-variables.md), MCP servers (docs/mcp.md), codemode scripts and non-LLM models such as classifiers and image models (docs/codemode.md)
- Before implementing harness changes, read the relevant docs and examples in full, following related cross-references.",
            get_readme_path(),
            get_docs_path(),
            get_examples_path()
        );
        prompt_sections.insert("docs".to_owned(), docs);
    }

    if !options.append_system_prompt.is_empty() {
        prompt_sections.insert("addendum".to_owned(), options.append_system_prompt);
    }
    if !options.context_files.is_empty() {
        prompt_sections.insert(
            "project_context".to_owned(),
            render_project_context(&options.context_files),
        );
    }

    let has_declared = |tool: &str| declared_tools.iter().any(|t| t == tool);
    let has_selected = |tool: &str| options.selected_tools.iter().any(|t| t == tool);
    let skill_file_read_tool = if has_declared("read") {
        Some(SkillFileReadTool::Read)
    } else if has_declared("bash") {
        Some(SkillFileReadTool::Bash)
    } else if has_selected("read") || has_selected("bash") {
        Some(SkillFileReadTool::Indirect)
    } else {
        None
    };

    if let Some(read_tool) = skill_file_read_tool
        && !options.skills.is_empty()
    {
        let formatted = format_skills_for_prompt(&options.skills, read_tool);
        let skills_prompt = pi_js::str16::trim(&formatted);
        if !skills_prompt.is_empty() {
            prompt_sections.insert("skills".to_owned(), skills_prompt.to_owned());
        }
    }

    prompt_sections.insert("cwd".to_owned(), options.cwd.replace('\\', "/"));
    for (name, content) in options.sections {
        if !content.is_empty() {
            prompt_sections.insert(name, content);
        }
    }

    let mut sections = SystemPromptSections::new();
    if let Some(preamble) = prompt_sections.get("preamble") {
        sections.insert("preamble".to_owned(), preamble.clone());
    }
    for (name, content) in prompt_sections {
        if name != "preamble" {
            sections.insert(name.clone(), format!("<{name}>\n{content}\n</{name}>"));
        }
    }
    Ok(sections)
}

/// Build the complete prompt state for `input`.
pub fn build_system_prompt_state(input: &BuildSystemPromptOptions) -> Result<SystemPromptState> {
    if let Some(forced) = &input.force_system_prompt {
        return Ok(SystemPromptState {
            content: forced.clone(),
            sections: None,
        });
    }
    Ok(SystemPromptState {
        content: String::new(),
        sections: Some(build_system_prompt_sections(input)?),
    })
}

/// Build the system prompt text, rendered exactly as the transcript's system message replays it.
pub fn build_system_prompt(input: &BuildSystemPromptOptions) -> Result<String> {
    let state = build_system_prompt_state(input)?;
    let msg = SystemMessage {
        content: SystemMessageContent::Text(state.content),
        sections: state
            .sections
            .map(|secs| secs.into_iter().map(|(k, v)| (k, Some(v))).collect()),
        tools_added: None,
        tools_removed: None,
        timestamp: 0,
    };
    Ok(get_system_message_text(&msg))
}

/// Build a full system prompt from [`NormalizedBuildSystemPromptOptions`].
pub fn build_system_prompt_from_normalized(options: &NormalizedBuildSystemPromptOptions) -> Result<String> {
    build_system_prompt(&BuildSystemPromptOptions {
        custom_prompt: options.custom_prompt.clone(),
        force_system_prompt: options.force_system_prompt.clone(),
        selected_tools: Some(options.selected_tools.clone()),
        hidden_tools: Some(options.hidden_tools.clone()),
        tool_snippets: Some(options.tool_snippets.clone()),
        tool_guidelines: Some(options.tool_guidelines.clone()),
        prompt_guidelines: Some(options.prompt_guidelines.clone()),
        append_system_prompt: Some(options.append_system_prompt.clone()),
        sections: Some(options.sections.clone()),
        cwd: options.cwd.clone(),
        context_files: Some(options.context_files.clone()),
        skills: Some(options.skills.clone()),
    })
}

/// Diff the sections currently active against desired sections.
/// Returns a `SystemMessage.sections` patch (`None` values represent deleted sections),
/// or `None` when nothing changed.
pub fn diff_system_prompt_sections(
    previous: &IndexMap<String, Option<String>>,
    current: &SystemPromptSections,
) -> Option<IndexMap<String, Option<String>>> {
    let mut patch: IndexMap<String, Option<String>> = IndexMap::new();
    for (name, text) in current {
        let changed = match previous.get(name) {
            Some(Some(prev_text)) => prev_text != text,
            _ => true,
        };
        if changed {
            patch.insert(name.clone(), Some(text.clone()));
        }
    }
    for name in previous.keys() {
        if !current.contains_key(name) {
            patch.insert(name.clone(), None);
        }
    }
    if patch.is_empty() { None } else { Some(patch) }
}

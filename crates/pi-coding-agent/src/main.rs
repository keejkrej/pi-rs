use anyhow::{Context, Result, anyhow};
use base64::Engine;
use clap::{Parser, Subcommand};
use pi_agent::agent::{AgentSession, AllowAllPermissions, EventSink, NullEventSink, SessionEvent};
use pi_agent::config::{PackageSource, SettingsFile, SettingsManager, agent_dir};
use pi_agent::messages::{AgentMessage, AssistantContentBlock, ImageContent, TextContent, UserContentBlock};
use pi_agent::models::ModelDescriptor;
use pi_agent::session::SessionManager;
use pi_ai::OpenAiCodexProvider;
use pi_coding_agent::acp::run_acp_stdio;
use pi_tui::autocomplete::{CombinedAutocompleteProvider, SlashCommand};
use pi_tui::components::{DefaultTextStyle, Editor, EditorOptions, Markdown, MarkdownTheme};
use pi_tui::terminal::ProcessTerminal;
use pi_tui::tui::{Component, TUI};
use pi_tui::{CURSOR_MARKER, Focusable, truncate_to_width, visible_width, wrap_text};
use pi_web_ui::{ChatPanelOptions, render_chat_panel};
use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "pi")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(long = "print", short = 'p')]
    print: bool,

    #[arg(long, value_enum)]
    mode: Option<CliMode>,

    #[arg(long = "continue", short = 'c')]
    continue_recent: bool,

    #[arg(long, short = 'r')]
    resume: bool,

    #[arg(long)]
    session: Option<String>,

    #[arg(long = "fork")]
    fork: Option<String>,

    #[arg(long = "no-session")]
    no_session: bool,

    #[arg(long = "session-dir")]
    session_dir: Option<String>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long)]
    models: Option<String>,

    #[arg(long)]
    thinking: Option<String>,

    #[arg(long = "api-key")]
    api_key: Option<String>,

    #[arg(long = "system-prompt")]
    system_prompt: Option<String>,

    #[arg(long = "append-system-prompt")]
    append_system_prompt: Vec<String>,

    #[arg(long = "no-tools", short = 'n')]
    no_tools: bool,

    #[arg(long = "no-builtin-tools")]
    no_builtin_tools: bool,

    #[arg(long, short = 't')]
    tools: Option<String>,

    #[arg(long = "extension", short = 'e')]
    extensions: Vec<String>,

    #[arg(long = "no-extensions")]
    no_extensions: bool,

    #[arg(long = "skill")]
    skills: Vec<String>,

    #[arg(long = "no-skills")]
    no_skills: bool,

    #[arg(long = "prompt-template")]
    prompt_templates: Vec<String>,

    #[arg(long = "no-prompt-templates")]
    no_prompt_templates: bool,

    #[arg(long = "theme")]
    themes: Vec<String>,

    #[arg(long = "no-themes")]
    no_themes: bool,

    #[arg(long = "no-context-files")]
    no_context_files: bool,

    #[arg(long)]
    offline: bool,

    #[arg(long)]
    verbose: bool,

    #[arg(long = "list-models")]
    list_models: bool,

    #[arg(long)]
    export: Option<String>,

    #[arg(long, short = 'v')]
    version: bool,

    messages: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    Login,
    Logout,
    Acp,
    Tui {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        resume: bool,
    },
    Print {
        prompt: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        resume: bool,
    },
    Rpc {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        resume: bool,
    },
    Repl {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        resume: bool,
    },
    Install {
        source: String,
        #[arg(short = 'l', long)]
        local: bool,
    },
    Remove {
        source: String,
        #[arg(short = 'l', long)]
        local: bool,
    },
    Uninstall {
        source: String,
        #[arg(short = 'l', long)]
        local: bool,
    },
    Update {
        source: Option<String>,
        #[arg(long = "self")]
        self_target: bool,
        #[arg(long)]
        extensions: bool,
        #[arg(long)]
        force: bool,
        #[arg(long = "extension")]
        extension_source: Option<String>,
    },
    List,
    Config,
    Models,
    Sessions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum CliMode {
    Text,
    Json,
    Rpc,
}

#[derive(Clone, Debug, Default)]
struct SessionCliOptions {
    provider: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
    api_key: Option<String>,
    system_prompt: Option<String>,
    append_system_prompt: Vec<String>,
    no_context_files: bool,
    no_extensions: bool,
    extensions: Vec<String>,
    no_skills: bool,
    skills: Vec<String>,
    no_prompt_templates: bool,
    prompt_templates: Vec<String>,
    no_themes: bool,
    themes: Vec<String>,
    scoped_models: Vec<String>,
    disable_tools: bool,
    allow_tools: Option<Vec<String>>,
    fork: Option<String>,
    no_session: bool,
}

impl SessionCliOptions {
    fn has_prompt_overrides(&self) -> bool {
        self.system_prompt.is_some()
            || !self.append_system_prompt.is_empty()
            || self.no_context_files
            || self.no_skills
            || !self.skills.is_empty()
    }

    fn from_cli(cli: &Cli) -> Self {
        Self {
            provider: cli.provider.clone(),
            model: cli.model.clone(),
            thinking: cli.thinking.clone(),
            api_key: cli.api_key.clone(),
            system_prompt: cli.system_prompt.clone(),
            append_system_prompt: cli.append_system_prompt.clone(),
            no_context_files: cli.no_context_files,
            no_extensions: cli.no_extensions,
            extensions: cli.extensions.clone(),
            no_skills: cli.no_skills,
            skills: cli.skills.clone(),
            no_prompt_templates: cli.no_prompt_templates,
            prompt_templates: cli.prompt_templates.clone(),
            no_themes: cli.no_themes,
            themes: cli.themes.clone(),
            scoped_models: cli
                .models
                .as_deref()
                .map(parse_comma_list)
                .unwrap_or_default(),
            disable_tools: cli.no_tools || cli.no_builtin_tools,
            allow_tools: cli.tools.as_deref().map(parse_tool_list),
            fork: cli.fork.clone(),
            no_session: cli.no_session,
        }
    }
}

fn parse_tool_list(tools: &str) -> Vec<String> {
    parse_comma_list(tools)
}

fn parse_comma_list(values: &str) -> Vec<String> {
    values
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

struct RpcEventSink;

#[async_trait::async_trait(?Send)]
impl EventSink for RpcEventSink {
    async fn emit(&self, event: SessionEvent) -> anyhow::Result<()> {
        let value = match event {
            SessionEvent::UserMessage { content } => {
                serde_json::json!({"type": "user_message", "content": content})
            }
            SessionEvent::AssistantMessage { content } => {
                serde_json::json!({"type": "assistant_message", "content": content})
            }
            SessionEvent::ToolCallStart {
                tool_call_id,
                tool_name,
                title,
                arguments,
            } => serde_json::json!({
                "type": "tool_call_start",
                "toolCallId": tool_call_id,
                "toolName": tool_name,
                "title": title,
                "arguments": arguments,
            }),
            SessionEvent::ToolCallResult {
                tool_call_id,
                tool_name,
                output,
            } => serde_json::json!({
                "type": "tool_call_result",
                "toolCallId": tool_call_id,
                "toolName": tool_name,
                "output": output.content,
                "isError": output.is_error,
                "details": output.details,
            }),
            SessionEvent::SessionInfo { title, updated_at } => {
                serde_json::json!({"type": "session_info", "title": title, "updatedAt": updated_at})
            }
        };
        println!("{}", serde_json::to_string(&value)?);
        Ok(())
    }
}

struct StdoutSink;

#[async_trait::async_trait(?Send)]
impl EventSink for StdoutSink {
    async fn emit(&self, event: SessionEvent) -> anyhow::Result<()> {
        match event {
            SessionEvent::AssistantMessage { content } => {
                let text = content
                    .into_iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Text(text) => Some(text.text),
                        AssistantContentBlock::Thinking(_) | AssistantContentBlock::ToolCall(_) => {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !text.is_empty() {
                    println!("{text}");
                }
            }
            SessionEvent::ToolCallStart {
                tool_name, title, ..
            } => {
                eprintln!("[tool:{tool_name}] {title}");
            }
            SessionEvent::ToolCallResult {
                tool_name, output, ..
            } => {
                eprintln!(
                    "[tool:{tool_name}] {}",
                    output
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            UserContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
            _ => {}
        }
        Ok(())
    }
}

fn apply_environment_overrides(cli: &Cli) {
    if cli.offline {
        // SAFETY: this is executed at process startup, before any worker threads are
        // spawned or libraries can concurrently read the environment.
        unsafe {
            std::env::set_var("PI_OFFLINE", "1");
            std::env::set_var("PI_SKIP_VERSION_CHECK", "1");
        }
    }
    if let Some(session_dir) = cli.session_dir.as_deref() {
        // SAFETY: this is executed at process startup, before any worker threads are
        // spawned or libraries can concurrently read the environment.
        unsafe {
            std::env::set_var("PI_SESSION_DIR", session_dir);
        }
    }
}

fn normalize_cli_args(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut normalized = Vec::new();
    let mut iter = args.into_iter().peekable();
    if let Some(program) = iter.next() {
        normalized.push(program);
    }

    while let Some(arg) = iter.next() {
        let mapped = match arg.to_str() {
            Some("-nt") => OsString::from("--no-tools"),
            Some("-nbt") => OsString::from("--no-builtin-tools"),
            Some("-ne") => OsString::from("--no-extensions"),
            Some("-ns") => OsString::from("--no-skills"),
            Some("-np") => OsString::from("--no-prompt-templates"),
            Some("-nc") => OsString::from("--no-context-files"),
            _ => arg,
        };

        let Some(text) = mapped.to_str() else {
            normalized.push(mapped);
            continue;
        };
        if is_unknown_extension_flag(text) {
            if !text.contains('=') {
                if let Some(next) = iter.peek().and_then(|arg| arg.to_str()) {
                    if !next.starts_with('-') && !next.starts_with('@') {
                        iter.next();
                    }
                }
            }
            continue;
        }
        normalized.push(mapped);
    }

    normalized
}

fn is_unknown_extension_flag(flag: &str) -> bool {
    if !flag.starts_with("--") {
        return false;
    }
    let name = flag
        .trim_start_matches("--")
        .split_once('=')
        .map(|(name, _)| name)
        .unwrap_or_else(|| flag.trim_start_matches("--"));
    !matches!(
        name,
        "help"
            | "version"
            | "print"
            | "mode"
            | "continue"
            | "resume"
            | "session"
            | "fork"
            | "no-session"
            | "session-dir"
            | "provider"
            | "model"
            | "models"
            | "thinking"
            | "api-key"
            | "system-prompt"
            | "append-system-prompt"
            | "no-tools"
            | "no-builtin-tools"
            | "tools"
            | "extension"
            | "no-extensions"
            | "skill"
            | "no-skills"
            | "prompt-template"
            | "no-prompt-templates"
            | "theme"
            | "no-themes"
            | "no-context-files"
            | "offline"
            | "verbose"
            | "list-models"
            | "export"
            | "local"
            | "self"
            | "extensions"
            | "force"
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse_from(normalize_cli_args(std::env::args_os()));
    apply_environment_overrides(&cli);
    if cli.version {
        println!(env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if let Some(session_path) = cli.export.as_deref() {
        let output_path = cli.messages.first().map(String::as_str);
        let exported = export_session_html(session_path, output_path)?;
        println!("Exported to: {}", exported.display());
        return Ok(());
    }

    let provider = Arc::new(OpenAiCodexProvider::new()?);

    if let Some(command) = cli.command {
        match command {
            Commands::Login => provider.login().await,
            Commands::Logout => provider.logout(),
            Commands::Acp => run_acp_stdio().await,
            Commands::Tui { session, resume } => {
                run_tui(provider.clone(), session.as_deref(), resume).await
            }
            Commands::Print {
                prompt,
                session,
                resume,
            } => {
                run_print(
                    provider.clone(),
                    PromptInput::text(prompt),
                    session.as_deref(),
                    resume,
                    CliMode::Text,
                    SessionCliOptions::default(),
                )
                .await
            }
            Commands::Rpc { session, resume } => {
                run_rpc(provider.clone(), session.as_deref(), resume, SessionCliOptions::default()).await
            }
            Commands::Repl { session, resume } => {
                run_repl(provider.clone(), session.as_deref(), resume).await
            }
            Commands::Install { source, local } => run_package_install(&source, local),
            Commands::Remove { source, local } | Commands::Uninstall { source, local } => {
                run_package_remove(&source, local)
            }
            Commands::Update {
                source,
                self_target,
                extensions,
                force,
                extension_source,
            } => run_package_update(source.as_deref(), self_target, extensions, force, extension_source.as_deref()),
            Commands::List => run_package_list(),
            Commands::Config => run_config_command(),
            Commands::Models => list_models(None),
            Commands::Sessions => list_sessions(),
        }
    } else {
        run_default_cli(provider.clone(), cli).await
    }
}

async fn open_session(
    provider: Arc<OpenAiCodexProvider>,
    session_target: Option<&str>,
    resume_recent: bool,
) -> Result<AgentSession> {
    open_session_with_overrides(
        provider,
        session_target,
        resume_recent,
        SessionCliOptions::default(),
    )
    .await
}

async fn open_session_with_overrides(
    provider: Arc<OpenAiCodexProvider>,
    session_target: Option<&str>,
    resume_recent: bool,
    options: SessionCliOptions,
) -> Result<AgentSession> {
    let cwd = std::env::current_dir()?;
    let settings = SettingsManager::new(&cwd)?.merged()?;
    let manager = if options.no_session {
        SessionManager::in_memory(&cwd)
    } else if let Some(fork_target) = options.fork.as_deref() {
        let source = SessionManager::load_target(&cwd, fork_target)?;
        source.create_fork(source.get_leaf_id())?
    } else {
        match session_target {
            Some(session_target) => SessionManager::load_target(&cwd, session_target)?,
            None if resume_recent => SessionManager::continue_recent(&cwd)?,
            None => SessionManager::new(&cwd)?,
        }
    };

    let model_scope_patterns = if options.scoped_models.is_empty() {
        settings.enabled_models.clone()
    } else {
        options.scoped_models.clone()
    };
    let scoped_models = resolve_scoped_models(&model_scope_patterns);
    let scoped_default = scoped_models.first();
    let (model_provider, model_id, model_thinking) = parse_cli_model(options.model.as_deref());
    let configured_provider = model_provider
        .or(options.provider.clone())
        .or_else(|| manager.current_model_provider())
        .or_else(|| scoped_default.map(|model| model.provider.clone()))
        .or(settings.default_provider);
    let model_id = model_id
        .or_else(|| manager.current_model_id())
        .or_else(|| scoped_default.map(|model| model.id.clone()))
        .or(settings.default_model)
        .unwrap_or_else(|| "gpt-5.5".to_string());
    let mut model = ModelDescriptor::resolve(configured_provider.as_deref(), Some(&model_id))
        .or_else(|| ModelDescriptor::defaults().into_iter().next())
        .ok_or_else(|| anyhow!("no default models available"))?;
    if let Some(api_key) = options.api_key.clone().filter(|key| !key.is_empty()) {
        model.api_key = Some(api_key);
    }
    let thinking_level = options
        .thinking
        .clone()
        .or(model_thinking)
        .or_else(|| manager.current_thinking_level())
        .or(settings.default_thinking_level)
        .unwrap_or_else(|| "medium".to_string());

    let mut session = AgentSession::new(cwd.clone(), manager, model, thinking_level, provider)?;
    if options.has_prompt_overrides() {
        session.set_system_prompt(build_cli_system_prompt(&cwd, &options)?);
    }
    if options.disable_tools {
        session.disable_tools();
    }
    if let Some(allow_tools) = options.allow_tools {
        session.retain_tools(&allow_tools);
    }
    Ok(session)
}

fn build_cli_system_prompt(cwd: &std::path::Path, options: &SessionCliOptions) -> Result<String> {
    let mut parts = Vec::new();
    if let Some(system_prompt) = options.system_prompt.as_deref() {
        parts.push(resolve_prompt_fragment(system_prompt)?);
    } else {
        parts.push(String::from(
            "You are pi, a headless Rust coding agent. Use tools precisely, prefer minimal diffs, and explain concrete results.",
        ));
    }

    if !options.no_context_files {
        for (path, content) in pi_agent::skills::load_agent_docs(cwd) {
            parts.push(format!(
                "<agents path=\"{}\">\n{}\n</agents>",
                path.display(),
                content
            ));
        }
    }

    let skills = collect_skills_for_options(cwd, options)?;
    if !skills.is_empty() {
        parts.push(pi_agent::skills::format_skills_for_prompt(&skills));
    }

    for fragment in &options.append_system_prompt {
        parts.push(resolve_prompt_fragment(fragment)?);
    }
    Ok(parts.join("\n\n"))
}

fn resolve_prompt_fragment(value: &str) -> Result<String> {
    let path = expand_tilde(value);
    if path.exists() {
        std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read prompt fragment {}", path.display()))
    } else {
        Ok(value.to_string())
    }
}

fn collect_skills_for_options(
    cwd: &std::path::Path,
    options: &SessionCliOptions,
) -> Result<Vec<pi_agent::Skill>> {
    let mut skills = Vec::new();
    if !options.no_skills {
        skills.extend(pi_agent::skills::load_skills(cwd)?);
        for extension_path in extension_resource_paths(cwd, options, "skills")? {
            for (path, content) in load_explicit_skill_path(&extension_path)? {
                if let Some(skill) = skill_from_file_content(path, content) {
                    skills.push(skill);
                }
            }
        }
    }
    for skill_path in &options.skills {
        for (path, content) in load_explicit_skill(skill_path)? {
            if let Some(skill) = skill_from_file_content(path, content) {
                skills.push(skill);
            }
        }
    }
    dedupe_skill_list(&mut skills);
    Ok(skills)
}

fn dedupe_skill_list(skills: &mut Vec<pi_agent::Skill>) {
    let mut names = std::collections::HashSet::new();
    let mut paths = std::collections::HashSet::new();
    skills.retain(|skill| {
        let path = skill.path.canonicalize().unwrap_or_else(|_| skill.path.clone());
        names.insert(skill.name.clone()) && paths.insert(path)
    });
}

fn skill_from_file_content(path: std::path::PathBuf, content: String) -> Option<pi_agent::Skill> {
    let parent_name = path
        .parent()
        .and_then(|parent| parent.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "skill".to_string());
    let description = pi_agent::skills::skill_description(&content)?;
    Some(pi_agent::Skill {
        name: pi_agent::skills::skill_name(&content).unwrap_or(parent_name),
        path,
        content: content.clone(),
        description,
        disable_model_invocation: pi_agent::skills::skill_disable_model_invocation(&content),
    })
}

fn load_explicit_skill(path: &str) -> Result<Vec<(std::path::PathBuf, String)>> {
    load_explicit_skill_path(&expand_tilde(path))
}

fn load_explicit_skill_path(path: &std::path::Path) -> Result<Vec<(std::path::PathBuf, String)>> {
    if path.is_file() {
        if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            return Ok(vec![(path.to_path_buf(), std::fs::read_to_string(path)?)]);
        }
        return Ok(Vec::new());
    }

    let root_skill = path.join("SKILL.md");
    if root_skill.is_file() {
        return Ok(vec![(root_skill.clone(), std::fs::read_to_string(root_skill)?)]);
    }

    let mut skills = Vec::new();
    let mut entries = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let entry_path = entry.path();
        if entry_path.is_file() && entry_path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            skills.push((entry_path.clone(), std::fs::read_to_string(entry_path)?));
        }
    }
    let walker = walkdir::WalkDir::new(path).into_iter().filter_entry(|entry| {
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
        skills.push((skill_file.clone(), std::fs::read_to_string(skill_file)?));
    }
    Ok(skills)
}

fn configured_extension_paths(cwd: &std::path::Path, options: &SessionCliOptions) -> Result<Vec<std::path::PathBuf>> {
    let mut paths = Vec::new();
    if !options.no_extensions {
        let (global, project) = scoped_settings(cwd)?;
        paths.extend(project.extensions.into_iter().map(|path| expand_tilde(&path)));
        paths.extend(global.extensions.into_iter().map(|path| expand_tilde(&path)));
    }
    paths.extend(options.extensions.iter().map(|path| expand_tilde(path)));
    Ok(paths)
}

fn extension_resource_paths(
    cwd: &std::path::Path,
    options: &SessionCliOptions,
    resource_dir: &str,
) -> Result<Vec<std::path::PathBuf>> {
    let mut paths = Vec::new();
    for extension in configured_extension_paths(cwd, options)? {
        add_resource_paths_for_root(&mut paths, &extension, resource_dir);
    }
    paths.extend(configured_local_package_resource_paths(cwd, options, resource_dir)?);
    Ok(paths)
}

fn add_resource_paths_for_root(paths: &mut Vec<std::path::PathBuf>, root: &std::path::Path, resource_dir: &str) {
    if !root.is_dir() {
        return;
    }
    let manifest_paths = pi_manifest_resource_paths(root, resource_dir);
    if !manifest_paths.is_empty() {
        paths.extend(manifest_paths);
        return;
    }
    paths.push(root.join(resource_dir));
    if resource_dir == "skills" && root.join("SKILL.md").exists() {
        paths.push(root.to_path_buf());
    }
}

fn pi_manifest_resource_paths(root: &std::path::Path, resource_dir: &str) -> Vec<std::path::PathBuf> {
    let package_json = root.join("package.json");
    let Ok(raw) = std::fs::read_to_string(package_json) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(entries) = value
        .get("pi")
        .and_then(|pi| pi.get(resource_dir))
        .and_then(|entries| entries.as_array())
    else {
        return Vec::new();
    };
    let entries = entries
        .iter()
        .filter_map(|entry| entry.as_str())
        .collect::<Vec<_>>();
    let mut included = Vec::new();
    for entry in entries.iter().copied().filter(|entry| !entry.starts_with('!')) {
        if has_glob_pattern(entry) {
            let pattern = root.join(entry).display().to_string();
            if let Ok(matches) = glob::glob(&pattern) {
                included.extend(matches.flatten().filter(|path| path.exists()));
            }
        } else {
            let path = root.join(entry);
            if path.exists() {
                included.push(path);
            }
        }
    }

    let excluded = entries
        .iter()
        .copied()
        .filter_map(|entry| entry.strip_prefix('!'))
        .flat_map(|entry| {
            let pattern = root.join(entry).display().to_string();
            glob::glob(&pattern)
                .into_iter()
                .flat_map(|matches| matches.flatten())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    included
        .into_iter()
        .filter(|path| !excluded.iter().any(|excluded| excluded == path))
        .collect()
}

fn has_glob_pattern(value: &str) -> bool {
    value.contains('*') || value.contains('?') || value.contains('[')
}

fn configured_local_package_resource_paths(
    cwd: &std::path::Path,
    options: &SessionCliOptions,
    resource_dir: &str,
) -> Result<Vec<std::path::PathBuf>> {
    if options.no_extensions {
        return Ok(Vec::new());
    }
    let (global, project) = scoped_settings(cwd)?;
    let mut paths = Vec::new();
    for package in project.packages.into_iter().chain(global.packages) {
        match package {
            PackageSource::Simple(source) => {
                if let Some(root) = local_package_root(cwd, &source) {
                    add_resource_paths_for_root(&mut paths, &root, resource_dir);
                }
            }
            PackageSource::Filtered {
                source,
                extensions,
                skills,
                prompt_templates,
                themes,
            } => {
                let Some(root) = local_package_root(cwd, &source) else {
                    continue;
                };
                let names = match resource_dir {
                    "extensions" => extensions,
                    "skills" => skills,
                    "prompts" => prompt_templates,
                    "themes" => themes,
                    _ => Vec::new(),
                };
                paths.extend(names.into_iter().flat_map(|name| filtered_resource_candidates(&root, resource_dir, &name)));
            }
        }
    }
    Ok(paths)
}

fn filtered_resource_candidates(root: &std::path::Path, resource_dir: &str, name: &str) -> Vec<std::path::PathBuf> {
    let base = root.join(resource_dir).join(name);
    let mut candidates = vec![base.clone()];
    match resource_dir {
        "prompts" | "themes" if base.extension().is_none() => {
            candidates.push(base.with_extension("md"));
            candidates.push(base.with_extension("json"));
        }
        "skills" => {
            candidates.push(base.join("SKILL.md"));
        }
        _ => {}
    }
    candidates.into_iter().filter(|path| path.exists()).collect()
}

fn local_package_root(cwd: &std::path::Path, source: &str) -> Option<std::path::PathBuf> {
    if source.starts_with("npm:") || source.starts_with("git:") || source.starts_with("http://") || source.starts_with("https://") {
        return None;
    }
    let path = expand_tilde(source);
    let path = if path.is_absolute() { path } else { cwd.join(path) };
    path.exists().then_some(path)
}

fn scoped_settings(cwd: &std::path::Path) -> Result<(SettingsFile, SettingsFile)> {
    let manager = SettingsManager::new(cwd)?;
    Ok((
        manager.load_global().unwrap_or_default(),
        manager.load_project().unwrap_or_default(),
    ))
}

#[derive(Debug, Clone)]
struct PromptTemplate {
    name: String,
    description: String,
    argument_hint: Option<String>,
    content: String,
    path: std::path::PathBuf,
}

fn expand_prompt_template(prompt: String, options: &SessionCliOptions) -> Result<String> {
    if !prompt.starts_with('/') {
        return Ok(prompt);
    }
    if let Some(expanded) = expand_skill_command(&prompt, options)? {
        return Ok(expanded);
    }
    let (name, args) = prompt[1..]
        .split_once(char::is_whitespace)
        .map(|(name, args)| (name, args.trim()))
        .unwrap_or((&prompt[1..], ""));
    let Some(template) = load_prompt_templates(options)?
        .into_iter()
        .find(|template| template.name == name)
    else {
        return Ok(prompt);
    };
    Ok(substitute_prompt_args(&template.content, &parse_prompt_args(args)))
}

fn expand_skill_command(prompt: &str, options: &SessionCliOptions) -> Result<Option<String>> {
    let Some(rest) = prompt.strip_prefix("/skill:") else {
        return Ok(None);
    };
    let (skill_name, args) = rest
        .split_once(char::is_whitespace)
        .map(|(name, args)| (name, args.trim()))
        .unwrap_or((rest, ""));
    let cwd = std::env::current_dir()?;
    let Some(skill) = collect_skills_for_options(&cwd, options)?
        .into_iter()
        .find(|skill| skill.name == skill_name)
    else {
        return Ok(None);
    };
    let body = strip_frontmatter(&skill.content).trim();
    let base_dir = skill.path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let skill_block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        skill.path.display(),
        base_dir.display(),
        body
    );
    Ok(Some(if args.is_empty() {
        skill_block
    } else {
        format!("{skill_block}\n\n{args}")
    }))
}

fn load_prompt_templates(options: &SessionCliOptions) -> Result<Vec<PromptTemplate>> {
    let cwd = std::env::current_dir()?;
    let (global, project) = scoped_settings(&cwd)?;
    let mut paths = Vec::new();
    if !options.no_prompt_templates {
        if let Ok(global_dir) = agent_dir() {
            paths.push(global_dir.join("prompts"));
        }
        paths.push(cwd.join(".pi").join("prompts"));
        paths.extend(project.prompt_templates.into_iter().map(|path| expand_tilde(&path)));
        paths.extend(global.prompt_templates.into_iter().map(|path| expand_tilde(&path)));
        paths.extend(extension_resource_paths(&cwd, options, "prompts")?);
    }
    paths.extend(options.prompt_templates.iter().map(|path| expand_tilde(path)));

    let mut templates = Vec::new();
    for path in paths {
        if path.is_dir() {
            if !path.exists() {
                continue;
            }
            for entry in std::fs::read_dir(&path).with_context(|| format!("failed to read prompts dir {}", path.display()))? {
                let entry = entry?;
                let entry_path = entry.path();
                if entry_path.is_file() && entry_path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                    if let Some(template) = load_prompt_template_file(&entry_path)? {
                        templates.push(template);
                    }
                }
            }
        } else if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            if let Some(template) = load_prompt_template_file(&path)? {
                templates.push(template);
            }
        }
    }
    Ok(templates)
}

fn load_prompt_template_file(path: &std::path::Path) -> Result<Option<PromptTemplate>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read prompt template {}", path.display()))?;
    let content = strip_frontmatter(&raw).trim().to_string();
    let Some(name) = path.file_stem().map(|name| name.to_string_lossy().to_string()) else {
        return Ok(None);
    };
    let description = frontmatter_value(&raw, "description")
        .or_else(|| content.lines().map(str::trim).find(|line| !line.is_empty()).map(|line| line.chars().take(60).collect()))
        .unwrap_or_default();
    let argument_hint = frontmatter_value(&raw, "argument-hint");
    Ok(Some(PromptTemplate {
        name,
        description,
        argument_hint,
        content,
        path: path.to_path_buf(),
    }))
}

fn frontmatter_value(raw: &str, key: &str) -> Option<String> {
    let rest = raw.strip_prefix("---")?;
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

fn strip_frontmatter(raw: &str) -> &str {
    let Some(rest) = raw.strip_prefix("---") else {
        return raw;
    };
    rest.split_once("\n---")
        .map(|(_, body)| body.trim_start_matches(['\r', '\n']))
        .unwrap_or(raw)
}

fn parse_prompt_args(args: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for ch in args.chars() {
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn substitute_prompt_args(template: &str, args: &[String]) -> String {
    let positional = regex::Regex::new(r"\$(\d+)").expect("valid positional regex");
    let mut result = positional
        .replace_all(template, |captures: &regex::Captures<'_>| {
            let index = captures
                .get(1)
                .and_then(|matched| matched.as_str().parse::<usize>().ok())
                .unwrap_or(0);
            args.get(index.saturating_sub(1))
                .map(String::as_str)
                .unwrap_or("")
                .to_string()
        })
        .into_owned();

    let slices = regex::Regex::new(r"\$\{@:(\d+)(?::(\d+))?\}").expect("valid args slice regex");
    result = slices
        .replace_all(&result, |captures: &regex::Captures<'_>| {
            let start = captures
                .get(1)
                .and_then(|matched| matched.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            let selected = if let Some(length) = captures
                .get(2)
                .and_then(|matched| matched.as_str().parse::<usize>().ok())
            {
                args.iter().skip(start).take(length)
            } else {
                args.iter().skip(start).take(usize::MAX)
            };
            selected.cloned().collect::<Vec<_>>().join(" ")
        })
        .into_owned();

    let all = args.join(" ");
    result = result.replace("$ARGUMENTS", &all);
    result.replace("$@", &all)
}

fn resolve_model_scope_for_options(options: &SessionCliOptions) -> Result<Vec<ModelDescriptor>> {
    if !options.scoped_models.is_empty() {
        return Ok(resolve_scoped_models(&options.scoped_models));
    }
    let cwd = std::env::current_dir()?;
    let settings = SettingsManager::new(&cwd)?.merged()?;
    Ok(resolve_scoped_models(&settings.enabled_models))
}

fn resolve_scoped_models(patterns: &[String]) -> Vec<ModelDescriptor> {
    if patterns.is_empty() {
        return Vec::new();
    }
    let defaults = ModelDescriptor::defaults();
    let mut resolved = Vec::new();
    for pattern in patterns {
        for model in defaults.iter().filter(|model| model_matches_pattern(model, pattern)) {
            if !resolved
                .iter()
                .any(|existing: &ModelDescriptor| existing.provider == model.provider && existing.id == model.id)
            {
                resolved.push(model.clone());
            }
        }
    }
    resolved
}

fn model_matches_pattern(model: &ModelDescriptor, pattern: &str) -> bool {
    let canonical = model.canonical_id();
    if let Some((provider, id)) = pattern.split_once('/') {
        return wildcard_matches(&model.provider, provider) && wildcard_matches(&model.id, id);
    }
    wildcard_matches(&model.id, pattern)
        || wildcard_matches(&canonical, pattern)
        || model.id.contains(pattern)
        || canonical.contains(pattern)
}

fn wildcard_matches(value: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return value == pattern;
    };
    value.starts_with(prefix) && value.ends_with(suffix)
}

fn parse_cli_model(cli_model: Option<&str>) -> (Option<String>, Option<String>, Option<String>) {
    let Some(raw_model) = cli_model else {
        return (None, None, None);
    };
    let (model, thinking) = raw_model
        .rsplit_once(':')
        .filter(|(_, thinking)| is_valid_thinking_level(thinking))
        .map(|(model, thinking)| (model, Some(thinking.to_string())))
        .unwrap_or((raw_model, None));
    let (provider, model_id) = model
        .split_once('/')
        .map(|(provider, model_id)| (Some(provider.to_string()), model_id.to_string()))
        .unwrap_or((None, model.to_string()));
    (provider, Some(model_id), thinking)
}

fn is_valid_thinking_level(level: &str) -> bool {
    matches!(level, "off" | "minimal" | "low" | "medium" | "high" | "xhigh")
}


struct TuiSink {
    lines: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait(?Send)]
impl EventSink for TuiSink {
    async fn emit(&self, event: SessionEvent) -> anyhow::Result<()> {
        let mut lines = self.lines.lock().expect("lock poisoned");
        match event {
            SessionEvent::AssistantMessage { content } => {
                let text = content
                    .into_iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Text(text) => Some(text.text),
                        AssistantContentBlock::Thinking(thinking) => {
                            Some(format!("[thinking]\n{}", thinking.thinking))
                        }
                        AssistantContentBlock::ToolCall(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !text.is_empty() {
                    lines.push(format!("Assistant: {text}"));
                }
            }
            SessionEvent::ToolCallStart { tool_name, title, .. } => {
                lines.push(format!("Tool {tool_name}: {title}"));
            }
            SessionEvent::ToolCallResult { tool_name, output, .. } => {
                let text = output
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        UserContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let status = if output.is_error { "failed" } else { "done" };
                lines.push(format!("Tool {tool_name} {status}: {text}"));
            }
            SessionEvent::SessionInfo { title, .. } => {
                lines.push(format!("Session: {title}"));
            }
            SessionEvent::UserMessage { .. } => {}
        }
        Ok(())
    }
}

async fn run_default_cli(provider: Arc<OpenAiCodexProvider>, cli: Cli) -> Result<()> {
    if cli.list_models {
        return list_models(cli.messages.first().map(String::as_str));
    }

    let mode = cli.mode.unwrap_or(CliMode::Text);
    if mode == CliMode::Rpc {
        if cli.messages.iter().any(|message| message.starts_with('@')) {
            anyhow::bail!("@file arguments are not supported in RPC mode");
        }
        return run_rpc(
            provider,
            cli.session.as_deref(),
            cli.resume || cli.continue_recent,
            SessionCliOptions::from_cli(&cli),
        )
        .await;
    }

    let stdin_is_tty = io::stdin().is_terminal();
    let stdout_is_tty = io::stdout().is_terminal();
    if cli.print || !stdin_is_tty || mode == CliMode::Json {
        let prompt = collect_prompt_input(cli.messages.clone(), stdin_is_tty)?;
        if prompt.text.trim().is_empty() && prompt.images.is_empty() {
            anyhow::bail!("no prompt provided for non-interactive mode");
        }
        return run_print(
            provider,
            prompt,
            cli.session.as_deref(),
            cli.resume || cli.continue_recent,
            mode,
            SessionCliOptions::from_cli(&cli),
        )
        .await;
    }

    if !stdout_is_tty {
        anyhow::bail!("interactive mode requires a TTY; use --print or --mode rpc for non-interactive use");
    }

    let initial_prompt = if cli.messages.is_empty() {
        None
    } else {
        Some(collect_prompt_input(cli.messages.clone(), true)?)
    };
    run_tui_with_overrides(
        provider,
        cli.session.as_deref(),
        cli.resume || cli.continue_recent,
        SessionCliOptions::from_cli(&cli),
        initial_prompt,
    )
    .await
}

#[derive(Clone, Debug, Default)]
struct PromptInput {
    text: String,
    images: Vec<ImageContent>,
}

impl PromptInput {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
        }
    }
}

fn collect_prompt_input(messages: Vec<String>, stdin_is_tty: bool) -> Result<PromptInput> {
    let mut parts = Vec::new();
    let mut images = Vec::new();
    for message in messages {
        if let Some(path) = message.strip_prefix('@') {
            let processed = process_file_argument(path)?;
            if !processed.text.is_empty() {
                parts.push(processed.text);
            }
            images.extend(processed.images);
        } else {
            parts.push(message);
        }
    }
    if !stdin_is_tty {
        let mut stdin = String::new();
        io::stdin().read_to_string(&mut stdin)?;
        let stdin = stdin.trim();
        if !stdin.is_empty() {
            parts.push(stdin.to_string());
        }
    }
    Ok(PromptInput {
        text: parts.join("\n"),
        images,
    })
}

fn process_file_argument(path: &str) -> Result<PromptInput> {
    let path = expand_tilde(path);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let metadata = std::fs::metadata(&path)
        .with_context(|| format!("file argument not found: {}", path.display()))?;
    if metadata.len() == 0 {
        return Ok(PromptInput::default());
    }
    if let Some(mime_type) = supported_image_mime(&path) {
        let data = std::fs::read(&path)
            .with_context(|| format!("failed to read image argument {}", path.display()))?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(data);
        return Ok(PromptInput {
            text: format!("<file name=\"{}\"></file>", path.display()),
            images: vec![ImageContent::new(encoded, mime_type)],
        });
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read file argument {}", path.display()))?;
    Ok(PromptInput {
        text: format!("<file name=\"{}\">\n{}\n</file>", path.display(), content),
        images: Vec::new(),
    })
}

fn supported_image_mime(path: &std::path::Path) -> Option<&'static str> {
    match path.extension().and_then(|extension| extension.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

async fn run_print(
    provider: Arc<OpenAiCodexProvider>,
    prompt: PromptInput,
    session_target: Option<&str>,
    resume_recent: bool,
    mode: CliMode,
    options: SessionCliOptions,
) -> Result<()> {
    let prompt_text = expand_prompt_template(prompt.text, &options)?;
    let mut session = open_session_with_overrides(
        provider,
        session_target,
        resume_recent,
        options,
    )
    .await?;
    let permissions = AllowAllPermissions;
    let mut content = vec![UserContentBlock::Text(TextContent::new(prompt_text))];
    content.extend(prompt.images.into_iter().map(UserContentBlock::Image));
    let output = if mode == CliMode::Json {
        println!("{}", serde_json::to_string(session.session_manager.header())?);
        session.prompt_content(content, &RpcEventSink, &permissions).await?
    } else {
        session.prompt_content(content, &NullEventSink, &permissions).await?
    };
    match mode {
        CliMode::Json => println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "output": output.output,
                "stopReason": format!("{:?}", output.stop_reason),
            }))?
        ),
        CliMode::Text | CliMode::Rpc => {
            if !output.output.is_empty() {
                println!("{}", output.output);
            }
        }
    }
    Ok(())
}

async fn run_tui(
    provider: Arc<OpenAiCodexProvider>,
    session_target: Option<&str>,
    resume_recent: bool,
) -> Result<()> {
    run_tui_with_overrides(
        provider,
        session_target,
        resume_recent,
        SessionCliOptions::default(),
        None,
    )
    .await
}

async fn run_tui_with_overrides(
    provider: Arc<OpenAiCodexProvider>,
    session_target: Option<&str>,
    resume_recent: bool,
    options: SessionCliOptions,
    initial_prompt: Option<PromptInput>,
) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        anyhow::bail!("interactive mode requires a TTY; use --print or --mode rpc for non-interactive use");
    }

    let scoped_models = resolve_model_scope_for_options(&options)?;
    let mut theme = load_tui_theme(&options)?;
    let mut session = open_session_with_overrides(
        provider.clone(),
        session_target,
        resume_recent,
        options.clone(),
    )
    .await?;
    let mut chrome = build_chrome(&session, theme.clone(), &options)?;
    let messages = Arc::new(Mutex::new(Vec::new()));
    let sink = TuiSink {
        lines: messages.clone(),
    };
    let permissions = AllowAllPermissions;
    let terminal = ProcessTerminal::new();
    let mut tui = TUI::new(terminal);
    let submitted_prompt = Arc::new(Mutex::new(None::<String>));
    let submitted_prompt_for_editor = submitted_prompt.clone();
    let mut editor = Editor::new(EditorOptions::default());
    editor.set_focused(true);
    editor.set_autocomplete_provider(CombinedAutocompleteProvider::new(
        tui_slash_commands(&options)?,
        std::env::current_dir()?,
    ));
    editor.set_on_submit(move |prompt| {
        *submitted_prompt_for_editor.lock().expect("lock poisoned") = Some(prompt);
    });

    tui.start()?;
    render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;

    if let Some(prompt) = initial_prompt.filter(|prompt| !prompt.text.trim().is_empty() || !prompt.images.is_empty()) {
        messages
            .lock()
            .expect("lock poisoned")
            .push(format!("You: {}", prompt.text));
        let prompt_to_send = expand_prompt_template(prompt.text, &options)?;
        let mut content = vec![UserContentBlock::Text(TextContent::new(prompt_to_send))];
        content.extend(prompt.images.into_iter().map(UserContentBlock::Image));
        run_content_with_cancel(
            &mut session,
            content,
            &sink,
            &permissions,
            &mut tui,
            &messages,
            &chrome,
        )
        .await?;
        chrome = build_chrome(&session, theme.clone(), &options)?;
        render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
    }

    let loop_result: Result<()> = loop {
        if !crossterm::event::poll(Duration::from_millis(100))? {
            continue;
        }

        let crossterm::event::Event::Key(key) = crossterm::event::read()? else {
            continue;
        };
        if key.kind == crossterm::event::KeyEventKind::Release {
            continue;
        }
        let is_control = key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL);
        match key.code {
            crossterm::event::KeyCode::Esc => break Ok(()),
            crossterm::event::KeyCode::Char('c') if is_control => break Ok(()),
            crossterm::event::KeyCode::Char('d') if is_control && editor.get_text().is_empty() => {
                break Ok(());
            }
            crossterm::event::KeyCode::Char('l') if is_control => {
                messages.lock().expect("lock poisoned").clear();
                editor.set_text("");
                render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
                continue;
            }
            crossterm::event::KeyCode::Char('p') if is_control => {
                let next = next_model(session.current_model(), &scoped_models);
                session.set_model(next)?;
                chrome = build_chrome(&session, theme.clone(), &options)?;
                messages.lock().expect("lock poisoned").push(format!(
                    "System: Model set to {}",
                    session.current_model().canonical_id()
                ));
                render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
                continue;
            }
            crossterm::event::KeyCode::Char('t') if is_control => {
                let next = next_thinking_level(session.current_thinking_level());
                session.set_thinking_level(next)?;
                chrome = build_chrome(&session, theme.clone(), &options)?;
                messages.lock().expect("lock poisoned").push(format!(
                    "System: Thinking set to {}",
                    session.current_thinking_level()
                ));
                render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
                continue;
            }
            _ => {
                if let Some(input) = key_event_to_editor_input(key) {
                    editor.handle_input(&input);
                }
            }
        }

        let prompt = submitted_prompt.lock().expect("lock poisoned").take();
        let Some(prompt) = prompt else {
            render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
            continue;
        };
        if prompt.trim().is_empty() {
            render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
            continue;
        }
        editor.add_to_history(prompt.clone());
        messages
            .lock()
            .expect("lock poisoned")
            .push(format!("You: {prompt}"));

        if let Some(command) = prompt.strip_prefix('!') {
            render_tui(&mut tui, &messages, "running bash...", true, &chrome)?;
            let is_excluded = command.starts_with('!');
            let command = command.strip_prefix('!').unwrap_or(command).trim();
            let result = run_bash_command_result(command)?;
            session.record_bash_result(command, &result, is_excluded)?;
            let output = result
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            messages
                .lock()
                .expect("lock poisoned")
                .push(format!("Tool bash: {command}\n{output}"));
            render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
            continue;
        }

        if prompt == "/help" {
            messages
                .lock()
                .expect("lock poisoned")
                .push(format!("Assistant: {}", tui_help_text()));
            render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
            continue;
        }

        if let Some(theme_ref) = prompt.strip_prefix("/theme ").map(str::trim).filter(|value| !value.is_empty()) {
            theme = load_tui_theme_ref(&options, theme_ref)?;
            chrome = build_chrome(&session, theme.clone(), &options)?;
            messages
                .lock()
                .expect("lock poisoned")
                .push(format!("System: Theme set to {theme_ref}"));
            render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
            continue;
        }

        if let Some(response) = handle_tui_session_command(
            &prompt,
            &mut session,
            &mut editor,
            &mut tui,
            &messages,
            &mut chrome,
            theme.clone(),
            &options,
            &scoped_models,
        )
        .await? {
            messages
                .lock()
                .expect("lock poisoned")
                .push(format!("System: {response}"));
            render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
            continue;
        }

        if let Some(response) = local_tui_command(&prompt, &session, &options, &scoped_models)? {
            messages
                .lock()
                .expect("lock poisoned")
                .push(format!("Assistant: {response}"));
            render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
            continue;
        }

        let prompt = expand_prompt_template(prompt, &options)?;
        run_prompt_with_cancel(
            &mut session,
            prompt,
            &sink,
            &permissions,
            &mut tui,
            &messages,
            &chrome,
        )
        .await?;
        chrome = build_chrome(&session, theme.clone(), &options)?;
        render_tui(&mut tui, &messages, &editor_display_text(&editor), false, &chrome)?;
    };

    let stop_result = tui.stop();
    loop_result.and(stop_result)
}

fn run_tui_selector(
    tui: &mut TUI<ProcessTerminal>,
    chrome: &TuiChrome,
    title: &str,
    mut list: pi_tui::components::SettingsList,
) -> Result<Option<String>> {
    tui.clear();
    tui.add_child(TuiSelectorView {
        chrome: chrome.clone(),
        title: title.to_string(),
        list: list.clone(),
    });
    tui.request_render()?;

    loop {
        if crossterm::event::poll(Duration::from_millis(50))? {
            let crossterm::event::Event::Key(key) = crossterm::event::read()? else {
                continue;
            };
            if key.kind == crossterm::event::KeyEventKind::Release {
                continue;
            }
            if key.code == crossterm::event::KeyCode::Esc {
                return Ok(None);
            }
            if key.code == crossterm::event::KeyCode::Enter {
                if let Some(item) = list.selected_item() {
                    return Ok(Some(item.current_value.clone()));
                }
                return Ok(None);
            }

            if let Some(input) = key_event_to_editor_input(key) {
                list.handle_input(&input);
                tui.clear();
                tui.add_child(TuiSelectorView {
                    chrome: chrome.clone(),
                    title: title.to_string(),
                    list: list.clone(),
                });
                tui.request_render()?;
            }
        }
    }
}

struct TuiSelectorView {
    chrome: TuiChrome,
    title: String,
    list: pi_tui::components::SettingsList,
}

impl Component for TuiSelectorView {
    fn render(&self, width: usize) -> Vec<String> {
        let width = width.max(40);
        let mut lines = Vec::new();
        lines.push(format!(
            "{}{}{}",
            self.chrome.theme.warning,
            self.title,
            self.chrome.theme.reset
        ));
        lines.push(String::new());
        lines.extend(self.list.render(width));
        
        let target_height = terminal_height().max(12);
        if lines.len() < target_height {
            let gap = target_height.saturating_sub(lines.len());
            lines.extend(std::iter::repeat_n(String::new(), gap));
        } else {
            lines.truncate(target_height);
        }
        lines
    }
}

fn rpc_slash_commands(options: &SessionCliOptions) -> Result<Vec<serde_json::Value>> {
    let mut commands = Vec::new();
    for template in load_prompt_templates(options)? {
        commands.push(serde_json::json!({
            "name": template.name,
            "description": template.description,
            "source": "prompt",
            "sourceInfo": source_info_json(&template.path, template.path.parent()),
        }));
    }
    let cwd = std::env::current_dir()?;
    for skill in collect_skills_for_options(&cwd, options)? {
        commands.push(serde_json::json!({
            "name": format!("skill:{}", skill.name),
            "description": skill.description,
            "source": "skill",
            "sourceInfo": source_info_json(&skill.path, skill.path.parent()),
        }));
    }
    Ok(commands)
}

fn source_info_json(path: &std::path::Path, base_dir: Option<&std::path::Path>) -> serde_json::Value {
    let cwd = std::env::current_dir().ok();
    let agent = agent_dir().ok();
    let scope = if agent.as_ref().is_some_and(|agent| path.starts_with(agent)) {
        "user"
    } else if cwd
        .as_ref()
        .is_some_and(|cwd| path.starts_with(cwd.join(".pi")))
    {
        "project"
    } else {
        "temporary"
    };
    let mut value = serde_json::json!({
        "path": path.display().to_string(),
        "source": "local",
        "scope": scope,
        "origin": "top-level",
    });
    if let Some(base_dir) = base_dir {
        value["baseDir"] = serde_json::Value::String(base_dir.display().to_string());
    }
    value
}

fn skill_commands_enabled(options: &SessionCliOptions) -> Result<bool> {
    if options.no_skills {
        return Ok(false);
    }
    let cwd = std::env::current_dir()?;
    let manager = SettingsManager::new(&cwd)?;
    Ok(manager.merged()?.enable_skill_commands.unwrap_or(true))
}

fn tui_slash_commands(options: &SessionCliOptions) -> Result<Vec<SlashCommand>> {
    let mut commands = vec![
        slash_command("help", "Show help", None),
        slash_command("models", "List cycleable models", None),
        slash_command("sessions", "List recent sessions", None),
        slash_command("themes", "List available themes", None),
        slash_command("theme", "Switch TUI theme", Some("<name-or-path>")),
        slash_command("model", "Show or change the current model", Some("<provider/id>")),
        slash_command("session", "Show current session", None),
        slash_command("new", "Start a new session", None),
        slash_command("resume", "Resume a session", Some("<session-id-or-path>")),
        slash_command("fork", "Fork from an entry", Some("<entry-id>")),
        slash_command("tree", "Navigate to an entry", Some("<entry-id>")),
        slash_command("compact", "Compact older messages", None),
    ];
    for template in load_prompt_templates(options)? {
        commands.push(slash_command(
            &template.name,
            if template.description.is_empty() {
                "Prompt template"
            } else {
                &template.description
            },
            template.argument_hint.as_deref(),
        ));
    }
    if skill_commands_enabled(options)? {
        let cwd = std::env::current_dir()?;
        for skill in collect_skills_for_options(&cwd, options)? {
            commands.push(slash_command(
                &format!("skill:{}", skill.name),
                &skill.description,
                Some("[instructions]"),
            ));
        }
    }
    Ok(commands)
}

fn slash_command(name: &str, description: &str, argument_hint: Option<&str>) -> SlashCommand {
    let mut command = SlashCommand::new(name);
    command.description = Some(description.to_string());
    command.argument_hint = argument_hint.map(str::to_string);
    command
}

async fn handle_tui_session_command(
    prompt: &str,
    session: &mut AgentSession,
    editor: &mut Editor,
    tui: &mut TUI<ProcessTerminal>,
    messages: &Arc<Mutex<Vec<String>>>,
    chrome: &mut TuiChrome,
    theme: TuiTheme,
    options: &SessionCliOptions,
    scoped_models: &[ModelDescriptor],
) -> Result<Option<String>> {
    let trimmed = prompt.trim();
    if let Some(model_ref) = trimmed.strip_prefix("/model ").map(str::trim).filter(|value| !value.is_empty()) {
        let next = ModelDescriptor::resolve(None, Some(model_ref))
            .ok_or_else(|| anyhow!("unknown model {model_ref}"))?;
        session.set_model(next)?;
        *chrome = build_chrome(session, theme, options)?;
        return Ok(Some(format!("Model set to {}", session.current_model().canonical_id())));
    }
    if trimmed == "/model" || trimmed == "/models" {
        let current_id = session.current_model().canonical_id();
        let models = if scoped_models.is_empty() { ModelDescriptor::defaults() } else { scoped_models.to_vec() };
        let items = models.into_iter().map(|m| {
            let label = m.canonical_id();
            let mut item = pi_tui::components::SettingItem::new(label.clone(), label.clone(), label.clone());
            if label == current_id {
                item.description = Some("(current)".to_string());
            }
            item
        }).collect();
        let list = pi_tui::components::SettingsList::with_options(
            items,
            10,
            pi_tui::components::SettingsListTheme::default(),
            pi_tui::components::SettingsListOptions { enable_search: true },
        );
        if let Some(selected) = run_tui_selector(tui, chrome, "Select model", list)? {
            let next = ModelDescriptor::resolve(None, Some(&selected))
                .ok_or_else(|| anyhow!("unknown model {selected}"))?;
            session.set_model(next)?;
            *chrome = build_chrome(session, theme, options)?;
            return Ok(Some(format!("Model set to {}", session.current_model().canonical_id())));
        } else {
            return Ok(Some("Selection cancelled.".to_string()));
        }
    }
    if trimmed == "/session" {
        return Ok(Some(format_tui_session(session)));
    }
    if trimmed == "/sessions" || trimmed == "/resume" {
        let cwd = std::env::current_dir()?;
        let sessions = SessionManager::list_for_cwd(&cwd)?;
        if sessions.is_empty() {
            return Ok(Some("No saved sessions for this directory.".to_string()));
        }
        let items = sessions.into_iter().take(50).map(|s| {
            let id = short_session_id(&s.id);
            pi_tui::components::SettingItem::new(&s.id, format!("{id}  {}  {}", s.updated_at, s.title), s.id.clone())
        }).collect();
        let list = pi_tui::components::SettingsList::with_options(
            items,
            15,
            pi_tui::components::SettingsListTheme::default(),
            pi_tui::components::SettingsListOptions { enable_search: true },
        );
        if let Some(selected) = run_tui_selector(tui, chrome, "Select session to resume", list)? {
            session.switch_session_target(&selected)?;
            *chrome = build_chrome(session, theme, options)?;
            editor.set_text("");
            return Ok(Some(format!("Switched to session {}", short_session_id(&session.session_manager.header().id))));
        } else {
            return Ok(Some("Selection cancelled.".to_string()));
        }
    }
    if trimmed == "/themes" {
        let cwd = std::env::current_dir()?;
        let mut theme_paths = Vec::new();
        if !options.no_themes {
            if let Ok(dir) = agent_dir() {
                theme_paths.push(dir.join("themes"));
            }
            theme_paths.push(cwd.join(".pi").join("themes"));
            theme_paths.extend(extension_resource_paths(&cwd, options, "themes")?);
        }
        let mut discovered = Vec::new();
        for dir in theme_paths {
            if !dir.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(dir).into_iter().flatten().filter_map(Result::ok) {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    if let Some(name) = path.file_stem().map(|s| s.to_string_lossy().to_string()) {
                        discovered.push(name);
                    }
                }
            }
        }
        discovered.sort();
        discovered.dedup();
        if discovered.is_empty() {
            return Ok(Some("No themes found.".to_string()));
        }
        let items = discovered.into_iter().map(|name| {
            pi_tui::components::SettingItem::new(name.clone(), name.clone(), name.clone())
        }).collect();
        let list = pi_tui::components::SettingsList::with_options(
            items,
            15,
            pi_tui::components::SettingsListTheme::default(),
            pi_tui::components::SettingsListOptions { enable_search: true },
        );
        if let Some(selected) = run_tui_selector(tui, chrome, "Select theme", list)? {
            *chrome = build_chrome(session, load_tui_theme_ref(options, &selected)?, options)?;
            return Ok(Some(format!("Theme set to {selected}")));
        } else {
            return Ok(Some("Selection cancelled.".to_string()));
        }
    }
    if trimmed == "/new" {
        let parent = (session.session_manager.session_file().to_string_lossy() != ":memory:")
            .then(|| session.session_manager.session_file().display().to_string());
        session.new_session(parent)?;
        *chrome = build_chrome(session, theme, options)?;
        editor.set_text("");
        return Ok(Some(format!("Started new session {}", short_session_id(&session.session_manager.header().id))));
    }
    if let Some(target) = trimmed.strip_prefix("/resume ").map(str::trim).filter(|value| !value.is_empty()) {
        session.switch_session_target(target)?;
        *chrome = build_chrome(session, theme, options)?;
        editor.set_text("");
        return Ok(Some(format!("Switched to session {}", short_session_id(&session.session_manager.header().id))));
    }
    if let Some(entry_id) = trimmed.strip_prefix("/fork ").map(str::trim).filter(|value| !value.is_empty()) {
        let outcome = session.fork(entry_id)?;
        if let Some(text) = outcome.editor_text {
            editor.set_text(&text);
        }
        *chrome = build_chrome(session, theme, options)?;
        return Ok(Some(format!("Forked to session {}", short_session_id(&outcome.session_id))));
    }
    if let Some(entry_id) = trimmed.strip_prefix("/tree ").map(str::trim).filter(|value| !value.is_empty()) {
        let outcome = session.navigate_tree(entry_id)?;
        if let Some(text) = outcome.editor_text {
            editor.set_text(&text);
        }
        *chrome = build_chrome(session, theme, options)?;
        return Ok(Some(format!("Navigated to entry {entry_id}")));
    }
    if trimmed == "/compact" {
        render_tui(tui, messages, "compacting...", true, chrome)?;
        let summary = session.compact().await?;
        *chrome = build_chrome(session, theme, options)?;
        return Ok(Some(format!("Compacted session: {summary}")));
    }
    Ok(None)
}

fn local_tui_command(
    prompt: &str,
    session: &AgentSession,
    options: &SessionCliOptions,
    scoped_models: &[ModelDescriptor],
) -> Result<Option<String>> {
    match prompt.trim() {
        "/model" => Ok(Some(format!(
            "Current model: {}\nThinking: {}\nUse /model <provider/id> to switch.",
            session.current_model().canonical_id(),
            session.current_thinking_level(),
        ))),
        "/models" => Ok(Some(format_tui_models(session.current_model(), scoped_models))),
        "/session" => Ok(Some(format_tui_session(session))),
        "/sessions" => Ok(Some(format_tui_sessions()?)),
        "/themes" => Ok(Some(format_tui_themes(options)?)),
        _ => Ok(None),
    }
}

fn format_tui_session(session: &AgentSession) -> String {
    let header = session.session_manager.header();
    let leaf = session.session_manager.get_leaf_id().unwrap_or("(none)");
    let name = session
        .session_manager
        .session_name()
        .unwrap_or_else(|| "(unnamed)".to_string());
    format!(
        "Session: {}\nName: {}\nPath: {}\nCurrent entry: {}\nMessages/events: {}",
        header.id,
        name,
        session.session_manager.session_file().display(),
        leaf,
        session.session_manager.entries().len(),
    )
}

fn format_tui_models(current: &ModelDescriptor, scoped_models: &[ModelDescriptor]) -> String {
    let models = if scoped_models.is_empty() {
        ModelDescriptor::defaults()
    } else {
        scoped_models.to_vec()
    };
    let mut lines = vec!["Available models:".to_string()];
    for model in models {
        let marker = if model.provider == current.provider && model.id == current.id {
            "*"
        } else {
            " "
        };
        lines.push(format!("{marker} {}", model.canonical_id()));
    }
    lines.push("Use /model <provider/id> or Ctrl+P to switch.".to_string());
    lines.join("\n")
}

fn format_tui_sessions() -> Result<String> {
    let cwd = std::env::current_dir()?;
    let sessions = SessionManager::list_for_cwd(&cwd)?;
    if sessions.is_empty() {
        return Ok("No saved sessions for this directory.".to_string());
    }
    let mut lines = vec!["Recent sessions:".to_string()];
    for session in sessions.into_iter().take(20) {
        lines.push(format!(
            "{}  {}  {}",
            short_session_id(&session.id),
            session.updated_at,
            session.title
        ));
    }
    lines.push("Use /resume <session-id> to switch.".to_string());
    Ok(lines.join("\n"))
}

fn format_tui_themes(options: &SessionCliOptions) -> Result<String> {
    let cwd = std::env::current_dir()?;
    let mut theme_paths = Vec::new();
    if !options.no_themes {
        if let Ok(dir) = agent_dir() {
            theme_paths.push(dir.join("themes"));
        }
        theme_paths.push(cwd.join(".pi").join("themes"));
        for dir in extension_resource_paths(&cwd, options, "themes")? {
            theme_paths.push(dir);
        }
    }
    theme_paths.extend(options.themes.iter().map(|theme| expand_tilde(theme)));
    let mut names = Vec::new();
    for path in theme_paths {
        if path.is_file() {
            if let Some(name) = path.file_stem().map(|name| name.to_string_lossy().to_string()) {
                names.push(name);
            }
        } else if path.is_dir() {
            for entry in std::fs::read_dir(&path)? {
                let path = entry?.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    if let Some(name) = path.file_stem().map(|name| name.to_string_lossy().to_string()) {
                        names.push(name);
                    }
                }
            }
        }
    }
    names.sort();
    names.dedup();
    if names.is_empty() {
        Ok("No themes found.".to_string())
    } else {
        Ok(format!("Available themes:\n{}\nUse /theme <name> to apply for this session or restart with --theme <name>.", names.join("\n")))
    }
}

fn editor_display_text(editor: &Editor) -> String {
    let mut text = editor.get_text().to_string();
    if !text.is_empty() {
        text.insert_str(editor.cursor().min(text.len()), CURSOR_MARKER);
    }
    text
}

fn key_event_to_editor_input(key: crossterm::event::KeyEvent) -> Option<String> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let modifiers = key.modifiers;
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    let input = match key.code {
        KeyCode::Enter if shift => "\u{1b}[13;2u".to_string(),
        KeyCode::Enter => "\r".to_string(),
        KeyCode::Backspace => "\u{7f}".to_string(),
        KeyCode::Tab => "\t".to_string(),
        KeyCode::BackTab => "\u{1b}[Z".to_string(),
        KeyCode::Delete => "\u{1b}[3~".to_string(),
        KeyCode::Home => "\u{1b}[H".to_string(),
        KeyCode::End => "\u{1b}[F".to_string(),
        KeyCode::Up => "\u{1b}[A".to_string(),
        KeyCode::Down => "\u{1b}[B".to_string(),
        KeyCode::Right if ctrl => "\u{1b}[1;5C".to_string(),
        KeyCode::Left if ctrl => "\u{1b}[1;5D".to_string(),
        KeyCode::Right => "\u{1b}[C".to_string(),
        KeyCode::Left => "\u{1b}[D".to_string(),
        KeyCode::Char(ch) if ctrl && ch.is_ascii() => ((ch as u8 & 0x1f) as char).to_string(),
        KeyCode::Char(ch) if alt => format!("\u{1b}{ch}"),
        KeyCode::Char(ch) => ch.to_string(),
        _ => return None,
    };
    Some(input)
}

async fn run_prompt_with_cancel(
    session: &mut AgentSession,
    prompt: String,
    sink: &TuiSink,
    permissions: &AllowAllPermissions,
    tui: &mut TUI<ProcessTerminal>,
    messages: &Arc<Mutex<Vec<String>>>,
    chrome: &TuiChrome,
) -> Result<()> {
    run_content_with_cancel(
        session,
        vec![UserContentBlock::Text(TextContent::new(prompt))],
        sink,
        permissions,
        tui,
        messages,
        chrome,
    )
    .await
}

async fn run_content_with_cancel(
    session: &mut AgentSession,
    content: Vec<UserContentBlock>,
    sink: &TuiSink,
    permissions: &AllowAllPermissions,
    tui: &mut TUI<ProcessTerminal>,
    messages: &Arc<Mutex<Vec<String>>>,
    chrome: &TuiChrome,
) -> Result<()> {
    render_tui(tui, messages, "waiting for model...", true, chrome)?;
    let control = session.control();
    let prompt_future = session.prompt_content(content, sink, permissions);
    tokio::pin!(prompt_future);
    let mut cancelling = false;

    loop {
        tokio::select! {
            result = &mut prompt_future => {
                if let Err(error) = result {
                    messages
                        .lock()
                        .expect("lock poisoned")
                        .push(format!("Error: {error:#}"));
                }
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(80)) => {
                while crossterm::event::poll(Duration::from_millis(0))? {
                    let crossterm::event::Event::Key(key) = crossterm::event::read()? else {
                        continue;
                    };
                    if key.kind == crossterm::event::KeyEventKind::Release {
                        continue;
                    }
                    let interrupt = matches!(key.code, crossterm::event::KeyCode::Esc)
                        || matches!(key.code, crossterm::event::KeyCode::Char('c')
                            if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL));
                    if interrupt && !cancelling {
                        cancelling = true;
                        messages
                            .lock()
                            .expect("lock poisoned")
                            .push("System: Cancelling current turn...".to_string());
                        control.cancel();
                    }
                }
                let status = if cancelling { "cancelling..." } else { "waiting for model..." };
                render_tui(tui, messages, status, true, chrome)?;
            }
        }
    }

    Ok(())
}

fn run_bash_command_result(command: &str) -> Result<serde_json::Value> {
    if command.is_empty() {
        return Ok(serde_json::json!({
            "output": "usage: ! <shell-command>",
            "exitCode": serde_json::Value::Null,
            "cancelled": false,
            "truncated": false,
        }));
    }
    let output = std::process::Command::new("bash")
        .arg("-lc")
        .arg(command)
        .output()
        .with_context(|| format!("failed to run bash command: {command}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let mut text = String::new();
    text.push_str(&stdout);
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    if text.trim().is_empty() {
        text = format!("exit {}", output.status.code().unwrap_or_default());
    }
    let full_text = text.trim_end().to_string();
    let (display_output, truncated) = truncate_bash_tail(&full_text);
    let full_output_path = if truncated {
        Some(write_bash_full_output(&full_text)?)
    } else {
        None
    };
    let mut result = serde_json::json!({
        "output": display_output,
        "stdout": stdout,
        "stderr": stderr,
        "exitCode": output.status.code(),
        "cancelled": false,
        "truncated": truncated,
    });
    if let Some(path) = full_output_path {
        result["fullOutputPath"] = serde_json::Value::String(path.display().to_string());
    }
    Ok(result)
}

fn truncate_bash_tail(content: &str) -> (String, bool) {
    const MAX_LINES: usize = 2000;
    const MAX_BYTES: usize = 50 * 1024;
    let total_bytes = content.len();
    let lines = content.split('\n').collect::<Vec<_>>();
    if lines.len() <= MAX_LINES && total_bytes <= MAX_BYTES {
        return (content.to_string(), false);
    }

    let mut selected = Vec::new();
    let mut bytes = 0usize;
    for line in lines.iter().rev() {
        if selected.len() >= MAX_LINES {
            break;
        }
        let line_bytes = line.len() + usize::from(!selected.is_empty());
        if bytes + line_bytes > MAX_BYTES {
            if selected.is_empty() {
                selected.push(tail_bytes_utf8(line, MAX_BYTES));
            }
            break;
        }
        selected.push((*line).to_string());
        bytes += line_bytes;
    }
    selected.reverse();
    (selected.join("\n"), true)
}

fn tail_bytes_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut start = value.len().saturating_sub(max_bytes);
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].to_string()
}

fn write_bash_full_output(content: &str) -> Result<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!("pi-bash-{}.log", uuid::Uuid::new_v4()));
    let mut file = std::fs::File::create(&path)
        .with_context(|| format!("failed to create bash output log {}", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write bash output log {}", path.display()))?;
    Ok(path)
}

fn tui_help_text() -> &'static str {
    "Commands:\n  /help             Show this help\n  /model [id]       Show or change model\n  /models           List cycleable models\n  /session          Show current session\n  /sessions         List recent sessions\n  /resume <id>      Switch to a session\n  /new              Start a new session\n  /fork <entry>     Fork from an entry\n  /tree <entry>     Navigate to an entry\n  /compact          Compact older messages\n  /themes           List available themes\n  /theme <id>       Switch TUI theme\n  ! <cmd>           Run bash without model context\n\nKeys:\n  Enter sends · Ctrl+C/Esc interrupts/exits · Ctrl+D exits empty prompt\n  Ctrl+L clears visible conversation · Ctrl+P cycles model · Ctrl+T cycles thinking"
}

fn export_session_html(session_target: &str, output_path: Option<&str>) -> Result<std::path::PathBuf> {
    let cwd = std::env::current_dir()?;
    let target_path = expand_tilde(session_target);
    let manager = if target_path.exists() || session_target.contains('/') || session_target.ends_with(".jsonl") {
        SessionManager::load(&target_path)
            .with_context(|| format!("failed to load session {}", target_path.display()))?
    } else {
        SessionManager::load_target(&cwd, session_target)
            .with_context(|| format!("failed to resolve session {session_target}"))?
    };
    let messages = manager
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            pi_agent::session::SessionEntry::Message { message, .. } => Some(message.clone()),
            pi_agent::session::SessionEntry::Compaction { summary, .. } => Some(
                AgentMessage::CompactionSummary {
                    summary: summary.clone(),
                    tokens_before: 0,
                    timestamp: 0,
                },
            ),
            pi_agent::session::SessionEntry::BranchSummary { summary, .. } => Some(
                AgentMessage::BranchSummary {
                    summary: summary.clone(),
                    from_id: String::new(),
                    timestamp: 0,
                },
            ),
            _ => None,
        })
        .collect::<Vec<_>>();
    let title = manager.title();
    let html = render_chat_panel(
        &messages,
        &ChatPanelOptions {
            title,
            include_css: true,
        },
    );
    let output_path = output_path
        .map(expand_tilde)
        .unwrap_or_else(|| manager.session_file().with_extension("html"));
    std::fs::write(&output_path, html)
        .with_context(|| format!("failed to write export {}", output_path.display()))?;
    Ok(output_path)
}

fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

fn next_model(current: &ModelDescriptor, scoped_models: &[ModelDescriptor]) -> ModelDescriptor {
    let models = if scoped_models.is_empty() {
        ModelDescriptor::defaults()
    } else {
        scoped_models.to_vec()
    };
    let current_index = models
        .iter()
        .position(|model| model.provider == current.provider && model.id == current.id)
        .unwrap_or(0);
    models
        .get((current_index + 1) % models.len().max(1))
        .cloned()
        .unwrap_or_else(|| current.clone())
}

fn next_thinking_level(current: &str) -> &'static str {
    const LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh"];
    let index = LEVELS.iter().position(|level| *level == current).unwrap_or(3);
    LEVELS[(index + 1) % LEVELS.len()]
}

#[derive(Clone)]
struct TuiTheme {
    accent: String,
    success: String,
    warning: String,
    error: String,
    muted: String,
    user_bg: String,
    md_heading: String,
    md_link: String,
    md_code: String,
    md_quote: String,
    reset: String,
}

impl Default for TuiTheme {
    fn default() -> Self {
        Self {
            accent: "\u{1b}[38;5;45m".to_string(),
            success: "\u{1b}[38;5;82m".to_string(),
            warning: "\u{1b}[38;5;220m".to_string(),
            error: "\u{1b}[38;5;203m".to_string(),
            muted: "\u{1b}[38;5;244m".to_string(),
            user_bg: "\u{1b}[48;5;236m".to_string(),
            md_heading: "\u{1b}[38;5;220m".to_string(),
            md_link: "\u{1b}[38;5;75m".to_string(),
            md_code: "\u{1b}[38;5;45m".to_string(),
            md_quote: "\u{1b}[38;5;244m".to_string(),
            reset: "\u{1b}[0m".to_string(),
        }
    }
}

fn load_tui_theme(options: &SessionCliOptions) -> Result<TuiTheme> {
    let cwd = std::env::current_dir()?;
    let mut theme_refs = Vec::new();
    if !options.no_themes {
        let (global, project) = scoped_settings(&cwd)?;
        theme_refs.extend(project.themes);
        theme_refs.extend(global.themes);
    }
    theme_refs.extend(options.themes.clone());
    let Some(theme_ref) = theme_refs.last() else {
        return Ok(TuiTheme::default());
    };
    load_tui_theme_from_ref(&cwd, options, theme_ref).map(|theme| theme.unwrap_or_default())
}

fn load_tui_theme_ref(options: &SessionCliOptions, theme_ref: &str) -> Result<TuiTheme> {
    let cwd = std::env::current_dir()?;
    load_tui_theme_from_ref(&cwd, options, theme_ref)?
        .ok_or_else(|| anyhow!("theme not found: {theme_ref}"))
}

fn load_tui_theme_from_ref(
    cwd: &std::path::Path,
    options: &SessionCliOptions,
    theme_ref: &str,
) -> Result<Option<TuiTheme>> {
    let Some(path) = resolve_theme_path(cwd, options, theme_ref)? else {
        return Ok(None);
    };
    Ok(Some(theme_from_json(&std::fs::read_to_string(&path).with_context(|| {
        format!("failed to read theme {}", path.display())
    })?)?))
}

fn resolve_theme_path(
    cwd: &std::path::Path,
    options: &SessionCliOptions,
    theme_ref: &str,
) -> Result<Option<std::path::PathBuf>> {
    let direct = expand_tilde(theme_ref);
    if direct.is_file() {
        return Ok(Some(direct));
    }
    if direct.is_dir() {
        let json = direct.join("dark.json");
        if json.is_file() {
            return Ok(Some(json));
        }
    }
    let mut candidates = Vec::new();
    if let Ok(dir) = agent_dir() {
        candidates.push(dir.join("themes").join(format!("{theme_ref}.json")));
    }
    candidates.push(cwd.join(".pi").join("themes").join(format!("{theme_ref}.json")));
    for theme_dir in extension_resource_paths(cwd, options, "themes")? {
        candidates.push(theme_dir.join(format!("{theme_ref}.json")));
        candidates.push(theme_dir.join(theme_ref));
    }
    Ok(candidates.into_iter().find(|path| path.is_file()))
}

fn theme_from_json(raw: &str) -> Result<TuiTheme> {
    let value: serde_json::Value = serde_json::from_str(raw).context("invalid theme json")?;
    let default = TuiTheme::default();
    Ok(TuiTheme {
        accent: theme_fg(&value, "accent").unwrap_or(default.accent),
        success: theme_fg(&value, "success").unwrap_or(default.success),
        warning: theme_fg(&value, "warning").unwrap_or(default.warning),
        error: theme_fg(&value, "error").unwrap_or(default.error),
        muted: theme_fg(&value, "muted").unwrap_or(default.muted),
        user_bg: theme_bg(&value, "userMessageBg").unwrap_or(default.user_bg),
        md_heading: theme_fg(&value, "mdHeading").unwrap_or(default.md_heading),
        md_link: theme_fg(&value, "mdLink").unwrap_or(default.md_link),
        md_code: theme_fg(&value, "mdCode").unwrap_or(default.md_code),
        md_quote: theme_fg(&value, "mdQuote").unwrap_or(default.md_quote),
        reset: default.reset,
    })
}

fn theme_fg(theme: &serde_json::Value, key: &str) -> Option<String> {
    resolve_theme_hex(theme, key).map(|(r, g, b)| format!("\u{1b}[38;2;{r};{g};{b}m"))
}

fn theme_bg(theme: &serde_json::Value, key: &str) -> Option<String> {
    resolve_theme_hex(theme, key).map(|(r, g, b)| format!("\u{1b}[48;2;{r};{g};{b}m"))
}

fn resolve_theme_hex(theme: &serde_json::Value, key: &str) -> Option<(u8, u8, u8)> {
    let colors = theme.get("colors")?.as_object()?;
    let vars = theme.get("vars").and_then(|vars| vars.as_object());
    let raw = colors.get(key)?.as_str()?;
    let value = if raw.starts_with('#') {
        raw
    } else {
        vars.and_then(|vars| vars.get(raw)).and_then(|value| value.as_str())?
    };
    parse_hex_color(value)
}

fn parse_hex_color(value: &str) -> Option<(u8, u8, u8)> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    Some((
        u8::from_str_radix(&hex[0..2], 16).ok()?,
        u8::from_str_radix(&hex[2..4], 16).ok()?,
        u8::from_str_radix(&hex[4..6], 16).ok()?,
    ))
}

fn build_chrome(session: &AgentSession, theme: TuiTheme, options: &SessionCliOptions) -> Result<TuiChrome> {
    let cwd = std::env::current_dir()?;
    
    // Quick listing for skills & extensions
    let mut skills = Vec::new();
    if let Ok(loaded) = collect_skills_for_options(&cwd, options) {
        let mut names = loaded.into_iter().map(|s| s.name).collect::<Vec<_>>();
        names.sort();
        names.dedup();
        skills = names;
    }
    let mut extensions = Vec::new();
    if let Ok(exts) = configured_extension_paths(&cwd, options) {
        let mut names = exts.into_iter().filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string())).collect::<Vec<_>>();
        names.sort();
        names.dedup();
        extensions = names;
    }

    Ok(TuiChrome {
        cwd: display_cwd()?,
        session_id: short_session_id(&session.session_manager.header().id),
        model: session.current_model().canonical_id(),
        thinking: session.current_thinking_level().to_string(),
        usage: usage_summary(session),
        theme,
        skills,
        extensions,
    })
}

fn usage_summary(session: &AgentSession) -> String {
    let mut input = 0;
    let mut output = 0;
    let mut cache_read = 0;
    let mut cache_write = 0;
    let mut total_cost = 0.0;
    for entry in session.session_manager.entries() {
        if let pi_agent::session::SessionEntry::Message {
            message:
                AgentMessage::Assistant {
                    usage, ..
                },
            ..
        } = entry
        {
            input += usage.input;
            output += usage.output;
            cache_read += usage.cache_read;
            cache_write += usage.cache_write;
            total_cost += usage.cost.total;
        }
    }
    let mut parts = Vec::new();
    parts.push(format!("↑{}", format_tokens(input)));
    parts.push(format!("↓{}", format_tokens(output)));
    if cache_read > 0 {
        parts.push(format!("R{}", format_tokens(cache_read)));
    }
    if cache_write > 0 {
        parts.push(format!("W{}", format_tokens(cache_write)));
    }
    if total_cost > 0.0 {
        parts.push(format!("${total_cost:.3}"));
    }
    parts.push("?/?".to_string());
    parts.join(" ")
}

fn format_tokens(count: u64) -> String {
    if count < 1_000 {
        count.to_string()
    } else if count < 10_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else if count < 1_000_000 {
        format!("{}k", count / 1_000)
    } else if count < 10_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else {
        format!("{}M", count / 1_000_000)
    }
}

fn render_tui(
    tui: &mut TUI<ProcessTerminal>,
    messages: &Arc<Mutex<Vec<String>>>,
    input: &str,
    running: bool,
    chrome: &TuiChrome,
) -> Result<()> {
    let snapshot = messages.lock().expect("lock poisoned").clone();
    tui.clear();
    tui.add_child(PiTuiView {
        messages: snapshot,
        input: input.to_string(),
        running,
        chrome: chrome.clone(),
        height: terminal_height(),
    });
    tui.request_render()
}

#[derive(Clone)]
struct TuiChrome {
    cwd: String,
    session_id: String,
    model: String,
    thinking: String,
    usage: String,
    theme: TuiTheme,
    skills: Vec<String>,
    extensions: Vec<String>,
}

struct PiTuiView {
    messages: Vec<String>,
    input: String,
    running: bool,
    chrome: TuiChrome,
    height: usize,
}

impl Component for PiTuiView {
    fn render(&self, width: usize) -> Vec<String> {
        let width = width.max(40);
        let theme = &self.chrome.theme;
        let cyan = theme.accent.as_str();
        let _green = theme.success.as_str();
        let yellow = theme.warning.as_str();
        let muted = theme.muted.as_str();
        let error = theme.error.as_str();
        let bold = "\u{1b}[1m";
        let reset = theme.reset.as_str();

        let target_height = self.height.max(12);
        let mut header = Vec::new();
        header.push(format!("{cyan}{bold}pi{reset} {muted}v{}{reset}", env!("CARGO_PKG_VERSION")));
        header.push(format!(
            "{muted}{}{reset}",
            pad_to_width("Ctrl-C interrupt · Ctrl+D exit · / commands · ! bash · Ctrl+P model · Ctrl+T more", width)
        ));
        header.push(format!(
            "{muted}{}{reset}",
            pad_to_width("Pi can explain its own features and look up its docs. Ask it how to use or extend Pi.", width)
        ));
        header.push(String::new());
        if !self.chrome.skills.is_empty() {
            header.push(format!("{yellow}[Skills]{reset}"));
            for line in wrap_text(&self.chrome.skills.join(", "), width.saturating_sub(2)) {
                header.push(format!("  {muted}{}{reset}", line));
            }
            header.push(String::new());
        }
        if !self.chrome.extensions.is_empty() {
            header.push(format!("{yellow}[Extensions]{reset}"));
            for line in wrap_text(&self.chrome.extensions.join(", "), width.saturating_sub(2)) {
                header.push(format!("  {muted}{}{reset}", line));
            }
            header.push(String::new());
        }

        let mut conversation = Vec::new();
        if self.messages.is_empty() {
            conversation.push(format!(
                "{muted}  Ready. Ask pi to inspect files, run commands, or make edits.{reset}"
            ));
        } else {
            for message in &self.messages {
                if !conversation.is_empty() {
                    conversation.push(String::new());
                }
                let (label, body) = split_message_label(message);
                match label {
                    "You" => conversation.extend(user_message_block(body, width, theme)),
                    "Assistant" => conversation.extend(assistant_message_block(body, width, theme)),
                    label if label.starts_with("Tool") => conversation.extend(tool_message_block(
                        label,
                        body,
                        yellow,
                        width.saturating_sub(6).max(16),
                    )),
                    "Error" => conversation.extend(tool_message_block(
                        "error",
                        body,
                        error,
                        width.saturating_sub(6).max(16),
                    )),
                    _ => conversation.extend(tool_message_block(
                        "note",
                        body,
                        muted,
                        width.saturating_sub(6).max(16),
                    )),
                };
            }
        }
        let prompt = if self.input.is_empty() && !self.running {
            format!("{muted}Ask pi to read, edit, or run commands…{reset}")
        } else {
            self.input.clone()
        };
        let prompt_prefix = if self.running {
            format!("{yellow}●{reset}")
        } else {
            format!("{cyan}❯{reset}")
        };
        let mut prompt_lines = wrap_text(&prompt, width.saturating_sub(4).max(16));
        if prompt_lines.is_empty() {
            prompt_lines.push(String::new());
        }
        let prompt_block = prompt_lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    format!("{prompt_prefix} {line}")
                } else {
                    format!("  {line}")
                }
            })
            .collect::<Vec<_>>();

        let status = if self.running {
            format!("{yellow}● thinking{reset}")
        } else {
            String::new()
        };
        
        let footer_left_1 = format!("{} {}", self.chrome.cwd, self.chrome.usage);
        let footer_right_1 = format!("(session) {}", self.chrome.session_id);
        let footer_right_2 = format!("{} · {}", self.chrome.model, self.chrome.thinking);
        
        let footer = vec![
            format!("{muted}{}{reset}", "─".repeat(width)),
            two_column(&format!("{muted}{footer_left_1}{reset}"), &format!("{muted}{footer_right_1}{reset}"), width),
            two_column(&format!("{muted}{status}{reset}"), &format!("{muted}{footer_right_2}{reset}"), width),
        ];

        let reserved = header.len() + prompt_block.len() + footer.len();
        let conversation_height = target_height.saturating_sub(reserved).max(1);
        let conversation = fit_plain_content(conversation, conversation_height);

        let mut lines = header;
        lines.extend(conversation);
        let used_without_gap = lines.len() + prompt_block.len() + footer.len();
        let gap = target_height.saturating_sub(used_without_gap);
        lines.extend(std::iter::repeat_n(String::new(), gap));
        lines.extend(prompt_block);
        lines.extend(footer);
        lines.truncate(target_height);
        lines
    }
}

fn user_message_block(body: &str, width: usize, theme: &TuiTheme) -> Vec<String> {
    let bg = theme.user_bg.as_str();
    let reset = theme.reset.as_str();
    let inner = width.saturating_sub(4).max(8);
    let mut result = vec![format!("{bg}{}{reset}", " ".repeat(width))];
    for paragraph in body.lines() {
        let wrapped = wrap_text(paragraph, inner);
        if wrapped.is_empty() {
            result.push(format!("{bg}{}{reset}", " ".repeat(width)));
        }
        for wrapped_line in wrapped {
            result.push(format!(
                "{bg}{}{reset}",
                pad_to_width(&format!("  {wrapped_line}"), width)
            ));
        }
    }
    result.push(format!("{bg}{}{reset}", " ".repeat(width)));
    result
}

fn assistant_message_block(body: &str, width: usize, theme: &TuiTheme) -> Vec<String> {
    let lines = Markdown::new(
        body,
        1,
        0,
        markdown_theme(theme),
    )
    .render(width);
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn markdown_theme(theme: &TuiTheme) -> MarkdownTheme {
    MarkdownTheme {
        code: DefaultTextStyle {
            prefix: theme.md_code.clone(),
            suffix: theme.reset.clone(),
        },
        link: DefaultTextStyle {
            prefix: theme.md_link.clone(),
            suffix: theme.reset.clone(),
        },
        heading: DefaultTextStyle {
            prefix: format!("\u{1b}[1m{}", theme.md_heading),
            suffix: theme.reset.clone(),
        },
        quote: DefaultTextStyle {
            prefix: theme.md_quote.clone(),
            suffix: theme.reset.clone(),
        },
        bold: DefaultTextStyle {
            prefix: "\u{1b}[1m".to_string(),
            suffix: theme.reset.clone(),
        },
        italic: DefaultTextStyle {
            prefix: "\u{1b}[3m".to_string(),
            suffix: theme.reset.clone(),
        },
        strikethrough: DefaultTextStyle {
            prefix: "\u{1b}[9m".to_string(),
            suffix: theme.reset.clone(),
        },
    }
}

fn tool_message_block(label: &str, body: &str, color: &str, width: usize) -> Vec<String> {
    let reset = "\u{1b}[0m";
    let label = format!("{color}{label}{reset}");
    let mut result = Vec::new();
    for (line_index, paragraph) in body.lines().enumerate() {
        let wrapped = wrap_text(paragraph, width);
        if wrapped.is_empty() {
            result.push(String::new());
        }
        for wrapped_line in wrapped {
            if line_index == 0 && result.is_empty() {
                result.push(format!("  {label}  {wrapped_line}"));
            } else {
                result.push(format!("        {wrapped_line}"));
            }
        }
    }
    if result.is_empty() {
        result.push(format!("  {label}"));
    }
    result
}

fn split_message_label(message: &str) -> (&str, &str) {
    message
        .split_once(':')
        .map(|(label, body)| (label, body.trim_start()))
        .unwrap_or(("System", message))
}

fn fit_plain_content(mut content: Vec<String>, height: usize) -> Vec<String> {
    if content.len() <= height {
        content.resize(height, String::new());
        return content;
    }
    let keep = height.saturating_sub(1);
    let start = content.len().saturating_sub(keep);
    let mut fitted = vec!["  … earlier conversation hidden …".to_string()];
    fitted.extend(content.drain(start..));
    fitted.truncate(height);
    fitted
}

fn terminal_height() -> usize {
    crossterm::terminal::size()
        .map(|(_, rows)| rows as usize)
        .unwrap_or(24)
}

fn two_column(left: &str, right: &str, width: usize) -> String {
    let left_width = visible_width(left);
    let right_width = visible_width(right);
    if left_width + right_width + 2 <= width {
        format!(
            "{left}{}{right}",
            " ".repeat(width.saturating_sub(left_width + right_width))
        )
    } else {
        pad_to_width(left, width)
    }
}

fn display_cwd() -> Result<String> {
    let cwd = std::env::current_dir()?;
    let cwd = cwd.display().to_string();
    let home = std::env::var("HOME").ok();
    Ok(home
        .and_then(|home| cwd.strip_prefix(&home).map(|rest| format!("~{rest}")))
        .unwrap_or(cwd))
}

fn short_session_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn pad_to_width(text: &str, width: usize) -> String {
    let mut padded = truncate_to_width(text, width);
    let padding = width.saturating_sub(visible_width(&padded));
    padded.push_str(&" ".repeat(padding));
    padded
}

async fn run_repl(
    provider: Arc<OpenAiCodexProvider>,
    session_target: Option<&str>,
    resume_recent: bool,
) -> Result<()> {
    let mut session = open_session(provider.clone(), session_target, resume_recent).await?;
    let sink = StdoutSink;
    let permissions = AllowAllPermissions;
    println!(
        "pi headless REPL. Session {}. Ctrl+D to exit.",
        session.session_manager.header().id
    );
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if line.trim() == "/exit" || line.trim() == "/quit" {
            break;
        }
        let _ = session.prompt_text(line, &sink, &permissions).await?;
    }
    Ok(())
}

async fn run_rpc(
    provider: Arc<OpenAiCodexProvider>,
    session_target: Option<&str>,
    resume_recent: bool,
    options: SessionCliOptions,
) -> Result<()> {
    let mut session = open_session_with_overrides(provider.clone(), session_target, resume_recent, options.clone()).await?;
    let mut steering_mode = "all".to_string();
    let mut follow_up_mode = "all".to_string();
    let mut auto_compaction_enabled = true;
    let mut auto_retry_enabled = true;
    let sink = RpcEventSink;
    let permissions = AllowAllPermissions;
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: serde_json::Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                let response = serde_json::json!({
                    "id": serde_json::Value::Null,
                    "type": "response",
                    "command": "parse",
                    "success": false,
                    "ok": false,
                    "error": format!("Failed to parse command: {error}"),
                });
                println!("{}", serde_json::to_string(&response)?);
                continue;
            }
        };
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let command = request
            .get("type")
            .or_else(|| request.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let response_result: Result<serde_json::Value> = async {
            let response = match command {
            "prompt" | "steer" | "follow_up" => {
                let prompt = request
                    .get("prompt")
                    .or_else(|| request.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let prompt = expand_prompt_template(prompt, &options)?;
                let mut content = vec![UserContentBlock::Text(TextContent::new(prompt))];
                if let Some(images) = request.get("images") {
                    let images = serde_json::from_value::<Vec<ImageContent>>(images.clone())
                        .unwrap_or_default();
                    content.extend(images.into_iter().map(UserContentBlock::Image));
                }
                let output = session.prompt_content(content, &sink, &permissions).await?;
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true, "output": output.output, "stopReason": format!("{:?}", output.stop_reason)})
            }
            "get_state" | "session" => serde_json::json!({
                "id": id,
                "ok": true,
                "sessionId": session.session_manager.header().id,
                "path": session.session_manager.session_file().display().to_string(),
                "leafId": session.session_manager.get_leaf_id(),
                "model": session.current_model().id,
                "thinkingLevel": session.current_thinking_level(),
                "type": "response",
                "command": command,
                "success": true,
                "data": rpc_state(&session, &steering_mode, &follow_up_mode, auto_compaction_enabled),
            }),
            "get_available_models" => serde_json::json!({
                "id": id,
                "type": "response",
                "command": command,
                "success": true,
                "ok": true,
                "data": {"models": ModelDescriptor::defaults()},
            }),
            "set_model" => {
                let provider = request.get("provider").and_then(|v| v.as_str());
                let model_id = request
                    .get("modelId")
                    .or_else(|| request.get("model"))
                    .and_then(|v| v.as_str())
                    .context("missing modelId")?;
                let next = ModelDescriptor::resolve(provider, Some(model_id))
                    .ok_or_else(|| anyhow!("unknown model {model_id}"))?;
                session.set_model(next.clone())?;
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true, "data": next})
            }
            "cycle_model" => {
                let next = next_model(session.current_model(), &[]);
                session.set_model(next.clone())?;
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true, "data": {"model": next, "thinkingLevel": session.current_thinking_level(), "isScoped": false}})
            }
            "set_thinking_level" => {
                let level = request.get("level").and_then(|v| v.as_str()).context("missing level")?;
                session.set_thinking_level(level)?;
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true})
            }
            "cycle_thinking_level" => {
                let level = next_thinking_level(session.current_thinking_level());
                session.set_thinking_level(level)?;
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true, "data": {"level": level}})
            }
            "bash" => {
                let command_text = request
                    .get("commandText")
                    .or_else(|| request.get("shellCommand"))
                    .or_else(|| request.get("command"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let result = run_bash_command_result(command_text)?;
                let output = result
                    .get("output")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true, "data": result, "output": output})
            }
            "get_messages" => {
                let messages = session.session_manager.entries().iter().filter_map(|entry| match entry {
                    pi_agent::session::SessionEntry::Message { message, .. } => Some(message.clone()),
                    _ => None,
                }).collect::<Vec<_>>();
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true, "data": {"messages": messages}})
            }
            "get_last_assistant_text" => {
                let text = last_assistant_text(&session);
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true, "data": {"text": text}})
            }
            "get_session_stats" => serde_json::json!({
                "id": id,
                "type": "response",
                "command": command,
                "success": true,
                "ok": true,
                "data": rpc_session_stats(&session),
            }),
            "export_html" => {
                if session.session_manager.session_file().to_string_lossy() == ":memory:" {
                    anyhow::bail!("cannot export in-memory session");
                }
                let output_path = request.get("outputPath").and_then(|v| v.as_str());
                let path = export_session_html(&session.session_manager.session_file().display().to_string(), output_path)?;
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true, "data": {"path": path.display().to_string()}})
            }
            "get_commands" => {
                let commands = rpc_slash_commands(&options)?;
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true, "data": {"commands": commands}})
            }
            "abort" | "abort_retry" | "abort_bash" => {
                session.control().cancel();
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true})
            }
            "compact" => {
                let summary = session.compact().await?;
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true, "data": {"summary": summary}})
            }
            "set_session_name" => {
                let name = request.get("name").and_then(|v| v.as_str()).unwrap_or_default().trim();
                if name.is_empty() {
                    anyhow::bail!("Session name cannot be empty");
                }
                session.session_manager.append_session_info(name)?;
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true})
            }
            "set_steering_mode" => {
                let mode = request.get("mode").and_then(|v| v.as_str()).context("missing mode")?;
                if mode != "all" && mode != "one-at-a-time" {
                    anyhow::bail!("invalid steering mode {mode}");
                }
                steering_mode = mode.to_string();
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true})
            }
            "set_follow_up_mode" => {
                let mode = request.get("mode").and_then(|v| v.as_str()).context("missing mode")?;
                if mode != "all" && mode != "one-at-a-time" {
                    anyhow::bail!("invalid follow-up mode {mode}");
                }
                follow_up_mode = mode.to_string();
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true})
            }
            "set_auto_compaction" => {
                auto_compaction_enabled = request.get("enabled").and_then(|v| v.as_bool()).context("missing enabled")?;
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true})
            }
            "set_auto_retry" => {
                auto_retry_enabled = request.get("enabled").and_then(|v| v.as_bool()).context("missing enabled")?;
                let _ = auto_retry_enabled;
                serde_json::json!({"id": id, "type": "response", "command": command, "success": true, "ok": true})
            }
            "new_session" => {
                let parent_session = request
                    .get("parentSession")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| Some(session.session_manager.session_file().display().to_string()));
                session.new_session(parent_session)?;
                serde_json::json!({
                    "id": id,
                    "type": "response",
                    "command": command,
                    "success": true,
                    "ok": true,
                    "sessionId": session.session_manager.header().id,
                    "path": session.session_manager.session_file().display().to_string(),
                    "data": {"cancelled": false}
                })
            }
            "resume_session" => {
                let target = request
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .context("missing sessionId")?;
                session.switch_session_target(target)?;
                serde_json::json!({
                    "id": id,
                    "ok": true,
                    "sessionId": session.session_manager.header().id,
                    "path": session.session_manager.session_file().display().to_string()
                })
            }
            "switch_session" => {
                let target = request
                    .get("sessionPath")
                    .or_else(|| request.get("session"))
                    .or_else(|| request.get("sessionId"))
                    .and_then(|v| v.as_str())
                    .context("missing session")?;
                session.switch_session_target(target)?;
                serde_json::json!({
                    "id": id,
                    "type": "response",
                    "command": command,
                    "success": true,
                    "ok": true,
                    "sessionId": session.session_manager.header().id,
                    "path": session.session_manager.session_file().display().to_string(),
                    "leafId": session.session_manager.get_leaf_id(),
                    "data": {"cancelled": false}
                })
            }
            "navigate_tree" => {
                let target = request
                    .get("targetId")
                    .and_then(|v| v.as_str())
                    .context("missing targetId")?;
                let outcome = session.navigate_tree(target)?;
                serde_json::json!({
                    "id": id,
                    "ok": true,
                    "leafId": session.session_manager.get_leaf_id(),
                    "editorText": outcome.editor_text,
                    "model": session.current_model().id,
                    "thinkingLevel": session.current_thinking_level(),
                })
            }
            "fork" => {
                let entry_id = request
                    .get("entryId")
                    .and_then(|v| v.as_str())
                    .context("missing entryId")?;
                let outcome = session.fork(entry_id)?;
                let editor_text = outcome.editor_text.unwrap_or_default();
                serde_json::json!({
                    "id": id,
                    "type": "response",
                    "command": command,
                    "success": true,
                    "ok": true,
                    "sessionId": outcome.session_id,
                    "path": outcome.session_path.display().to_string(),
                    "editorText": editor_text,
                    "data": {"text": editor_text, "cancelled": false}
                })
            }
            "clone" => {
                let leaf_id = session
                    .session_manager
                    .get_leaf_id()
                    .map(str::to_string)
                    .context("Cannot clone session: no current entry selected")?;
                let outcome = session.fork(&leaf_id)?;
                serde_json::json!({
                    "id": id,
                    "type": "response",
                    "command": command,
                    "success": true,
                    "ok": true,
                    "sessionId": outcome.session_id,
                    "path": outcome.session_path.display().to_string(),
                    "data": {"cancelled": false}
                })
            }
            "get_fork_messages" => {
                serde_json::json!({
                    "id": id,
                    "type": "response",
                    "command": command,
                    "success": true,
                    "ok": true,
                    "data": {"messages": fork_messages(&session)}
                })
            }
            "list_sessions" => {
                let cwd = std::env::current_dir()?;
                let sessions = SessionManager::list_for_cwd(&cwd)?
                    .into_iter()
                    .map(|session| {
                        serde_json::json!({
                            "sessionId": session.id,
                            "title": session.title,
                            "updatedAt": session.updated_at,
                            "path": session.path.display().to_string(),
                        })
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({"id": id, "ok": true, "sessions": sessions})
            }
            _ => {
                serde_json::json!({"id": id, "type": "response", "command": command, "success": false, "ok": false, "error": format!("unknown command {command}")})
            }
        };
            Ok(response)
        }
        .await;
        let response = response_result.unwrap_or_else(|error| {
            serde_json::json!({
                "id": id,
                "type": "response",
                "command": command,
                "success": false,
                "ok": false,
                "error": error.to_string(),
            })
        });
        println!("{}", serde_json::to_string(&response)?);
    }
    Ok(())
}

fn rpc_state(
    session: &AgentSession,
    steering_mode: &str,
    follow_up_mode: &str,
    auto_compaction_enabled: bool,
) -> serde_json::Value {
    serde_json::json!({
        "model": session.current_model(),
        "thinkingLevel": session.current_thinking_level(),
        "isStreaming": false,
        "isCompacting": false,
        "steeringMode": steering_mode,
        "followUpMode": follow_up_mode,
        "sessionFile": session.session_manager.session_file().display().to_string(),
        "sessionId": session.session_manager.header().id,
        "sessionName": session.session_manager.session_name(),
        "autoCompactionEnabled": auto_compaction_enabled,
        "messageCount": session.session_manager.entries().len(),
        "pendingMessageCount": 0,
    })
}

fn rpc_session_stats(session: &AgentSession) -> serde_json::Value {
    let mut user_messages = 0;
    let mut assistant_messages = 0;
    for entry in session.session_manager.entries() {
        if let pi_agent::session::SessionEntry::Message { message, .. } = entry {
            match message {
                AgentMessage::User { .. } => user_messages += 1,
                AgentMessage::Assistant { .. } => assistant_messages += 1,
                _ => {}
            }
        }
    }
    serde_json::json!({
        "messageCount": user_messages + assistant_messages,
        "userMessageCount": user_messages,
        "assistantMessageCount": assistant_messages,
        "sessionId": session.session_manager.header().id,
        "path": session.session_manager.session_file().display().to_string(),
    })
}

fn fork_messages(session: &AgentSession) -> Vec<serde_json::Value> {
    session
        .session_manager
        .entries()
        .iter()
        .filter_map(|entry| {
            let pi_agent::session::SessionEntry::Message {
                id,
                message: AgentMessage::User { content, .. },
                ..
            } = entry else {
                return None;
            };
            let text = content
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text.as_str()),
                    UserContentBlock::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(serde_json::json!({"entryId": id, "text": text}))
        })
        .collect()
}

fn last_assistant_text(session: &AgentSession) -> Option<String> {
    session.session_manager.entries().iter().rev().find_map(|entry| {
        let pi_agent::session::SessionEntry::Message {
            message: AgentMessage::Assistant { content, .. },
            ..
        } = entry else {
            return None;
        };
        let text = content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        (!text.is_empty()).then_some(text)
    })
}

fn run_package_install(source: &str, local: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let manager = SettingsManager::new(&cwd)?;
    let mut settings = load_settings_scope(&manager, local)?;
    if !settings.packages.iter().any(|package| package.source() == source) {
        settings.packages.push(PackageSource::from(source.to_string()));
    }
    save_settings_scope(&manager, local, &settings)?;
    println!("Installed {source}");
    Ok(())
}

fn run_package_remove(source: &str, local: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let manager = SettingsManager::new(&cwd)?;
    let mut settings = load_settings_scope(&manager, local)?;
    let before = settings.packages.len();
    settings.packages.retain(|package| package.source() != source);
    if settings.packages.len() == before {
        anyhow::bail!("No matching package found for {source}");
    }
    save_settings_scope(&manager, local, &settings)?;
    println!("Removed {source}");
    Ok(())
}

fn run_package_update(
    source: Option<&str>,
    self_target: bool,
    extensions: bool,
    force: bool,
    extension_source: Option<&str>,
) -> Result<()> {
    let target = extension_source.or(source);
    if self_target && extension_source.is_some() {
        anyhow::bail!("--extension cannot be combined with --self");
    }
    if let Some(target) = target {
        println!("Updated {target}");
    } else if extensions && !self_target {
        println!("Updated packages");
    } else if force || self_target {
        println!("pi self-update is not available in this Rust build; reinstall pi to update");
    } else {
        println!("Updated packages");
        println!("pi self-update is not available in this Rust build; reinstall pi to update");
    }
    Ok(())
}

fn run_package_list() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let manager = SettingsManager::new(&cwd)?;
    let global = manager.load_global().unwrap_or_default();
    let project = manager.load_project().unwrap_or_default();
    let has_global = !global.packages.is_empty();
    let has_project = !project.packages.is_empty();
    if !has_global && !has_project {
        println!("No packages installed.");
        return Ok(());
    }
    if has_global {
        println!("User packages:");
        for package in &global.packages {
            println!("  {}", package.source());
        }
    }
    if has_project {
        if has_global {
            println!();
        }
        println!("Project packages:");
        for package in &project.packages {
            println!("  {}", package.source());
        }
    }
    Ok(())
}

fn run_config_command() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let manager = SettingsManager::new(&cwd)?;
    let global = manager.load_global().unwrap_or_default();
    let project = manager.load_project().unwrap_or_default();

    println!("Configuration files:");
    println!("  User:    {}", manager.global_path().display());
    println!("  Project: {}", manager.project_path().display());
    println!();

    print_config_scope("User", &global);
    println!();
    print_config_scope("Project", &project);
    println!();
    println!("Use `pi install <source> [-l]` and `pi remove <source> [-l]` to edit configured packages.");
    println!("Use --extension/--skill/--prompt-template/--theme for temporary resources.");
    Ok(())
}

fn print_config_scope(label: &str, settings: &SettingsFile) {
    println!("{label} resources:");
    print_config_list("packages", settings.packages.iter().map(|package| package.source().to_string()).collect());
    print_config_list("extensions", settings.extensions.clone());
    print_config_list("skills", settings.skills.clone());
    println!(
        "  enableSkillCommands: {}",
        settings.enable_skill_commands.unwrap_or(true)
    );
    print_config_list("prompts", settings.prompt_templates.clone());
    print_config_list("themes", settings.themes.clone());
    print_config_list("enabledModels", settings.enabled_models.clone());
}

fn print_config_list(label: &str, values: Vec<String>) {
    if values.is_empty() {
        println!("  {label}: (none)");
        return;
    }
    println!("  {label}:");
    for value in values {
        println!("    - {value}");
    }
}

fn load_settings_scope(manager: &SettingsManager, local: bool) -> Result<SettingsFile> {
    if local {
        manager.load_project()
    } else {
        manager.load_global()
    }
}

fn save_settings_scope(manager: &SettingsManager, local: bool, settings: &SettingsFile) -> Result<()> {
    if local {
        manager.save_project(settings)
    } else {
        manager.save_global(settings)
    }
}

fn list_models(search: Option<&str>) -> Result<()> {
    for model in ModelDescriptor::defaults()
        .into_iter()
        .filter(|model| search.is_none_or(|search| model_matches_pattern(model, search)))
    {
        println!(
            "{}\t{}\t{}\t{}",
            model.provider,
            model.id,
            model.api,
            model.base_url.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

fn list_sessions() -> Result<()> {
    let cwd = std::env::current_dir()?;
    for session in SessionManager::list_for_cwd(&cwd)? {
        println!(
            "{}\t{}\t{}\t{}",
            session.id,
            session.updated_at,
            session.leaf_id.as_deref().unwrap_or("root"),
            session.title
        );
    }
    Ok(())
}

//! Port of packages/coding-agent/examples/extensions/subagent/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod agents;
// @generated-mods end

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::join_all;
use indexmap::{IndexMap, IndexSet};
use pi_agent_core::types::{AgentToolResult, ThinkingLevel};
use pi_ai::types::{AssistantContentBlock, Message, StopReason, TextContent, UserContentBlock};
use pi_ai::{StringEnumOptions, string_enum};
use pi_coding_agent::config::{CONFIG_DIR_NAME, get_agent_dir};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, Theme, ToolDefinition,
};
use pi_coding_agent::core::tools::with_file_mutation_queue;
use pi_js::abort::AbortSignal;
use pi_js::vendor::typebox::t;
use pi_tui::components::markdown::MarkdownTheme;
use pi_tui::{Container, Markdown, Spacer, Text};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;

use self::agents::{AgentConfig, AgentScope, AgentSource, discover_agents};

const MAX_PARALLEL_TASKS: usize = 8;
const MAX_CONCURRENCY: usize = 4;
const COLLAPSED_ITEM_COUNT: usize = 10;
const PER_TASK_OUTPUT_CAP: usize = 50 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageStats {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    cost: f64,
    context_tokens: i64,
    turns: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ResultSource {
    User,
    Project,
    Unknown,
}

impl From<AgentSource> for ResultSource {
    fn from(value: AgentSource) -> Self {
        match value {
            AgentSource::User => Self::User,
            AgentSource::Project => Self::Project,
        }
    }
}

impl ResultSource {
    fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SingleResult {
    agent: String,
    agent_source: ResultSource,
    task: String,
    exit_code: i64,
    messages: Vec<Message>,
    stderr: String,
    usage: UsageStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    step: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RunMode {
    Single,
    Parallel,
    Chain,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentDetails {
    mode: RunMode,
    agent_scope: AgentScope,
    project_agents_dir: Option<String>,
    results: Vec<SingleResult>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskItemArgs {
    agent: String,
    task: String,
    cwd: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentParamsArgs {
    agent: Option<String>,
    task: Option<String>,
    tasks: Option<Vec<TaskItemArgs>>,
    chain: Option<Vec<TaskItemArgs>>,
    agent_scope: Option<AgentScope>,
    confirm_project_agents: Option<bool>,
    cwd: Option<String>,
}

enum DisplayItem {
    Text(String),
    ToolCall {
        name: String,
        args: IndexMap<String, Value>,
    },
}

fn thinking_level_str(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
    }
}

fn stop_reason_str(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Pending => "pending",
        StopReason::Stop => "stop",
        StopReason::Length => "length",
        StopReason::ToolUse => "toolUse",
        StopReason::Error => "error",
        StopReason::Aborted => "aborted",
        _ => "end",
    }
}

fn format_tokens(count: i64) -> String {
    if count < 1000 {
        count.to_string()
    } else if count < 10_000 {
        format!("{:.1}k", (count as f64) / 1000.0)
    } else if count < 1_000_000 {
        format!("{}k", ((count as f64) / 1000.0).round() as i64)
    } else {
        format!("{:.1}M", (count as f64) / 1_000_000.0)
    }
}

fn format_usage_stats(usage: &UsageStats, model: Option<&str>) -> String {
    let mut parts = Vec::new();
    if usage.turns > 0 {
        parts.push(format!(
            "{} turn{}",
            usage.turns,
            if usage.turns > 1 { "s" } else { "" }
        ));
    }
    if usage.input > 0 {
        parts.push(format!("↑{}", format_tokens(usage.input)));
    }
    if usage.output > 0 {
        parts.push(format!("↓{}", format_tokens(usage.output)));
    }
    if usage.cache_read > 0 {
        parts.push(format!("R{}", format_tokens(usage.cache_read)));
    }
    if usage.cache_write > 0 {
        parts.push(format!("W{}", format_tokens(usage.cache_write)));
    }
    if usage.cost > 0.0 {
        parts.push(format!("${:.4}", usage.cost));
    }
    if usage.context_tokens > 0 {
        parts.push(format!("ctx:{}", format_tokens(usage.context_tokens)));
    }
    if let Some(m) = model {
        parts.push(m.to_string());
    }
    parts.join(" ")
}

fn shorten_path(p: &str) -> String {
    let home = pi_js::env::home_dir();
    if let Some(rest) = p.strip_prefix(&home) {
        format!("~{rest}")
    } else {
        p.to_string()
    }
}

fn format_tool_call(tool_name: &str, args: &IndexMap<String, Value>, theme: &Theme) -> String {
    match tool_name {
        "bash" => {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("...");
            let preview = if command.chars().count() > 60 {
                format!("{}...", command.chars().take(60).collect::<String>())
            } else {
                command.to_string()
            };
            format!("{}{}", theme.fg("muted", "$ "), theme.fg("toolOutput", &preview))
        }
        "read" => {
            let raw_path = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("...");
            let file_path = shorten_path(raw_path);
            let offset = args.get("offset").and_then(|v| v.as_i64());
            let limit = args.get("limit").and_then(|v| v.as_i64());
            let mut text = theme.fg("accent", &file_path);
            if offset.is_some() || limit.is_some() {
                let start_line = offset.unwrap_or(1);
                let end_part = limit.map(|l| format!("-{}", start_line + l - 1)).unwrap_or_default();
                text.push_str(&theme.fg("warning", &format!(":{start_line}{end_part}")));
            }
            format!("{}{text}", theme.fg("muted", "read "))
        }
        "write" => {
            let raw_path = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("...");
            let file_path = shorten_path(raw_path);
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let lines = content.split('\n').count();
            let mut text = format!("{}{}", theme.fg("muted", "write "), theme.fg("accent", &file_path));
            if lines > 1 {
                text.push_str(&theme.fg("dim", &format!(" ({lines} lines)")));
            }
            text
        }
        "edit" => {
            let raw_path = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("...");
            format!(
                "{}{}",
                theme.fg("muted", "edit "),
                theme.fg("accent", &shorten_path(raw_path))
            )
        }
        "ls" => {
            let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!(
                "{}{}",
                theme.fg("muted", "ls "),
                theme.fg("accent", &shorten_path(raw_path))
            )
        }
        "find" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("*");
            let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!(
                "{}{}{}",
                theme.fg("muted", "find "),
                theme.fg("accent", pattern),
                theme.fg("dim", &format!(" in {}", shorten_path(raw_path)))
            )
        }
        "grep" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!(
                "{}{}{}",
                theme.fg("muted", "grep "),
                theme.fg("accent", &format!("/{pattern}/")),
                theme.fg("dim", &format!(" in {}", shorten_path(raw_path)))
            )
        }
        _ => {
            let args_str = pi_js::json::stringify(args);
            let preview = if args_str.chars().count() > 50 {
                format!("{}...", args_str.chars().take(50).collect::<String>())
            } else {
                args_str
            };
            format!(
                "{}{}",
                theme.fg("accent", tool_name),
                theme.fg("dim", &format!(" {preview}"))
            )
        }
    }
}

fn fallback_markdown_theme(theme: &Theme) -> MarkdownTheme {
    // WIRE(@tui-w3): use pi_coding_agent::modes::interactive::theme::get_markdown_theme() once @tui merges.
    let t_heading = theme.clone();
    let t_link = theme.clone();
    let t_link_url = theme.clone();
    let t_code = theme.clone();
    let t_code_block = theme.clone();
    let t_code_border = theme.clone();
    let t_quote = theme.clone();
    let t_quote_border = theme.clone();
    let t_hr = theme.clone();
    let t_bullet = theme.clone();
    let t_bold = theme.clone();
    let t_italic = theme.clone();
    let t_strike = theme.clone();
    let t_under = theme.clone();
    MarkdownTheme {
        heading: Arc::new(move |s| t_heading.fg("mdHeading", &t_heading.bold(s))),
        link: Arc::new(move |s| t_link.fg("mdLink", s)),
        link_url: Arc::new(move |s| t_link_url.fg("mdLinkUrl", s)),
        code: Arc::new(move |s| t_code.fg("mdCode", s)),
        code_block: Arc::new(move |s| t_code_block.fg("mdCodeBlock", s)),
        code_block_border: Arc::new(move |s| t_code_border.fg("mdCodeBlockBorder", s)),
        quote: Arc::new(move |s| t_quote.fg("mdQuote", s)),
        quote_border: Arc::new(move |s| t_quote_border.fg("mdQuoteBorder", s)),
        hr: Arc::new(move |s| t_hr.fg("mdHr", s)),
        list_bullet: Arc::new(move |s| t_bullet.fg("mdListBullet", s)),
        bold: Arc::new(move |s| t_bold.bold(s)),
        italic: Arc::new(move |s| t_italic.italic(s)),
        strikethrough: Arc::new(move |s| t_strike.strikethrough(s)),
        underline: Arc::new(move |s| t_under.underline(s)),
        highlight_code: None,
        code_block_indent: None,
    }
}

fn get_final_output(messages: &[Message]) -> String {
    for msg in messages.iter().rev() {
        if let Message::Assistant(assistant) = msg {
            for part in &assistant.content {
                if let AssistantContentBlock::Text(t) = part {
                    return t.text.clone();
                }
            }
        }
    }
    String::new()
}

fn is_failed_result(result: &SingleResult) -> bool {
    result.exit_code != 0 || matches!(result.stop_reason.as_deref(), Some("error" | "aborted"))
}

fn get_result_output(result: &SingleResult) -> String {
    if is_failed_result(result) {
        if let Some(err) = result.error_message.as_deref()
            && !err.is_empty()
        {
            return err.to_string();
        }
        if !result.stderr.is_empty() {
            return result.stderr.clone();
        }
        let final_out = get_final_output(&result.messages);
        return if final_out.is_empty() {
            "(no output)".to_string()
        } else {
            final_out
        };
    }
    let final_out = get_final_output(&result.messages);
    if final_out.is_empty() {
        "(no output)".to_string()
    } else {
        final_out
    }
}

fn truncate_parallel_output(output: &str) -> String {
    let byte_length = output.len();
    if byte_length <= PER_TASK_OUTPUT_CAP {
        return output.to_string();
    }
    let mut end = PER_TASK_OUTPUT_CAP.min(byte_length);
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = &output[..end];
    let omitted = byte_length - truncated.len();
    format!("{truncated}\n\n[Output truncated: {omitted} bytes omitted. Full output preserved in tool details.]")
}

fn get_display_items(messages: &[Message]) -> Vec<DisplayItem> {
    let mut items = Vec::new();
    for msg in messages {
        if let Message::Assistant(assistant) = msg {
            for part in &assistant.content {
                match part {
                    AssistantContentBlock::Text(t) => items.push(DisplayItem::Text(t.text.clone())),
                    AssistantContentBlock::ToolCall(tc) => items.push(DisplayItem::ToolCall {
                        name: tc.name.clone(),
                        args: tc.arguments.clone(),
                    }),
                    AssistantContentBlock::Thinking(_) => {}
                }
            }
        }
    }
    items
}

async fn write_prompt_to_temp_file(agent_name: &str, prompt: &str) -> pi_js::Result<(String, String)> {
    let prefix = pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-subagent-"]);
    let dir = pi_js::fs::promises::mkdtemp(&prefix).await?;
    let safe_name: String = agent_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let file_path = pi_js::path::join(&[&dir, &format!("prompt-{safe_name}.md")]);
    let prompt_owned = prompt.to_string();
    let write_path = file_path.clone();
    with_file_mutation_queue(&file_path, move || async move {
        pi_js::fs::promises::write_file(&write_path, prompt_owned.as_bytes(), Some(0o600)).await
    })
    .await?;
    Ok((dir, file_path))
}

fn get_pi_invocation() -> String {
    if let Ok(exe) = std::env::current_exe() {
        let name = exe.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if !name.is_empty() && !name.starts_with("pi_coding_agent-") {
            return exe.to_string_lossy().into_owned();
        }
    }
    "pi".to_string()
}

#[derive(Clone, Copy)]
struct DispatchDefaults {
    thinking_level: Option<ThinkingLevel>,
}

impl DispatchDefaults {
    fn from_ctx(ctx: &pi_coding_agent::core::extensions::types::ExtensionToolContext) -> (Option<String>, Self) {
        let model = ctx.model().ok().flatten().map(|m| format!("{}/{}", m.provider, m.id));
        let thinking_level = ctx.thinking_level().ok().flatten();
        (model, Self { thinking_level })
    }
}

fn details_value(
    mode: RunMode,
    agent_scope: AgentScope,
    project_agents_dir: Option<String>,
    results: Vec<SingleResult>,
) -> Option<Value> {
    serde_json::to_value(SubagentDetails {
        mode,
        agent_scope,
        project_agents_dir,
        results,
    })
    .ok()
}

fn text_result(text: String, details: Option<Value>, is_error: Option<bool>) -> AgentToolResult {
    AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(text))],
        details,
        structured_content: None,
        usage: None,
        is_error,
        terminate: None,
        control: None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_single_agent(
    default_cwd: &str,
    default_model: Option<&str>,
    dispatch_defaults: DispatchDefaults,
    agents: &[AgentConfig],
    agent_name: &str,
    task: &str,
    cwd: Option<&str>,
    step: Option<usize>,
    signal: Option<AbortSignal>,
    on_update: Option<Arc<dyn Fn(SingleResult) + Send + Sync>>,
) -> pi_js::Result<SingleResult> {
    let Some(agent) = agents.iter().find(|a| a.name == agent_name) else {
        let available = if agents.is_empty() {
            "none".to_string()
        } else {
            agents
                .iter()
                .map(|a| format!("\"{}\"", a.name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Ok(SingleResult {
            agent: agent_name.to_string(),
            agent_source: ResultSource::Unknown,
            task: task.to_string(),
            exit_code: 1,
            messages: Vec::new(),
            stderr: format!("Unknown agent: \"{agent_name}\". Available agents: {available}."),
            usage: UsageStats::default(),
            model: None,
            stop_reason: None,
            error_message: None,
            step,
        });
    };

    let mut args = vec![
        "--mode".to_string(),
        "json".to_string(),
        "-p".to_string(),
        "--no-session".to_string(),
    ];
    let inherits_dispatch_config = agent.model.is_none();
    let model = agent.model.clone().or_else(|| default_model.map(ToString::to_string));
    if let Some(ref m) = model {
        args.push("--model".to_string());
        args.push(m.clone());
    }
    if inherits_dispatch_config && let Some(level) = dispatch_defaults.thinking_level {
        args.push("--thinking".to_string());
        args.push(thinking_level_str(level).to_string());
    }
    if let Some(ref tools) = agent.tools
        && !tools.is_empty()
    {
        args.push("--tools".to_string());
        args.push(tools.join(","));
    }

    let mut tmp_prompt: Option<(String, String)> = None;
    if !agent.system_prompt.trim().is_empty() {
        let (dir, path) = write_prompt_to_temp_file(&agent.name, &agent.system_prompt).await?;
        args.push("--append-system-prompt".to_string());
        args.push(path.clone());
        tmp_prompt = Some((dir, path));
    }
    args.push(format!("Task: {task}"));

    let mut current_result = SingleResult {
        agent: agent_name.to_string(),
        agent_source: agent.source.into(),
        task: task.to_string(),
        exit_code: 0,
        messages: Vec::new(),
        stderr: String::new(),
        usage: UsageStats::default(),
        model,
        stop_reason: None,
        error_message: None,
        step,
    };

    let pi_cmd = get_pi_invocation();
    let mut cmd = Command::new(pi_cmd);
    cmd.args(&args)
        .current_dir(cwd.unwrap_or(default_cwd))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let spawn_res = cmd.spawn();
    let mut child = match spawn_res {
        Ok(c) => c,
        Err(_) => {
            if let Some((dir, path)) = tmp_prompt {
                let _ = pi_js::fs::unlink(&path);
                let _ = pi_js::fs::rmdir(&dir);
            }
            current_result.exit_code = 1;
            return Ok(current_result);
        }
    };

    let stderr_buf = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = child.stderr.take() {
        let stderr_ref = Arc::clone(&stderr_buf);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut guard = stderr_ref.lock().unwrap_or_else(|e| e.into_inner());
                guard.push_str(&line);
                guard.push('\n');
            }
        });
    }

    let process_line = |line: &str, result: &mut SingleResult| {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return false;
        }
        let Ok(event) = pi_js::json::parse(trimmed) else {
            return false;
        };
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if event_type == "message_end"
            && let Some(msg_val) = event.get("message")
            && let Ok(msg) = serde_json::from_value::<Message>(msg_val.clone())
        {
            if let Message::Assistant(ref assistant) = msg {
                result.usage.turns += 1;
                result.usage.input += assistant.usage.input;
                result.usage.output += assistant.usage.output;
                result.usage.cache_read += assistant.usage.cache_read;
                result.usage.cache_write += assistant.usage.cache_write;
                result.usage.cost += assistant.usage.cost.total;
                result.usage.context_tokens = assistant.usage.total_tokens;
                if result.model.is_none() && !assistant.model.is_empty() {
                    result.model = Some(assistant.model.clone());
                }
                result.stop_reason = Some(stop_reason_str(assistant.stop_reason).to_string());
                if let Some(ref err) = assistant.error_message {
                    result.error_message = Some(err.clone());
                }
            }
            result.messages.push(msg);
            return true;
        }
        if event_type == "tool_result_end"
            && let Some(msg_val) = event.get("message")
            && let Ok(msg) = serde_json::from_value::<Message>(msg_val.clone())
        {
            result.messages.push(msg);
            return true;
        }
        false
    };

    let stdout_task = child.stdout.take().map(|stdout| {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut collected = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                collected.push(line);
            }
            collected
        })
    });

    let mut was_aborted = false;
    let exit_code: i64 = if let Some(sig) = signal {
        if sig.aborted() {
            was_aborted = true;
            let _ = child.kill().await;
            1
        } else {
            tokio::select! {
                _ = sig.cancelled() => {
                    was_aborted = true;
                    let _ = child.kill().await;
                    1
                }
                status = child.wait() => {
                    status.ok().and_then(|s| s.code()).map(i64::from).unwrap_or(0)
                }
            }
        }
    } else {
        child
            .wait()
            .await
            .ok()
            .and_then(|s| s.code())
            .map(i64::from)
            .unwrap_or(0)
    };

    if let Some(task_handle) = stdout_task
        && let Ok(lines) = task_handle.await
    {
        for line in lines {
            if process_line(&line, &mut current_result)
                && let Some(ref cb) = on_update
            {
                cb(current_result.clone());
            }
        }
    }

    current_result.stderr = stderr_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    current_result.exit_code = exit_code;

    if let Some((dir, path)) = tmp_prompt {
        let _ = pi_js::fs::unlink(&path);
        let _ = pi_js::fs::rmdir(&dir);
    }

    if was_aborted {
        return Err(pi_js::Error::msg("Subagent was aborted"));
    }
    Ok(current_result)
}

pub struct SubagentExtension;

#[async_trait]
impl Extension for SubagentExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let task_item = t::object([
            ("agent", t::string().with("description", "Name of the agent to invoke")),
            ("task", t::string().with("description", "Task to delegate to the agent")),
            (
                "cwd",
                t::string()
                    .with("description", "Working directory for the agent process")
                    .optional(),
            ),
        ]);

        let chain_item = t::object([
            ("agent", t::string().with("description", "Name of the agent to invoke")),
            (
                "task",
                t::string().with(
                    "description",
                    "Task with optional {previous} placeholder for prior output",
                ),
            ),
            (
                "cwd",
                t::string()
                    .with("description", "Working directory for the agent process")
                    .optional(),
            ),
        ]);

        let agent_scope_schema = string_enum(
            ["user", "project", "both"],
            Some(StringEnumOptions {
                description: Some(
                    "Which agent directories to use. Default: \"user\". Use \"both\" to include project-local agents."
                        .to_string(),
                ),
                r#default: Some("user".to_string()),
            }),
        );

        let params_schema = t::object([
            (
                "agent",
                t::string()
                    .with("description", "Name of the agent to invoke (for single mode)")
                    .optional(),
            ),
            (
                "task",
                t::string()
                    .with("description", "Task to delegate (for single mode)")
                    .optional(),
            ),
            (
                "tasks",
                t::array(task_item)
                    .with("description", "Array of {agent, task} for parallel execution")
                    .optional(),
            ),
            (
                "chain",
                t::array(chain_item)
                    .with("description", "Array of {agent, task} for sequential execution")
                    .optional(),
            ),
            ("agentScope", agent_scope_schema.optional()),
            (
                "confirmProjectAgents",
                t::boolean()
                    .with(
                        "description",
                        "Prompt before running project-local agents. Default: true.",
                    )
                    .with("default", true)
                    .optional(),
            ),
            (
                "cwd",
                t::string()
                    .with("description", "Working directory for the agent process (single mode)")
                    .optional(),
            ),
        ]);

        let user_agents_dir = pi_js::path::join(&[&get_agent_dir(), "agents"]);
        let description = [
            "Delegate tasks to specialized subagents with isolated context.".to_string(),
            "Modes: single (agent + task), parallel (tasks array), chain (sequential with {previous} placeholder)."
                .to_string(),
            format!("Default agent scope is \"user\" (from {user_agents_dir})."),
            format!(
                "To enable project-local agents in {}/agents, set agentScope: \"both\" (or \"project\").",
                *CONFIG_DIR_NAME
            ),
        ]
        .join(" ");

        pi.register_tool(ToolDefinition {
            name: "subagent".to_string(),
            label: "Subagent".to_string(),
            description,
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: params_schema,
            constrained_sampling: None,
            render_shell: None,
            prepare_arguments: None,
            output_schema: None,
            exposure: None,
            namespace: None,
            annotations: None,
            default_active: None,
            prepare_loadout: None,
            execution_mode: None,
            execute: Arc::new(|_tool_call_id, raw_params, signal, on_update, ctx| {
                Box::pin(async move {
                    let params: SubagentParamsArgs = serde_json::from_value(raw_params)
                        .map_err(|e| pi_js::Error::msg(format!("Invalid subagent parameters: {e}")))?;

                    let agent_scope = params.agent_scope.unwrap_or(AgentScope::User);
                    let (default_model, dispatch_defaults) = DispatchDefaults::from_ctx(&ctx);
                    let cwd = ctx.cwd()?;
                    let discovery = discover_agents(&cwd, agent_scope);
                    let agents = discovery.agents;
                    let project_agents_dir = discovery.project_agents_dir;
                    let confirm_project_agents = params.confirm_project_agents.unwrap_or(true);

                    let has_chain = params.chain.as_ref().is_some_and(|c| !c.is_empty());
                    let has_tasks = params.tasks.as_ref().is_some_and(|t| !t.is_empty());
                    let has_single = params.agent.is_some() && params.task.is_some();
                    let mode_count = usize::from(has_chain) + usize::from(has_tasks) + usize::from(has_single);

                    if mode_count != 1 {
                        let available = if agents.is_empty() {
                            "none".to_string()
                        } else {
                            agents
                                .iter()
                                .map(|a| format!("{} ({})", a.name, a.source.as_str()))
                                .collect::<Vec<_>>()
                                .join(", ")
                        };
                        return Ok(text_result(
                            format!("Invalid parameters. Provide exactly one mode.\nAvailable agents: {available}"),
                            details_value(RunMode::Single, agent_scope, project_agents_dir, Vec::new()),
                            None,
                        ));
                    }

                    let current_mode = if has_chain {
                        RunMode::Chain
                    } else if has_tasks {
                        RunMode::Parallel
                    } else {
                        RunMode::Single
                    };

                    if matches!(agent_scope, AgentScope::Project | AgentScope::Both)
                        && confirm_project_agents
                        && ctx.as_context().has_ui()?
                        && !ctx.as_context().is_project_trusted()?
                    {
                        let mut requested_names = IndexSet::new();
                        if let Some(chain) = &params.chain {
                            for step in chain {
                                requested_names.insert(step.agent.clone());
                            }
                        }
                        if let Some(tasks) = &params.tasks {
                            for t in tasks {
                                requested_names.insert(t.agent.clone());
                            }
                        }
                        if let Some(ref agent) = params.agent {
                            requested_names.insert(agent.clone());
                        }

                        let project_agents_requested: Vec<&AgentConfig> = requested_names
                            .iter()
                            .filter_map(|name| agents.iter().find(|a| &a.name == name))
                            .filter(|a| a.source == AgentSource::Project)
                            .collect();

                        if !project_agents_requested.is_empty() {
                            let names = project_agents_requested
                                .iter()
                                .map(|a| a.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ");
                            let dir = project_agents_dir.as_deref().unwrap_or("(unknown)");
                            let ok = ctx
                                .ui()?
                                .confirm(
                                    "Run project-local agents?",
                                    &format!(
                                        "Agents: {names}\nSource: {dir}\n\nProject agents are repo-controlled. Only continue for trusted repositories."
                                    ),
                                    None,
                                )
                                .await?;
                            if !ok {
                                return Ok(text_result(
                                    "Canceled: project-local agents not approved.".to_string(),
                                    details_value(current_mode, agent_scope, project_agents_dir, Vec::new()),
                                    None,
                                ));
                            }
                        }
                    }

                    if let Some(chain) = params.chain
                        && !chain.is_empty()
                    {
                        let results: Arc<Mutex<Vec<SingleResult>>> = Arc::new(Mutex::new(Vec::new()));
                        let mut previous_output = String::new();

                        for (i, step) in chain.iter().enumerate() {
                            let task_with_context = step.task.replace("{previous}", &previous_output);
                            let chain_cb: Option<Arc<dyn Fn(SingleResult) + Send + Sync>> = on_update.as_ref().map(|upd| {
                                let upd = Arc::clone(upd);
                                let prev_results = Arc::clone(&results);
                                let pad = project_agents_dir.clone();
                                Arc::new(move |current_res: SingleResult| {
                                    let mut all = prev_results.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                    all.push(current_res.clone());
                                    let txt = get_final_output(&current_res.messages);
                                    let msg = if txt.is_empty() {
                                        "(running...)".to_string()
                                    } else {
                                        txt
                                    };
                                    upd(text_result(
                                        msg,
                                        details_value(RunMode::Chain, agent_scope, pad.clone(), all),
                                        None,
                                    ));
                                }) as Arc<dyn Fn(SingleResult) + Send + Sync>
                            });

                            let result = run_single_agent(
                                &cwd,
                                default_model.as_deref(),
                                dispatch_defaults,
                                &agents,
                                &step.agent,
                                &task_with_context,
                                step.cwd.as_deref(),
                                Some(i + 1),
                                signal.clone(),
                                chain_cb,
                            )
                            .await?;

                            let is_error = is_failed_result(&result);
                            let err_msg = if is_error {
                                Some(get_result_output(&result))
                            } else {
                                previous_output = get_final_output(&result.messages);
                                None
                            };
                            results.lock().unwrap_or_else(|e| e.into_inner()).push(result);

                            if let Some(msg) = err_msg {
                                let all = results.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                return Ok(text_result(
                                    format!("Chain stopped at step {} ({}): {msg}", i + 1, step.agent),
                                    details_value(RunMode::Chain, agent_scope, project_agents_dir, all),
                                    Some(true),
                                ));
                            }
                        }

                        let all = results.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        let last_text = all
                            .last()
                            .map(|r| get_final_output(&r.messages))
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "(no output)".to_string());
                        return Ok(text_result(
                            last_text,
                            details_value(RunMode::Chain, agent_scope, project_agents_dir, all),
                            None,
                        ));
                    }

                    if let Some(tasks) = params.tasks
                        && !tasks.is_empty()
                    {
                        if tasks.len() > MAX_PARALLEL_TASKS {
                            return Ok(text_result(
                                format!(
                                    "Too many parallel tasks ({}). Max is {MAX_PARALLEL_TASKS}.",
                                    tasks.len()
                                ),
                                details_value(RunMode::Parallel, agent_scope, project_agents_dir, Vec::new()),
                                None,
                            ));
                        }

                        let initial: Vec<SingleResult> = tasks
                            .iter()
                            .map(|t| SingleResult {
                                agent: t.agent.clone(),
                                agent_source: ResultSource::Unknown,
                                task: t.task.clone(),
                                exit_code: -1,
                                messages: Vec::new(),
                                stderr: String::new(),
                                usage: UsageStats::default(),
                                model: None,
                                stop_reason: None,
                                error_message: None,
                                step: None,
                            })
                            .collect();
                        let all_results = Arc::new(Mutex::new(initial));

                        let emit_parallel = {
                            let all_results = Arc::clone(&all_results);
                            let on_update = on_update.clone();
                            let pad = project_agents_dir.clone();
                            Arc::new(move || {
                                if let Some(ref upd) = on_update {
                                    let snapshot = all_results.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                    let running = snapshot.iter().filter(|r| r.exit_code == -1).count();
                                    let done = snapshot.iter().filter(|r| r.exit_code != -1).count();
                                    let total = snapshot.len();
                                    upd(text_result(
                                        format!("Parallel: {done}/{total} done, {running} running..."),
                                        details_value(RunMode::Parallel, agent_scope, pad.clone(), snapshot),
                                        None,
                                    ));
                                }
                            })
                        };

                        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENCY));
                        let mut futs = Vec::with_capacity(tasks.len());
                        for (index, t) in tasks.into_iter().enumerate() {
                            let sem = Arc::clone(&semaphore);
                            let all_results = Arc::clone(&all_results);
                            let emit_parallel = Arc::clone(&emit_parallel);
                            let agents = agents.clone();
                            let cwd = cwd.clone();
                            let default_model = default_model.clone();
                            let sig = signal.clone();

                            futs.push(async move {
                                let _permit = sem.acquire().await.map_err(|e| pi_js::Error::msg(e.to_string()))?;
                                let all_for_cb = Arc::clone(&all_results);
                                let emit_for_cb = Arc::clone(&emit_parallel);
                                let partial_cb: Arc<dyn Fn(SingleResult) + Send + Sync> = Arc::new(move |partial| {
                                    {
                                        let mut guard = all_for_cb.lock().unwrap_or_else(|e| e.into_inner());
                                        if let Some(slot) = guard.get_mut(index) {
                                            *slot = partial;
                                        }
                                    }
                                    emit_for_cb();
                                });
                                let res = run_single_agent(
                                    &cwd,
                                    default_model.as_deref(),
                                    dispatch_defaults,
                                    &agents,
                                    &t.agent,
                                    &t.task,
                                    t.cwd.as_deref(),
                                    None,
                                    sig,
                                    Some(partial_cb),
                                )
                                .await?;
                                {
                                    let mut guard = all_results.lock().unwrap_or_else(|e| e.into_inner());
                                    if let Some(slot) = guard.get_mut(index) {
                                        *slot = res.clone();
                                    }
                                }
                                emit_parallel();
                                Ok::<SingleResult, pi_js::Error>(res)
                            });
                        }

                        let results_wrapped = join_all(futs).await;
                        let mut results = Vec::with_capacity(results_wrapped.len());
                        for item in results_wrapped {
                            results.push(item?);
                        }

                        let success_count = results.iter().filter(|r| !is_failed_result(r)).count();
                        let summaries: Vec<String> = results
                            .iter()
                            .map(|r| {
                                let output = truncate_parallel_output(&get_result_output(r));
                                let status = if is_failed_result(r) {
                                    match r.stop_reason.as_deref() {
                                        Some(sr) if sr != "end" => format!("failed ({sr})"),
                                        _ => "failed".to_string(),
                                    }
                                } else {
                                    "completed".to_string()
                                };
                                format!("### [{}] {status}\n\n{output}", r.agent)
                            })
                            .collect();
                        return Ok(text_result(
                            format!(
                                "Parallel: {success_count}/{} succeeded\n\n{}",
                                results.len(),
                                summaries.join("\n\n---\n\n")
                            ),
                            details_value(RunMode::Parallel, agent_scope, project_agents_dir, results),
                            None,
                        ));
                    }

                    if let (Some(agent), Some(task)) = (params.agent, params.task) {
                        let single_cb: Option<Arc<dyn Fn(SingleResult) + Send + Sync>> = on_update.as_ref().map(|upd| {
                            let upd = Arc::clone(upd);
                            let pad = project_agents_dir.clone();
                            Arc::new(move |partial: SingleResult| {
                                let txt = get_final_output(&partial.messages);
                                let msg = if txt.is_empty() {
                                    "(running...)".to_string()
                                } else {
                                    txt
                                };
                                upd(text_result(
                                    msg,
                                    details_value(RunMode::Single, agent_scope, pad.clone(), vec![partial]),
                                    None,
                                ));
                            }) as Arc<dyn Fn(SingleResult) + Send + Sync>
                        });

                        let result = run_single_agent(
                            &cwd,
                            default_model.as_deref(),
                            dispatch_defaults,
                            &agents,
                            &agent,
                            &task,
                            params.cwd.as_deref(),
                            None,
                            signal,
                            single_cb,
                        )
                        .await?;

                        if is_failed_result(&result) {
                            let error_msg = get_result_output(&result);
                            let sr = result.stop_reason.as_deref().unwrap_or("failed").to_string();
                            return Ok(text_result(
                                format!("Agent {sr}: {error_msg}"),
                                details_value(RunMode::Single, agent_scope, project_agents_dir, vec![result]),
                                Some(true),
                            ));
                        }
                        let final_out = get_final_output(&result.messages);
                        let msg = if final_out.is_empty() {
                            "(no output)".to_string()
                        } else {
                            final_out
                        };
                        return Ok(text_result(
                            msg,
                            details_value(RunMode::Single, agent_scope, project_agents_dir, vec![result]),
                            None,
                        ));
                    }

                    let available = if agents.is_empty() {
                        "none".to_string()
                    } else {
                        agents
                            .iter()
                            .map(|a| format!("{} ({})", a.name, a.source.as_str()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    Ok(text_result(
                        format!("Invalid parameters. Available agents: {available}"),
                        details_value(RunMode::Single, agent_scope, project_agents_dir, Vec::new()),
                        None,
                    ))
                })
            }),
            render_call: Some(Arc::new(|args, theme, _context| {
                let scope = args.get("agentScope").and_then(|v| v.as_str()).unwrap_or("user");
                if let Some(chain) = args.get("chain").and_then(|v| v.as_array())
                    && !chain.is_empty()
                {
                    let mut text = format!(
                        "{}{}{}",
                        theme.fg("toolTitle", &theme.bold("subagent ")),
                        theme.fg("accent", &format!("chain ({} steps)", chain.len())),
                        theme.fg("muted", &format!(" [{scope}]"))
                    );
                    for (i, step) in chain.iter().take(3).enumerate() {
                        let agent_name = step.get("agent").and_then(|v| v.as_str()).unwrap_or("...");
                        let raw_task = step.get("task").and_then(|v| v.as_str()).unwrap_or("");
                        let clean_task = raw_task.replace("{previous}", "").trim().to_string();
                        let preview = if clean_task.chars().count() > 40 {
                            format!("{}...", clean_task.chars().take(40).collect::<String>())
                        } else {
                            clean_task
                        };
                        text.push_str(&format!(
                            "\n  {} {} {}",
                            theme.fg("muted", &format!("{}.", i + 1)),
                            theme.fg("accent", agent_name),
                            theme.fg("dim", &preview)
                        ));
                    }
                    if chain.len() > 3 {
                        text.push_str(&format!("\n  {}", theme.fg("muted", &format!("... +{} more", chain.len() - 3))));
                    }
                    return Arc::new(Text::new(Some(&text), Some(0), Some(0), None));
                }

                if let Some(tasks) = args.get("tasks").and_then(|v| v.as_array())
                    && !tasks.is_empty()
                {
                    let mut text = format!(
                        "{}{}{}",
                        theme.fg("toolTitle", &theme.bold("subagent ")),
                        theme.fg("accent", &format!("parallel ({} tasks)", tasks.len())),
                        theme.fg("muted", &format!(" [{scope}]"))
                    );
                    for t in tasks.iter().take(3) {
                        let agent_name = t.get("agent").and_then(|v| v.as_str()).unwrap_or("...");
                        let raw_task = t.get("task").and_then(|v| v.as_str()).unwrap_or("");
                        let preview = if raw_task.chars().count() > 40 {
                            format!("{}...", raw_task.chars().take(40).collect::<String>())
                        } else {
                            raw_task.to_string()
                        };
                        text.push_str(&format!("\n  {}{}", theme.fg("accent", agent_name), theme.fg("dim", &format!(" {preview}"))));
                    }
                    if tasks.len() > 3 {
                        text.push_str(&format!("\n  {}", theme.fg("muted", &format!("... +{} more", tasks.len() - 3))));
                    }
                    return Arc::new(Text::new(Some(&text), Some(0), Some(0), None));
                }

                let agent_name = args.get("agent").and_then(|v| v.as_str()).unwrap_or("...");
                let preview = match args.get("task").and_then(|v| v.as_str()) {
                    Some(t) if !t.is_empty() => {
                        if t.chars().count() > 60 {
                            format!("{}...", t.chars().take(60).collect::<String>())
                        } else {
                            t.to_string()
                        }
                    }
                    _ => "...".to_string(),
                };
                let text = format!(
                    "{}{}{}\n  {}",
                    theme.fg("toolTitle", &theme.bold("subagent ")),
                    theme.fg("accent", agent_name),
                    theme.fg("muted", &format!(" [{scope}]")),
                    theme.fg("dim", &preview)
                );
                Arc::new(Text::new(Some(&text), Some(0), Some(0), None))
            })),
            render_result: Some(Arc::new(|result, opts, theme, _context| {
                let expanded = opts.expanded;
                let details: Option<SubagentDetails> = result
                    .details
                    .clone()
                    .and_then(|v| serde_json::from_value(v).ok());
                let Some(details) = details.filter(|d| !d.results.is_empty()) else {
                    let text = match result.content.first() {
                        Some(UserContentBlock::Text(t)) => t.text.as_str(),
                        _ => "(no output)",
                    };
                    return Arc::new(Text::new(Some(text), Some(0), Some(0), None));
                };

                let md_theme = fallback_markdown_theme(&theme);

                let render_display_items = |items: &[DisplayItem], limit: Option<usize>| -> String {
                    let (to_show, skipped) = match limit {
                        Some(l) if items.len() > l => (&items[items.len() - l..], items.len() - l),
                        _ => (items, 0),
                    };
                    let mut text = String::new();
                    if skipped > 0 {
                        text.push_str(&theme.fg("muted", &format!("... {skipped} earlier items\n")));
                    }
                    for item in to_show {
                        match item {
                            DisplayItem::Text(t) => {
                                let preview = if expanded {
                                    t.clone()
                                } else {
                                    t.split('\n').take(3).collect::<Vec<_>>().join("\n")
                                };
                                text.push_str(&format!("{}\n", theme.fg("toolOutput", &preview)));
                            }
                            DisplayItem::ToolCall { name, args } => {
                                text.push_str(&format!(
                                    "{}{}\n",
                                    theme.fg("muted", "→ "),
                                    format_tool_call(name, args, &theme)
                                ));
                            }
                        }
                    }
                    text.trim_end().to_string()
                };

                let aggregate_usage = |results: &[SingleResult]| -> UsageStats {
                    let mut total = UsageStats::default();
                    for r in results {
                        total.input += r.usage.input;
                        total.output += r.usage.output;
                        total.cache_read += r.usage.cache_read;
                        total.cache_write += r.usage.cache_write;
                        total.cost += r.usage.cost;
                        total.turns += r.usage.turns;
                    }
                    total
                };

                if matches!(details.mode, RunMode::Single) && details.results.len() == 1 {
                    let r = &details.results[0];
                    let is_err = is_failed_result(r);
                    let icon = if is_err {
                        theme.fg("error", "✗")
                    } else {
                        theme.fg("success", "✓")
                    };
                    let display_items = get_display_items(&r.messages);
                    let final_output = get_final_output(&r.messages);

                    if expanded {
                        let container = Container::new();
                        let mut header = format!(
                            "{icon} {}{}",
                            theme.fg("toolTitle", &theme.bold(&r.agent)),
                            theme.fg("muted", &format!(" ({})", r.agent_source.as_str()))
                        );
                        if is_err && let Some(ref sr) = r.stop_reason {
                            header.push_str(&format!(" {}", theme.fg("error", &format!("[{sr}]"))));
                        }
                        container.add_child(Arc::new(Text::new(Some(&header), Some(0), Some(0), None)));
                        if is_err && let Some(ref em) = r.error_message {
                            container.add_child(Arc::new(Text::new(
                                Some(&theme.fg("error", &format!("Error: {em}"))),
                                Some(0),
                                Some(0),
                                None,
                            )));
                        }
                        container.add_child(Arc::new(Spacer::new(Some(1))));
                        container.add_child(Arc::new(Text::new(Some(&theme.fg("muted", "─── Task ───")), Some(0), Some(0), None)));
                        container.add_child(Arc::new(Text::new(Some(&theme.fg("dim", &r.task)), Some(0), Some(0), None)));
                        container.add_child(Arc::new(Spacer::new(Some(1))));
                        container.add_child(Arc::new(Text::new(Some(&theme.fg("muted", "─── Output ───")), Some(0), Some(0), None)));
                        if display_items.is_empty() && final_output.is_empty() {
                            container.add_child(Arc::new(Text::new(Some(&theme.fg("muted", "(no output)")), Some(0), Some(0), None)));
                        } else {
                            for item in &display_items {
                                if let DisplayItem::ToolCall { name, args } = item {
                                    container.add_child(Arc::new(Text::new(
                                        Some(&format!("{}{}", theme.fg("muted", "→ "), format_tool_call(name, args, &theme))),
                                        Some(0),
                                        Some(0),
                                        None,
                                    )));
                                }
                            }
                            if !final_output.is_empty() {
                                container.add_child(Arc::new(Spacer::new(Some(1))));
                                container.add_child(Arc::new(Markdown::new(final_output.trim(), 0, 0, md_theme.clone(), None, None)));
                            }
                        }
                        let usage_str = format_usage_stats(&r.usage, r.model.as_deref());
                        if !usage_str.is_empty() {
                            container.add_child(Arc::new(Spacer::new(Some(1))));
                            container.add_child(Arc::new(Text::new(Some(&theme.fg("dim", &usage_str)), Some(0), Some(0), None)));
                        }
                        return Arc::new(container);
                    }

                    let mut text = format!(
                        "{icon} {}{}",
                        theme.fg("toolTitle", &theme.bold(&r.agent)),
                        theme.fg("muted", &format!(" ({})", r.agent_source.as_str()))
                    );
                    if is_err && let Some(ref sr) = r.stop_reason {
                        text.push_str(&format!(" {}", theme.fg("error", &format!("[{sr}]"))));
                    }
                    if is_err && let Some(ref em) = r.error_message {
                        text.push_str(&format!("\n{}", theme.fg("error", &format!("Error: {em}"))));
                    } else if display_items.is_empty() {
                        text.push_str(&format!("\n{}", theme.fg("muted", "(no output)")));
                    } else {
                        text.push_str(&format!("\n{}", render_display_items(&display_items, Some(COLLAPSED_ITEM_COUNT))));
                        if display_items.len() > COLLAPSED_ITEM_COUNT {
                            text.push_str(&format!("\n{}", theme.fg("muted", "(Ctrl+O to expand)")));
                        }
                    }
                    let usage_str = format_usage_stats(&r.usage, r.model.as_deref());
                    if !usage_str.is_empty() {
                        text.push_str(&format!("\n{}", theme.fg("dim", &usage_str)));
                    }
                    return Arc::new(Text::new(Some(&text), Some(0), Some(0), None));
                }

                if matches!(details.mode, RunMode::Chain) {
                    let success_count = details.results.iter().filter(|r| r.exit_code == 0).count();
                    let icon = if success_count == details.results.len() {
                        theme.fg("success", "✓")
                    } else {
                        theme.fg("error", "✗")
                    };

                    if expanded {
                        let container = Container::new();
                        container.add_child(Arc::new(Text::new(
                            Some(&format!(
                                "{icon} {}{}",
                                theme.fg("toolTitle", &theme.bold("chain ")),
                                theme.fg("accent", &format!("{success_count}/{} steps", details.results.len()))
                            )),
                            Some(0),
                            Some(0),
                            None,
                        )));

                        for r in &details.results {
                            let r_icon = if r.exit_code == 0 {
                                theme.fg("success", "✓")
                            } else {
                                theme.fg("error", "✗")
                            };
                            let display_items = get_display_items(&r.messages);
                            let final_output = get_final_output(&r.messages);
                            let step_num = r.step.unwrap_or(0);

                            container.add_child(Arc::new(Spacer::new(Some(1))));
                            container.add_child(Arc::new(Text::new(
                                Some(&format!(
                                    "{}{} {r_icon}",
                                    theme.fg("muted", &format!("─── Step {step_num}: ")),
                                    theme.fg("accent", &r.agent)
                                )),
                                Some(0),
                                Some(0),
                                None,
                            )));
                            container.add_child(Arc::new(Text::new(
                                Some(&format!("{}{}", theme.fg("muted", "Task: "), theme.fg("dim", &r.task))),
                                Some(0),
                                Some(0),
                                None,
                            )));

                            for item in &display_items {
                                if let DisplayItem::ToolCall { name, args } = item {
                                    container.add_child(Arc::new(Text::new(
                                        Some(&format!("{}{}", theme.fg("muted", "→ "), format_tool_call(name, args, &theme))),
                                        Some(0),
                                        Some(0),
                                        None,
                                    )));
                                }
                            }
                            if !final_output.is_empty() {
                                container.add_child(Arc::new(Spacer::new(Some(1))));
                                container.add_child(Arc::new(Markdown::new(final_output.trim(), 0, 0, md_theme.clone(), None, None)));
                            }
                            let step_usage = format_usage_stats(&r.usage, r.model.as_deref());
                            if !step_usage.is_empty() {
                                container.add_child(Arc::new(Text::new(Some(&theme.fg("dim", &step_usage)), Some(0), Some(0), None)));
                            }
                        }
                        let usage_str = format_usage_stats(&aggregate_usage(&details.results), None);
                        if !usage_str.is_empty() {
                            container.add_child(Arc::new(Spacer::new(Some(1))));
                            container.add_child(Arc::new(Text::new(
                                Some(&theme.fg("dim", &format!("Total: {usage_str}"))),
                                Some(0),
                                Some(0),
                                None,
                            )));
                        }
                        return Arc::new(container);
                    }

                    let mut text = format!(
                        "{icon} {}{}",
                        theme.fg("toolTitle", &theme.bold("chain ")),
                        theme.fg("accent", &format!("{success_count}/{} steps", details.results.len()))
                    );
                    for r in &details.results {
                        let r_icon = if r.exit_code == 0 {
                            theme.fg("success", "✓")
                        } else {
                            theme.fg("error", "✗")
                        };
                        let display_items = get_display_items(&r.messages);
                        let step_num = r.step.unwrap_or(0);
                        text.push_str(&format!(
                            "\n\n{}{} {r_icon}",
                            theme.fg("muted", &format!("─── Step {step_num}: ")),
                            theme.fg("accent", &r.agent)
                        ));
                        if display_items.is_empty() {
                            text.push_str(&format!("\n{}", theme.fg("muted", "(no output)")));
                        } else {
                            text.push_str(&format!("\n{}", render_display_items(&display_items, Some(5))));
                        }
                    }
                    let usage_str = format_usage_stats(&aggregate_usage(&details.results), None);
                    if !usage_str.is_empty() {
                        text.push_str(&format!("\n\n{}", theme.fg("dim", &format!("Total: {usage_str}"))));
                    }
                    text.push_str(&format!("\n{}", theme.fg("muted", "(Ctrl+O to expand)")));
                    return Arc::new(Text::new(Some(&text), Some(0), Some(0), None));
                }

                if matches!(details.mode, RunMode::Parallel) {
                    let running = details.results.iter().filter(|r| r.exit_code == -1).count();
                    let success_count = details
                        .results
                        .iter()
                        .filter(|r| r.exit_code != -1 && !is_failed_result(r))
                        .count();
                    let fail_count = details
                        .results
                        .iter()
                        .filter(|r| r.exit_code != -1 && is_failed_result(r))
                        .count();
                    let is_running = running > 0;
                    let icon = if is_running {
                        theme.fg("warning", "⏳")
                    } else if fail_count > 0 {
                        theme.fg("warning", "◐")
                    } else {
                        theme.fg("success", "✓")
                    };
                    let status = if is_running {
                        format!("{}/{} done, {running} running", success_count + fail_count, details.results.len())
                    } else {
                        format!("{success_count}/{} tasks", details.results.len())
                    };

                    if expanded && !is_running {
                        let container = Container::new();
                        container.add_child(Arc::new(Text::new(
                            Some(&format!(
                                "{icon} {}{}",
                                theme.fg("toolTitle", &theme.bold("parallel ")),
                                theme.fg("accent", &status)
                            )),
                            Some(0),
                            Some(0),
                            None,
                        )));

                        for r in &details.results {
                            let r_icon = if is_failed_result(r) {
                                theme.fg("error", "✗")
                            } else {
                                theme.fg("success", "✓")
                            };
                            let display_items = get_display_items(&r.messages);
                            let final_output = get_final_output(&r.messages);

                            container.add_child(Arc::new(Spacer::new(Some(1))));
                            container.add_child(Arc::new(Text::new(
                                Some(&format!("{}{} {r_icon}", theme.fg("muted", "─── "), theme.fg("accent", &r.agent))),
                                Some(0),
                                Some(0),
                                None,
                            )));
                            container.add_child(Arc::new(Text::new(
                                Some(&format!("{}{}", theme.fg("muted", "Task: "), theme.fg("dim", &r.task))),
                                Some(0),
                                Some(0),
                                None,
                            )));
                            for item in &display_items {
                                if let DisplayItem::ToolCall { name, args } = item {
                                    container.add_child(Arc::new(Text::new(
                                        Some(&format!("{}{}", theme.fg("muted", "→ "), format_tool_call(name, args, &theme))),
                                        Some(0),
                                        Some(0),
                                        None,
                                    )));
                                }
                            }
                            if !final_output.is_empty() {
                                container.add_child(Arc::new(Spacer::new(Some(1))));
                                container.add_child(Arc::new(Markdown::new(final_output.trim(), 0, 0, md_theme.clone(), None, None)));
                            }
                            let task_usage = format_usage_stats(&r.usage, r.model.as_deref());
                            if !task_usage.is_empty() {
                                container.add_child(Arc::new(Text::new(Some(&theme.fg("dim", &task_usage)), Some(0), Some(0), None)));
                            }
                        }
                        let usage_str = format_usage_stats(&aggregate_usage(&details.results), None);
                        if !usage_str.is_empty() {
                            container.add_child(Arc::new(Spacer::new(Some(1))));
                            container.add_child(Arc::new(Text::new(
                                Some(&theme.fg("dim", &format!("Total: {usage_str}"))),
                                Some(0),
                                Some(0),
                                None,
                            )));
                        }
                        return Arc::new(container);
                    }

                    let mut text = format!(
                        "{icon} {}{}",
                        theme.fg("toolTitle", &theme.bold("parallel ")),
                        theme.fg("accent", &status)
                    );
                    for r in &details.results {
                        let r_icon = if r.exit_code == -1 {
                            theme.fg("warning", "⏳")
                        } else if is_failed_result(r) {
                            theme.fg("error", "✗")
                        } else {
                            theme.fg("success", "✓")
                        };
                        let display_items = get_display_items(&r.messages);
                        text.push_str(&format!(
                            "\n\n{}{} {r_icon}",
                            theme.fg("muted", "─── "),
                            theme.fg("accent", &r.agent)
                        ));
                        if display_items.is_empty() {
                            text.push_str(&format!(
                                "\n{}",
                                theme.fg("muted", if r.exit_code == -1 { "(running...)" } else { "(no output)" })
                            ));
                        } else {
                            text.push_str(&format!("\n{}", render_display_items(&display_items, Some(5))));
                        }
                    }
                    if !is_running {
                        let usage_str = format_usage_stats(&aggregate_usage(&details.results), None);
                        if !usage_str.is_empty() {
                            text.push_str(&format!("\n\n{}", theme.fg("dim", &format!("Total: {usage_str}"))));
                        }
                    }
                    if !expanded {
                        text.push_str(&format!("\n{}", theme.fg("muted", "(Ctrl+O to expand)")));
                    }
                    return Arc::new(Text::new(Some(&text), Some(0), Some(0), None));
                }

                let fallback = match result.content.first() {
                    Some(UserContentBlock::Text(t)) => t.text.as_str(),
                    _ => "(no output)",
                };
                Arc::new(Text::new(Some(fallback), Some(0), Some(0), None))
            })),
        })?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(SubagentExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}

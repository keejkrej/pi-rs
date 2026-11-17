//! Pure serializable view models and lossless pure data conversions for `pi-gui`.

use chrono::{DateTime, Datelike, Local, Utc};
use indexmap::IndexMap;
use pi_agent_core::types::AgentMessage;
use pi_ai::types::{AssistantContentBlock, Message, Model, StopReason, UserContentBlock, UserMessageContent};
use pi_coding_agent::SessionInfo;
use pi_coding_agent::core::model_runtime::ModelRuntime;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ThreadGroup {
    #[default]
    Today,
    Yesterday,
    ThisWeek,
    Earlier,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub model: Option<String>,
    pub updated_at_ms: i64,
    pub message_count: usize,
    pub is_streaming: bool,
    pub has_unread: bool,
    pub group: ThreadGroup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MessageRoleView {
    User,
    Assistant,
    System,
    ToolResult,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ToolCallStatus {
    Pending,
    Running,
    Success,
    Error,
    Aborted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCallViewModel {
    pub id: String,
    pub name: String,
    pub arguments_preview: String,
    pub output_preview: Option<String>,
    pub status: ToolCallStatus,
    pub duration_ms: Option<i64>,
    pub expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ContentBlockView {
    Text(String),
    Thinking(String),
    ToolCall(ToolCallViewModel),
    Image { mime_type: String, label: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageViewModel {
    pub id: String,
    pub role: MessageRoleView,
    pub timestamp_ms: i64,
    pub model_label: Option<String>,
    pub blocks: Vec<ContentBlockView>,
    pub is_error: bool,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveStreamState {
    pub turn_id: String,
    pub partial_text: String,
    pub partial_thinking: Option<String>,
    pub started_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SteeringQueueItem {
    pub id: String,
    pub text: String,
    pub is_follow_up: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffLineViewModel {
    pub kind: DiffLineKind,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffHunkViewModel {
    pub header: String,
    pub lines: Vec<DiffLineViewModel>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffFileViewModel {
    pub path: String,
    pub status: String,
    pub additions: usize,
    pub deletions: usize,
    pub hunks: Vec<DiffHunkViewModel>,
    pub collapsed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub branch: Option<String>,
    pub files: Vec<DiffFileViewModel>,
    pub selected_path: Option<String>,
    pub total_additions: usize,
    pub total_deletions: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileTreeEntryView {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
    pub modified: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenFileView {
    pub path: String,
    pub language: Option<String>,
    pub content: String,
    pub line_count: usize,
    pub dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerminalOutputLine {
    pub text: String,
    pub is_stderr: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TerminalSessionView {
    pub cwd: String,
    pub running: bool,
    pub last_exit_code: Option<i32>,
    pub lines: Vec<TerminalOutputLine>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PluginKind {
    Builtin,
    QuickJs,
    Mcp,
    Skill,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PluginOrigin {
    Builtin,
    Global,
    Project,
    Inline,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginViewModel {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: PluginKind,
    pub origin: PluginOrigin,
    pub enabled: bool,
    pub tool_count: usize,
    pub command_count: usize,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthProviderViewModel {
    pub id: String,
    pub name: String,
    pub authenticated: bool,
    pub source_label: Option<String>,
    pub supports_oauth: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelViewModel {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    pub context_window: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelUsageSummary {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub total_cost: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextUsageView {
    pub used_tokens: i64,
    pub window_tokens: i64,
    pub percent: f64,
    pub thinking_level: String,
    pub session_usage: ModelUsageSummary,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionDialogView {
    pub request_id: String,
    pub title: String,
    pub message: Option<String>,
    pub options: Vec<String>,
    pub allow_input: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ToastTone {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToastMessage {
    pub id: u64,
    pub message: String,
    pub tone: ToastTone,
}

/// Complete offline showcase bundle applied by [`crate::state::DemoScene`].
#[derive(Clone, Debug, PartialEq)]
pub struct DemoSceneBundle {
    pub threads: Vec<ThreadSummary>,
    pub active_thread_id: Option<String>,
    pub messages: Vec<MessageViewModel>,
    pub diff: DiffSummary,
    pub files: Vec<FileTreeEntryView>,
    pub open_file: Option<OpenFileView>,
    pub terminal: TerminalSessionView,
    pub plugins: Vec<PluginViewModel>,
    pub auth_providers: Vec<AuthProviderViewModel>,
    pub models: Vec<ModelViewModel>,
    pub active_model: Option<ModelViewModel>,
    pub context_usage: ContextUsageView,
}

/// Truncates a string safely at UTF-8 char boundaries for previews and sidebar labels.
#[must_use]
pub fn truncate_preview(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let prefix: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{prefix}…")
}

/// Classifies a millisecond timestamp into sidebar recency buckets without disk I/O.
#[must_use]
pub fn classify_thread_group(timestamp_ms: i64, now_ms: i64) -> ThreadGroup {
    let Some(dt) = DateTime::<Utc>::from_timestamp_millis(timestamp_ms) else {
        return ThreadGroup::Earlier;
    };
    let Some(now_dt) = DateTime::<Utc>::from_timestamp_millis(now_ms) else {
        return ThreadGroup::Earlier;
    };
    let local_dt = dt.with_timezone(&Local).date_naive();
    let local_now = now_dt.with_timezone(&Local).date_naive();
    let days = (local_now - local_dt).num_days();
    if days <= 0 {
        ThreadGroup::Today
    } else if days == 1 {
        ThreadGroup::Yesterday
    } else if days < 7 && local_now.iso_week() == local_dt.iso_week() {
        ThreadGroup::ThisWeek
    } else {
        ThreadGroup::Earlier
    }
}

/// Converts a `SessionInfo` header summary into a sidebar [`ThreadSummary`].
#[must_use]
pub fn session_info_to_thread_summary(info: &SessionInfo, is_streaming: bool, now_ms: i64) -> ThreadSummary {
    let raw_title = info
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| info.first_message.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or("Untitled thread");
    ThreadSummary {
        id: info.id.clone(),
        title: truncate_preview(raw_title, 56),
        cwd: info.cwd.clone(),
        model: None,
        updated_at_ms: info.modified,
        message_count: info.message_count,
        is_streaming,
        has_unread: false,
        group: classify_thread_group(info.modified, now_ms),
    }
}

/// Infers a syntax-highlighter language identifier from a workspace file path.
#[must_use]
pub fn infer_language_from_path(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next()?;
    match ext {
        "rs" => Some("rust"),
        "ts" | "mts" | "cts" | "tsx" => Some("typescript"),
        "js" | "mjs" | "cjs" | "jsx" => Some("javascript"),
        "json" | "jsonl" => Some("json"),
        "toml" => Some("toml"),
        "md" | "markdown" => Some("markdown"),
        "sh" | "bash" | "zsh" => Some("bash"),
        "py" => Some("python"),
        "yaml" | "yml" => Some("yaml"),
        "css" => Some("css"),
        "html" => Some("html"),
        "sql" => Some("sql"),
        "wgsl" => Some("wgsl"),
        _ => None,
    }
}

fn format_user_blocks(content: &UserMessageContent) -> Vec<ContentBlockView> {
    match content {
        UserMessageContent::Text(t) => {
            if t.is_empty() {
                Vec::new()
            } else {
                vec![ContentBlockView::Text(t.clone())]
            }
        }
        UserMessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(t) if !t.text.is_empty() => Some(ContentBlockView::Text(t.text.clone())),
                UserContentBlock::Image(img) => Some(ContentBlockView::Image {
                    mime_type: img.mime_type.clone(),
                    label: format!("Image ({})", img.mime_type),
                }),
                _ => None,
            })
            .collect(),
    }
}

fn format_tool_result_text(blocks: &[UserContentBlock]) -> String {
    let parts: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    truncate_preview(&parts.join("\n"), 480)
}

fn mk_msg(
    idx: usize,
    role: MessageRoleView,
    timestamp_ms: i64,
    model_label: Option<String>,
    blocks: Vec<ContentBlockView>,
    is_error: bool,
    error_message: Option<String>,
) -> MessageViewModel {
    MessageViewModel {
        id: format!("msg-{idx}"),
        role,
        timestamp_ms,
        model_label,
        blocks,
        is_error,
        error_message,
    }
}

/// Converts `AgentSession` transcript messages into renderable [`MessageViewModel`] items.
#[must_use]
pub fn convert_agent_messages(messages: &[AgentMessage]) -> Vec<MessageViewModel> {
    let mut out: Vec<MessageViewModel> = Vec::new();
    let mut tool_index: IndexMap<String, (usize, usize)> = IndexMap::new();
    for (idx, msg) in messages.iter().enumerate() {
        match msg {
            AgentMessage::Message(Message::User(u)) => out.push(mk_msg(
                idx,
                MessageRoleView::User,
                u.timestamp,
                None,
                format_user_blocks(&u.content),
                false,
                None,
            )),
            AgentMessage::Message(Message::Assistant(a)) => {
                let msg_pos = out.len();
                let mut blocks = Vec::new();
                for block in &a.content {
                    match block {
                        AssistantContentBlock::Text(t) if !t.text.is_empty() => {
                            blocks.push(ContentBlockView::Text(t.text.clone()))
                        }
                        AssistantContentBlock::Thinking(th) if !th.thinking.is_empty() => {
                            blocks.push(ContentBlockView::Thinking(th.thinking.clone()))
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            let obj = tc.arguments.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                            let args_str = pi_js::json::stringify(&serde_json::Value::Object(obj));
                            tool_index.insert(tc.id.clone(), (msg_pos, blocks.len()));
                            blocks.push(ContentBlockView::ToolCall(ToolCallViewModel {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                arguments_preview: truncate_preview(&args_str, 120),
                                output_preview: None,
                                status: ToolCallStatus::Pending,
                                duration_ms: None,
                                expanded: false,
                            }));
                        }
                        _ => {}
                    }
                }
                let is_err = a.stop_reason == StopReason::Error || a.error_message.is_some();
                out.push(mk_msg(
                    idx,
                    MessageRoleView::Assistant,
                    a.timestamp,
                    Some(format!("{} / {}", a.provider, a.model)),
                    blocks,
                    is_err,
                    a.error_message.clone(),
                ));
            }
            AgentMessage::Message(Message::ToolResult(tr)) => {
                let preview = format_tool_result_text(&tr.content);
                if let Some(&(mp, bp)) = tool_index.get(&tr.tool_call_id)
                    && let Some(ContentBlockView::ToolCall(tc)) = out.get_mut(mp).and_then(|m| m.blocks.get_mut(bp))
                {
                    tc.output_preview = (!preview.is_empty()).then_some(preview);
                    tc.status = if tr.is_error {
                        ToolCallStatus::Error
                    } else {
                        ToolCallStatus::Success
                    };
                    continue;
                }
                out.push(mk_msg(
                    idx,
                    MessageRoleView::ToolResult,
                    tr.timestamp,
                    Some(tr.tool_name.clone()),
                    vec![ContentBlockView::Text(preview)],
                    tr.is_error,
                    None,
                ));
            }
            AgentMessage::Message(Message::System(sys)) => {
                let text = match &sys.content {
                    pi_ai::types::SystemMessageContent::Text(s) => s.clone(),
                    pi_ai::types::SystemMessageContent::Blocks(bs) => {
                        bs.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n")
                    }
                };
                if !text.is_empty() {
                    out.push(mk_msg(
                        idx,
                        MessageRoleView::System,
                        sys.timestamp,
                        Some("System".to_string()),
                        vec![ContentBlockView::Text(text)],
                        false,
                        None,
                    ));
                }
            }
            AgentMessage::Custom(val) => {
                let txt = val
                    .get("content")
                    .and_then(|c| c.as_str())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| pi_js::json::stringify(val));
                if !txt.is_empty() {
                    let lbl = val.get("role").and_then(|r| r.as_str()).map(ToOwned::to_owned);
                    out.push(mk_msg(
                        idx,
                        MessageRoleView::System,
                        0,
                        lbl,
                        vec![ContentBlockView::Text(truncate_preview(&txt, 600))],
                        false,
                        None,
                    ));
                }
            }
        }
    }
    out
}

/// Builds rich offline showcase fixtures shared across `--demo` and `--scene <id>`.
#[must_use]
pub fn build_demo_bundle(cwd: &str) -> DemoSceneBundle {
    let md_body = concat!(
        "## Bridge & QuickJS Pipeline Ready\n\n- Connected `AgentSessionRuntime` off-thread\n- Streaming text, thinking deltas, and tool cards\n\n",
        "```rust\npub fn palette(dark: bool) -> WakuPalette {\n    if dark { WakuPalette::dark() } else { WakuPalette::light() }\n}\n```\n\n",
        "```ts\nexport default function (pi: ExtensionAPI) {\n  pi.on(\"input\", (ev) => ({ action: \"continue\", text: ev.text }));\n}\n```\n\n",
        "```mermaid\nflowchart LR\n  RuneUI --> BridgeCommand --> AgentSession --> BridgeEvent\n```"
    );
    let models = vec![
        ModelViewModel {
            provider: "anthropic".into(),
            id: "claude-sonnet-4-5".into(),
            name: "Claude Sonnet 4.5".into(),
            reasoning: true,
            context_window: 200_000,
        },
        ModelViewModel {
            provider: "openai".into(),
            id: "gpt-5".into(),
            name: "GPT-5".into(),
            reasoning: true,
            context_window: 256_000,
        },
    ];
    let threads = vec![
        ThreadSummary {
            id: "demo-session-1".into(),
            title: "Wire QuickJS plugins and Rune diff panel".into(),
            cwd: cwd.into(),
            model: Some("claude-sonnet-4-5".into()),
            updated_at_ms: 1_763_370_000_000,
            message_count: 3,
            is_streaming: false,
            has_unread: false,
            group: ThreadGroup::Today,
        },
        ThreadSummary {
            id: "demo-session-2".into(),
            title: "Verify Waku graphite palette parity".into(),
            cwd: cwd.into(),
            model: Some("gpt-5".into()),
            updated_at_ms: 1_763_280_000_000,
            message_count: 5,
            is_streaming: false,
            has_unread: false,
            group: ThreadGroup::Yesterday,
        },
    ];
    let tc = ContentBlockView::ToolCall(ToolCallViewModel {
        id: "tc-1".into(),
        name: "read".into(),
        arguments_preview: r#"{"path":"crates/pi-gui/src/bridge.rs"}"#.into(),
        output_preview: Some("Read 280 lines from crates/pi-gui/src/bridge.rs".into()),
        status: ToolCallStatus::Success,
        duration_ms: Some(14),
        expanded: true,
    });
    let messages = vec![
        mk_msg(
            1,
            MessageRoleView::User,
            1_763_369_900_000,
            None,
            vec![ContentBlockView::Text(
                "Inspect workspace diff and QuickJS .ts extensions.".into(),
            )],
            false,
            None,
        ),
        mk_msg(
            2,
            MessageRoleView::Assistant,
            1_763_369_960_000,
            Some("anthropic / claude-sonnet-4-5".into()),
            vec![
                ContentBlockView::Thinking("Checking git diff hunks and QuickJS extensions in .pi/extensions/".into()),
                tc,
                ContentBlockView::Text(md_body.into()),
            ],
            false,
            None,
        ),
    ];
    let diff = parse_git_diff(
        Some("swarm/codemode-w4".into()),
        " M crates/pi-gui/src/bridge.rs\n",
        "diff --git a/crates/pi-gui/src/bridge.rs b/crates/pi-gui/src/bridge.rs\n@@ -10,4 +10,5 @@ impl GuiBridge {\n  let runtime = create_agent_session_runtime(...)\n-// placeholder stub\n+session.subscribe(forward_events);\n+session.bind_extensions(bindings).await?;\n",
    );
    let files = vec![
        FileTreeEntryView {
            path: "crates".into(),
            name: "crates".into(),
            is_dir: true,
            depth: 0,
            expanded: true,
            modified: true,
        },
        FileTreeEntryView {
            path: "crates/pi-gui/src/bridge.rs".into(),
            name: "bridge.rs".into(),
            is_dir: false,
            depth: 1,
            expanded: false,
            modified: true,
        },
        FileTreeEntryView {
            path: "package/examples/extensions/input-transform.ts".into(),
            name: "input-transform.ts".into(),
            is_dir: false,
            depth: 1,
            expanded: false,
            modified: false,
        },
    ];
    let plugins = vec![
        PluginViewModel {
            id: "ext:input-transform".into(),
            name: "input-transform.ts".into(),
            description: "QuickJS WASI extension (.ts)".into(),
            kind: PluginKind::QuickJs,
            origin: PluginOrigin::Project,
            enabled: true,
            tool_count: 1,
            command_count: 2,
            error: None,
        },
        PluginViewModel {
            id: "ext:codemode".into(),
            name: "codemode".into(),
            description: "Built-in JavaScript tool sandbox".into(),
            kind: PluginKind::Builtin,
            origin: PluginOrigin::Builtin,
            enabled: true,
            tool_count: 1,
            command_count: 0,
            error: None,
        },
        PluginViewModel {
            id: "mcp:filesystem".into(),
            name: "mcp-filesystem".into(),
            description: "MCP workspace tools".into(),
            kind: PluginKind::Mcp,
            origin: PluginOrigin::Global,
            enabled: true,
            tool_count: 4,
            command_count: 1,
            error: None,
        },
    ];
    DemoSceneBundle {
        active_model: models.first().cloned(),
        threads,
        active_thread_id: Some("demo-session-1".into()),
        messages,
        diff,
        files,
        open_file: Some(OpenFileView {
            path: "crates/pi-gui/src/theme.rs".into(), language: Some("rust".into()), dirty: false, line_count: 3,
            content: "pub fn palette(dark: bool) -> WakuPalette {\n    if dark { WakuPalette::dark() } else { WakuPalette::light() }\n}".into(),
        }),
        terminal: TerminalSessionView {
            cwd: cwd.into(), running: false, last_exit_code: Some(0),
            lines: vec![
                TerminalOutputLine { text: "$ cargo check -p pi-gui --all-targets".into(), is_stderr: false },
                TerminalOutputLine { text: "Finished `dev` profile target(s)".into(), is_stderr: false },
            ],
        },
        plugins,
        auth_providers: vec![
            AuthProviderViewModel { id: "anthropic".into(), name: "Anthropic".into(), authenticated: true, source_label: Some("environment".into()), supports_oauth: true },
            AuthProviderViewModel { id: "openai".into(), name: "OpenAI".into(), authenticated: false, source_label: None, supports_oauth: true },
        ],
        models,
        context_usage: ContextUsageView {
            used_tokens: 18_420, window_tokens: 200_000, percent: 9.21, thinking_level: "medium".into(),
            session_usage: ModelUsageSummary { input_tokens: 14_200, output_tokens: 4_220, cache_read_tokens: 9_600, total_cost: 0.042 },
        },
    }
}

/// Converts runtime models into GUI view models plus optional active selection.
#[must_use]
pub fn build_models_snapshot(
    runtime: &ModelRuntime,
    active: Option<&Model>,
) -> (Vec<ModelViewModel>, Option<ModelViewModel>) {
    let vm = |m: &Model| ModelViewModel {
        provider: m.provider.clone(),
        id: m.id.clone(),
        name: m.name.clone(),
        reasoning: m.reasoning,
        context_window: m.context_window,
    };
    (runtime.get_models(None).iter().map(vm).collect(), active.map(vm))
}

/// Converts configured providers and auth states into GUI view models.
#[must_use]
pub fn build_auth_snapshot(runtime: &ModelRuntime) -> Vec<AuthProviderViewModel> {
    runtime
        .get_providers()
        .into_iter()
        .map(|p| {
            let st = runtime.get_provider_auth_status(p.id());
            AuthProviderViewModel {
                id: p.id().into(),
                name: p.name().into(),
                authenticated: st.configured,
                source_label: st.label,
                supports_oauth: p.auth().oauth.is_some(),
            }
        })
        .collect()
}

pub use crate::bridge::parse_git_diff;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_user_assistant_and_tool_result_messages() {
        let msgs = vec![AgentMessage::Message(Message::User(pi_ai::types::UserMessage {
            content: UserMessageContent::Text("hello".into()),
            timestamp: 10,
        }))];
        let vm = convert_agent_messages(&msgs);
        assert_eq!(vm.len(), 1);
        assert_eq!(vm[0].role, MessageRoleView::User);
        assert_eq!(vm[0].blocks, vec![ContentBlockView::Text("hello".into())]);
        assert_eq!(infer_language_from_path("src/main.rs"), Some("rust"));
        assert_eq!(infer_language_from_path("ext.ts"), Some("typescript"));
    }
}

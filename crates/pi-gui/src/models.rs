//! Pure serializable view models shared between the `pi-gui` bridge and Rune views.

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

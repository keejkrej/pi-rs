//! Port of packages/coding-agent/src/modes/interactive/interactive-mode.ts#L1-L432

// @generated-mods begin (scaffold-owned, do not edit)
mod part_2;
pub use part_2::*;
mod part_3;
pub use part_3::*;
mod part_4;
pub use part_4::*;
mod part_5;
pub use part_5::*;
mod part_6;
pub use part_6::*;
// @generated-mods end

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::core::keybindings::{KEYBINDINGS, KeybindingsManager};
use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_agent_core::AgentMessage;
use pi_ai::auth::types::AuthType;
use pi_ai::{ImageContent, Model, ModelThinkingLevel, Usage};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::autocomplete::AutocompleteProvider;
use pi_tui::components::editor::EditorCursor;
use pi_tui::editor_component::{EditorBorderColor, EditorTextCallback};
use pi_tui::tui::{OverlayHandle, OverlayOptions, TUI};
use pi_tui::{
    AutocompleteItem, Component, Container, EditorComponent, ScrollViewScrollbar, Terminal, TuiMouseEvent,
    TuiMouseEventResult, TuiMouseEventType, WheelScrollLines, fuzzy_filter,
};
use serde_json::Value;

use crate::modes::interactive::bug_report::BugReportSession;
use crate::modes::interactive::components::assistant_message::AssistantMessageComponent;
use crate::modes::interactive::components::bash_execution::BashExecutionComponent;
use crate::modes::interactive::components::custom_editor::CustomEditor;
use crate::modes::interactive::components::custom_entry::EntryRenderer;
use crate::modes::interactive::components::custom_message::MessageRenderer;
use crate::modes::interactive::components::footer::{FooterComponent, FooterSession, ReadonlyFooterDataProvider};
use crate::modes::interactive::components::markdown_transform::MarkdownTransformer;
use crate::modes::interactive::components::mermaid::MermaidRenderingMode;
use crate::modes::interactive::components::model_selector::ScopedModelItem;
use crate::modes::interactive::components::oauth_selector::{AuthSelectorProvider, format_auth_selector_provider_type};
use crate::modes::interactive::components::session_selector::SessionListProgress;
use crate::modes::interactive::components::session_selector_search::SessionInfo;
use crate::modes::interactive::components::settings_selector::{
    CacheWarmingMode, DefaultProjectTrust, DoubleEscapeAction, FullscreenExitOutput, QuietStartup, SteeringMode,
    TreeFilterMode, TuiMode, WarningSettings,
};
use crate::modes::interactive::components::status_indicator::StatusIndicator;
use crate::modes::interactive::components::themed_text::ThemedText;
use crate::modes::interactive::components::tool_execution::{ToolExecutionComponent, ToolRenderers};
use crate::modes::interactive::components::tree_selector::{SessionEntry, SessionTreeNode};
use crate::modes::interactive::components::user_message_selector::UserMessageItem;
use crate::modes::interactive::model_catalog_refresh::ModelCatalogRuntime;
use crate::modes::interactive::session_share::{SessionShareDestination, ShareSession};
use crate::modes::interactive::theme::theme::{Theme, ThemeInfo};
use crate::modes::interactive::theme::theme_controller::{InteractiveThemeController, ThemeSettingsSource};
use crate::modes::interactive::tui_renderer::{InteractiveTui, InteractiveTuiReference};
use crate::modes::json_event::{AgentSessionEvent, CompactionReason};

pub(crate) use crate::config::{
    APP_NAME, APP_TITLE, CONFIG_DIR_NAME, VERSION, get_agent_dir, get_auth_path, get_debug_log_path, get_docs_path,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
    Collision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSessionRuntimeDiagnostic {
    pub diagnostic_type: DiagnosticLevel,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceInfo {
    pub path: String,
    pub source: String,
    pub scope: String,
    pub origin: String,
    pub base_dir: Option<String>,
    pub changelog_path: Option<String>,
}

impl Default for SourceInfo {
    fn default() -> Self {
        Self {
            path: String::new(),
            source: "local".to_string(),
            scope: "project".to_string(),
            origin: "top-level".to_string(),
            base_dir: None,
            changelog_path: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceCollision {
    pub name: String,
    pub winner_path: String,
    pub loser_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceDiagnostic {
    pub diagnostic_type: DiagnosticLevel,
    pub message: String,
    pub path: Option<String>,
    pub collision: Option<ResourceCollision>,
}

#[derive(Clone, Debug)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub source_info: Option<SourceInfo>,
}

#[derive(Clone, Debug)]
pub struct PromptTemplateInfo {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub file_path: String,
    pub source_info: Option<SourceInfo>,
}

#[derive(Clone)]
pub struct LoadedThemeResource {
    pub theme: Theme,
    pub source_path: Option<String>,
    pub source_info: Option<SourceInfo>,
}

#[derive(Clone, Debug)]
pub struct LoadedExtensionInfo {
    pub path: String,
    pub hidden: bool,
    pub source_info: Option<SourceInfo>,
}

#[derive(Clone, Debug)]
pub struct ExtensionLoadMessage {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct LoadedExtensionsResult {
    pub extensions: Vec<LoadedExtensionInfo>,
    pub errors: Vec<ExtensionLoadMessage>,
    pub warnings: Vec<ExtensionLoadMessage>,
}

#[derive(Clone)]
pub struct RegisteredExtensionCommand {
    pub name: String,
    pub invocation_name: String,
    pub description: Option<String>,
    pub source_info: SourceInfo,
    pub get_argument_completions: Option<pi_tui::autocomplete::GetArgumentCompletions>,
}

#[derive(Clone)]
pub struct RegisteredExtensionShortcut {
    pub key: String,
    pub description: Option<String>,
    pub extension_path: String,
    pub handler: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>,
}

#[derive(Clone, Debug, Default)]
pub struct ToolStatus {
    pub is_warning: bool,
    pub message: String,
}

impl From<crate::utils::tools_manager::ToolStatus> for ToolStatus {
    fn from(status: crate::utils::tools_manager::ToolStatus) -> Self {
        Self {
            is_warning: status.r#type == crate::utils::tools_manager::ToolStatusKind::Warning,
            message: status.message,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockedKind {
    Permission,
    Question,
    Auth,
}

impl BlockedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedStatus {
    pub kind: BlockedKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramStatusState {
    Idle,
    Working,
    Blocked,
    Done,
    Error,
    Clear,
}

impl ProgramStatusState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Error => "error",
            Self::Clear => "clear",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: ProgramStatusState,
    pub app: Option<String>,
    pub kind: Option<BlockedKind>,
    pub message: Option<String>,
}

fn replace_control_chars(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            let code = c as u32;
            if code < 32 || code == 127 { ' ' } else { c }
        })
        .collect()
}

fn truncate_utf8_bytes(input: &str, max_bytes: usize) -> &str {
    if input.len() <= max_bytes {
        return input;
    }
    let mut end = 0;
    for (idx, ch) in input.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    &input[..end]
}

fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn first_line_or_error(text: Option<&str>) -> String {
    text.and_then(|t| t.split(['\r', '\n']).next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Error")
        .to_string()
}

pub fn format_program_status(status: &ProgramStatus) -> String {
    let mut pairs = vec![format!("state={}", status.state.as_str())];
    if let Some(app) = &status.app
        && !app.is_empty()
        && app.len() <= 32
        && app
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-'))
    {
        pairs.push(format!("app={app}"));
    }
    if status.state == ProgramStatusState::Blocked
        && let Some(kind) = status.kind
    {
        pairs.push(format!("kind={}", kind.as_str()));
    }
    let cleaned = replace_control_chars(status.message.as_deref().unwrap_or(""));
    let trimmed = cleaned.trim();
    let msg = truncate_utf8_bytes(trimmed, 2048);
    if !msg.is_empty() {
        pairs.push(format!("msg={}", encode_base64(msg.as_bytes())));
    }
    format!("\x1b]7501;{}\x1b\\", pairs.join(":"))
}

struct ProgramStatusStateInner {
    run_active: bool,
    compacting: bool,
    run_result: ProgramStatus,
    resting_status: ProgramStatus,
    blocked: IndexMap<String, BlockedStatus>,
    last_report_key: Option<String>,
    last_status: Option<ProgramStatus>,
}

/// Reports interactive-mode state to the terminal (OSC 7501).
#[derive(Clone)]
pub struct ProgramStatusReporter {
    get_terminal: Arc<dyn Fn() -> Arc<dyn Terminal> + Send + Sync>,
    get_session_name: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    state: Arc<Mutex<ProgramStatusStateInner>>,
}

impl ProgramStatusReporter {
    pub fn new(
        get_terminal: Arc<dyn Fn() -> Arc<dyn Terminal> + Send + Sync>,
        get_session_name: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Self {
        Self {
            get_terminal,
            get_session_name,
            state: Arc::new(Mutex::new(ProgramStatusStateInner {
                run_active: false,
                compacting: false,
                run_result: ProgramStatus {
                    state: ProgramStatusState::Done,
                    app: None,
                    kind: None,
                    message: None,
                },
                resting_status: ProgramStatus {
                    state: ProgramStatusState::Idle,
                    app: None,
                    kind: None,
                    message: None,
                },
                blocked: IndexMap::new(),
                last_report_key: None,
                last_status: None,
            })),
        }
    }

    pub fn handle_event(&self, event: &AgentSessionEvent) {
        let should_report = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            match event {
                AgentSessionEvent::AgentStart => {
                    st.run_active = true;
                    st.run_result = ProgramStatus {
                        state: ProgramStatusState::Done,
                        app: None,
                        kind: None,
                        message: None,
                    };
                    true
                }
                AgentSessionEvent::MessageEnd { message } => {
                    if let AgentMessage::Message(pi_ai::Message::Assistant(assistant)) = message {
                        st.run_result = if assistant.stop_reason == pi_ai::StopReason::Error {
                            ProgramStatus {
                                state: ProgramStatusState::Error,
                                app: None,
                                kind: None,
                                message: Some(first_line_or_error(assistant.error_message.as_deref())),
                            }
                        } else {
                            ProgramStatus {
                                state: ProgramStatusState::Done,
                                app: None,
                                kind: None,
                                message: None,
                            }
                        };
                        true
                    } else {
                        false
                    }
                }
                AgentSessionEvent::CompactionStart { .. } => {
                    st.compacting = true;
                    true
                }
                AgentSessionEvent::CompactionEnd {
                    reason,
                    aborted,
                    error_message,
                    ..
                } => {
                    st.compacting = false;
                    if st.run_active {
                        if *aborted {
                            st.run_result = ProgramStatus {
                                state: ProgramStatusState::Idle,
                                app: None,
                                kind: None,
                                message: None,
                            };
                        } else if let Some(err) = error_message {
                            st.run_result = ProgramStatus {
                                state: ProgramStatusState::Error,
                                app: None,
                                kind: None,
                                message: Some(first_line_or_error(Some(err))),
                            };
                        }
                    } else if *aborted {
                        st.resting_status = ProgramStatus {
                            state: ProgramStatusState::Idle,
                            app: None,
                            kind: None,
                            message: None,
                        };
                    } else if *reason == CompactionReason::Manual {
                        st.resting_status = if let Some(err) = error_message {
                            ProgramStatus {
                                state: ProgramStatusState::Error,
                                app: None,
                                kind: None,
                                message: Some(first_line_or_error(Some(err))),
                            }
                        } else {
                            ProgramStatus {
                                state: ProgramStatusState::Done,
                                app: None,
                                kind: None,
                                message: None,
                            }
                        };
                    }
                    true
                }
                AgentSessionEvent::AgentSettled => {
                    st.run_active = false;
                    st.resting_status = st.run_result.clone();
                    true
                }
                AgentSessionEvent::SessionInfoChanged { .. } => true,
                _ => false,
            }
        };
        if should_report {
            self.report();
        }
    }

    pub fn set_blocked(&self, source: &str, status: Option<BlockedStatus>) {
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.blocked.shift_remove(source);
            if let Some(status) = status {
                st.blocked.insert(source.to_string(), status);
            }
        }
        self.report();
    }

    pub fn reset(&self) {
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.run_active = false;
            st.compacting = false;
            st.run_result = ProgramStatus {
                state: ProgramStatusState::Done,
                app: None,
                kind: None,
                message: None,
            };
            st.resting_status = ProgramStatus {
                state: ProgramStatusState::Idle,
                app: None,
                kind: None,
                message: None,
            };
        }
        self.report();
    }

    pub fn last_status(&self) -> Option<ProgramStatus> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).last_status.clone()
    }

    pub fn report(&self) {
        let session_name = (self.get_session_name)();
        let status_opt = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut status = if let Some(blocked) = st.blocked.values().last() {
                ProgramStatus {
                    state: ProgramStatusState::Blocked,
                    app: Some((*APP_NAME).to_string()),
                    kind: Some(blocked.kind),
                    message: Some(blocked.message.clone()),
                }
            } else if st.compacting {
                ProgramStatus {
                    state: ProgramStatusState::Working,
                    app: Some((*APP_NAME).to_string()),
                    kind: None,
                    message: Some("Compacting context".to_string()),
                }
            } else if st.run_active {
                ProgramStatus {
                    state: ProgramStatusState::Working,
                    app: Some((*APP_NAME).to_string()),
                    kind: None,
                    message: session_name,
                }
            } else {
                let mut resting = st.resting_status.clone();
                resting.app = Some((*APP_NAME).to_string());
                if resting.state == ProgramStatusState::Done {
                    resting.message = session_name;
                }
                resting
            };
            if status.app.is_none() {
                status.app = Some((*APP_NAME).to_string());
            }
            let key = format!(
                "{}:{}:{}:{}",
                status.state.as_str(),
                status.app.as_deref().unwrap_or(""),
                status.kind.map(BlockedKind::as_str).unwrap_or(""),
                status.message.as_deref().unwrap_or("")
            );
            if st.last_report_key.as_deref() == Some(key.as_str()) {
                None
            } else {
                st.last_report_key = Some(key);
                st.last_status = Some(status.clone());
                Some(status)
            }
        };
        if let Some(status) = status_opt
            && pi_js::env::var("PI_PROGRAM_STATUS").as_deref() == Some("1")
        {
            let term = (self.get_terminal)();
            term.write(&format_program_status(&status));
        }
    }
}

/// Trait for components that support expand/collapse toggling.
pub trait Expandable: Send + Sync {
    fn set_expanded(&self, expanded: bool);
}

pub(crate) struct BashExecutionExpandable(
    pub(crate) crate::modes::interactive::components::bash_execution::BashExecutionComponent,
);

impl Expandable for BashExecutionExpandable {
    fn set_expanded(&self, expanded: bool) {
        self.0.set_expanded(expanded);
    }
}

#[derive(Clone)]
pub struct ExpandableText {
    inner: ThemedText,
    expanded: Arc<AtomicBool>,
}

impl ExpandableText {
    pub fn new(
        get_collapsed_text: Arc<dyn Fn() -> String + Send + Sync>,
        get_expanded_text: Arc<dyn Fn() -> String + Send + Sync>,
        expanded: bool,
        padding_x: i64,
        padding_y: i64,
    ) -> Self {
        let state = Arc::new(AtomicBool::new(expanded));
        let state_c = Arc::clone(&state);
        let text = ThemedText::new(
            Arc::new(move || {
                if state_c.load(Ordering::Relaxed) {
                    get_expanded_text()
                } else {
                    get_collapsed_text()
                }
            }),
            Some(padding_x),
            Some(padding_y),
        );
        Self {
            inner: text,
            expanded: state,
        }
    }

    pub fn set_expanded(&self, expanded: bool) {
        self.expanded.store(expanded, Ordering::Relaxed);
        self.inner.invalidate();
    }
}

impl Expandable for ExpandableText {
    fn set_expanded(&self, expanded: bool) {
        ExpandableText::set_expanded(self, expanded);
    }
}

impl Component for ExpandableText {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }

    fn invalidate(&self) {
        self.inner.invalidate();
    }
}

#[derive(Clone)]
pub struct BuiltInHeader {
    text: ExpandableText,
    on_logo_click: Arc<Mutex<Option<Arc<dyn Fn(i64, i64) + Send + Sync>>>>,
}

impl BuiltInHeader {
    pub fn new(
        get_collapsed_text: Arc<dyn Fn() -> String + Send + Sync>,
        get_expanded_text: Arc<dyn Fn() -> String + Send + Sync>,
        expanded: bool,
        padding_x: i64,
        padding_y: i64,
    ) -> Self {
        Self {
            text: ExpandableText::new(get_collapsed_text, get_expanded_text, expanded, padding_x, padding_y),
            on_logo_click: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_on_logo_click(&self, cb: Option<Arc<dyn Fn(i64, i64) + Send + Sync>>) {
        *self.on_logo_click.lock().unwrap_or_else(|e| e.into_inner()) = cb;
    }

    pub fn set_expanded(&self, expanded: bool) {
        self.text.set_expanded(expanded);
    }
}

impl Expandable for BuiltInHeader {
    fn set_expanded(&self, expanded: bool) {
        BuiltInHeader::set_expanded(self, expanded);
    }
}

impl Component for BuiltInHeader {
    fn render(&self, width: usize) -> Vec<String> {
        self.text.render(width)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.r#type != TuiMouseEventType::Click || event.y > 1 || event.x < 1 || event.x > 4 {
            return None;
        }
        let cb = self.on_logo_click.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        cb(event.screen_x - event.x + 1, event.screen_y - event.y);
        Some(TuiMouseEventResult {
            handled: Some(true),
            ..Default::default()
        })
    }

    fn invalidate(&self) {
        self.text.invalidate();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionQueuedMessage {
    pub text: String,
    pub mode: CompactionQueueMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionQueueMode {
    Steer,
    FollowUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionCostKind {
    Compaction,
    BranchSummary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompactionCostNotice {
    pub kind: CompactionCostKind,
    pub usage: Usage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissingSessionCwdChoice {
    Continue,
    Fork,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CancelledResult {
    pub cancelled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeForkResult {
    pub cancelled: bool,
    pub selected_text: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeForkOptions {
    pub at_leaf: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SwitchSessionOptions {
    pub cwd_override: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ClearQueueResult {
    pub steering: Vec<String>,
    pub follow_up: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SessionStats {
    pub session_file: Option<String>,
    pub session_id: String,
    pub total_messages: usize,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_calls: usize,
    pub tool_results: usize,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub total_tokens: i64,
    pub cost: f64,
}

#[derive(Clone, Debug)]
pub struct UsageBreakdownEntry {
    pub key: String,
    pub cost: f64,
    pub tokens: i64,
}

#[derive(Clone, Debug)]
pub struct CacheWarmingStatusSummary {
    pub status_text: String,
    pub economics_available: bool,
    pub miss_cost: f64,
    pub warm_cost: f64,
}

pub type CacheMissNotice = crate::core::cache_stats::CacheMiss;

#[derive(Clone, Debug, Default)]
pub struct CompactionSummaryResult {
    pub summary: String,
    pub tokens_before: i64,
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, Default)]
pub struct NavigateTreeOptions {
    pub summarize: bool,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct NavigateTreeResult {
    pub aborted: bool,
    pub cancelled: bool,
    pub editor_text: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct InteractivePromptOptions {
    pub images: Option<Vec<ImageContent>>,
    pub streaming_behavior: Option<CompactionQueueMode>,
}

#[derive(Clone, Debug)]
pub struct CycleModelResult {
    pub model: Model,
    pub thinking_level: ModelThinkingLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CycleDirection {
    Forward,
    Backward,
}

#[derive(Clone, Debug, Default)]
pub struct BashExecutionResult {
    pub output: String,
    pub exit_code: Option<i64>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyKind {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Default)]
pub struct ExtensionUIDialogOptions {
    pub timeout_ms: Option<i64>,
    pub signal: Option<AbortSignal>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WidgetPlacement {
    #[default]
    AboveEditor,
    BelowEditor,
}

#[derive(Clone, Debug, Default)]
pub struct ExtensionWidgetOptions {
    pub placement: WidgetPlacement,
}

#[derive(Clone)]
pub enum WidgetContent {
    Lines(Vec<String>),
    Factory(Arc<dyn Fn(Arc<dyn TUI + Send + Sync>, Theme) -> Arc<dyn Component> + Send + Sync>),
}

pub type EditorFactory = Arc<
    dyn Fn(Arc<dyn TUI + Send + Sync>, pi_tui::EditorTheme, KeybindingsManager) -> Arc<dyn EditorComponent>
        + Send
        + Sync,
>;
pub type AutocompleteProviderFactory = Arc<
    dyn Fn(Arc<dyn AutocompleteProvider + Send + Sync>) -> Arc<dyn AutocompleteProvider + Send + Sync> + Send + Sync,
>;
pub type FooterFactory = Arc<
    dyn Fn(Arc<dyn TUI + Send + Sync>, Theme, Arc<dyn ReadonlyFooterDataProvider>) -> Arc<dyn Component> + Send + Sync,
>;
pub type HeaderFactory = Arc<dyn Fn(Arc<dyn TUI + Send + Sync>, Theme) -> Arc<dyn Component> + Send + Sync>;
pub type DoneFn = Arc<dyn Fn(Value) + Send + Sync>;
pub type CustomUiFactory = Box<
    dyn FnOnce(Arc<dyn TUI + Send + Sync>, Theme, KeybindingsManager, DoneFn) -> BoxFuture<Result<Arc<dyn Component>>>
        + Send,
>;

#[derive(Clone, Default)]
pub struct ExtensionCustomOptions {
    pub overlay: bool,
    pub overlay_options: Option<OverlayOptions>,
    pub on_handle: Option<Arc<dyn Fn(OverlayHandle) + Send + Sync>>,
    pub blocked: Option<BlockedStatus>,
}

pub trait ExtensionUiContext: Send + Sync {
    fn select(
        &self,
        title: String,
        options: Vec<String>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> BoxFuture<Option<String>>;
    fn confirm(&self, title: String, message: String, opts: Option<ExtensionUIDialogOptions>) -> BoxFuture<bool>;
    fn input(
        &self,
        title: String,
        placeholder: Option<String>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> BoxFuture<Option<String>>;
    fn notify(&self, message: &str, kind: Option<NotifyKind>);
    fn on_terminal_input(&self, handler: pi_tui::tui::TuiInputListener) -> Unsubscribe;
    fn set_status(&self, key: &str, text: Option<&str>);
    fn set_working_message(&self, message: Option<&str>);
    fn set_working_visible(&self, visible: bool);
    fn set_working_indicator(&self, options: Option<pi_tui::LoaderIndicatorOptions>);
    fn set_hidden_thinking_label(&self, label: Option<&str>);
    fn set_widget(&self, key: &str, content: Option<WidgetContent>, options: Option<ExtensionWidgetOptions>);
    fn set_footer(&self, factory: Option<FooterFactory>);
    fn set_header(&self, factory: Option<HeaderFactory>);
    fn set_title(&self, title: &str);
    fn custom(&self, factory: CustomUiFactory, options: Option<ExtensionCustomOptions>) -> BoxFuture<Result<Value>>;
    fn paste_to_editor(&self, text: &str);
    fn set_editor_text(&self, text: &str);
    fn get_editor_text(&self) -> String;
    fn editor(&self, title: String, prefill: Option<String>) -> BoxFuture<Option<String>>;
    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory);
    fn set_editor_component(&self, factory: Option<EditorFactory>);
    fn get_editor_component(&self) -> Option<EditorFactory>;
    fn theme(&self) -> Theme;
    fn get_all_themes(&self) -> Vec<ThemeInfo>;
    fn get_theme(&self, name: &str) -> Option<Theme>;
    fn set_theme_name(&self, name: &str) -> crate::modes::interactive::theme::theme_controller::ThemeResult;
    fn set_theme_instance(
        &self,
        theme_instance: Theme,
    ) -> crate::modes::interactive::theme::theme_controller::ThemeResult;
    fn get_tools_expanded(&self) -> bool;
    fn set_tools_expanded(&self, expanded: bool);
}

pub struct InteractiveExtensionBindings {
    pub ui_context: Arc<dyn ExtensionUiContext>,
    pub abort_handler: Arc<dyn Fn() + Send + Sync>,
    pub shutdown_handler: Arc<dyn Fn() + Send + Sync>,
    pub on_error: Arc<dyn Fn(&str, &str, Option<&str>) + Send + Sync>,
    pub wait_for_idle: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>,
    pub new_session: Arc<dyn Fn() -> BoxFuture<Result<CancelledResult>> + Send + Sync>,
    pub fork: Arc<dyn Fn(String, Option<RuntimeForkOptions>) -> BoxFuture<Result<CancelledResult>> + Send + Sync>,
    pub navigate_tree: Arc<dyn Fn(String, NavigateTreeOptions) -> BoxFuture<Result<CancelledResult>> + Send + Sync>,
    pub switch_session: Arc<dyn Fn(String) -> BoxFuture<Result<CancelledResult>> + Send + Sync>,
    pub reload: Arc<dyn Fn() -> BoxFuture<Result<()>> + Send + Sync>,
}

pub trait InteractiveSettingsManager: ThemeSettingsSource + Send + Sync {
    fn get_terminal_capability_overrides(&self) -> pi_tui::terminal_image::TerminalCapabilityOverrides;
    fn get_tui_mode(&self) -> TuiMode;
    fn set_tui_mode(&self, mode: TuiMode);
    fn get_show_hardware_cursor(&self) -> bool;
    fn set_show_hardware_cursor(&self, enabled: bool);
    fn get_clear_on_shrink(&self) -> bool;
    fn set_clear_on_shrink(&self, enabled: bool);
    fn get_fullscreen_copy_on_select(&self) -> bool;
    fn set_fullscreen_copy_on_select(&self, enabled: bool);
    fn get_fullscreen_wheel_scroll_lines(&self) -> WheelScrollLines;
    fn set_fullscreen_wheel_scroll_lines(&self, lines: WheelScrollLines);
    fn get_fullscreen_exit_output(&self) -> FullscreenExitOutput;
    fn set_fullscreen_exit_output(&self, output: FullscreenExitOutput);
    fn get_fullscreen_scrollbar(&self) -> ScrollViewScrollbar;
    fn set_fullscreen_scrollbar(&self, mode: ScrollViewScrollbar);
    fn get_editor_padding_x(&self) -> i64;
    fn set_editor_padding_x(&self, padding: i64);
    fn get_autocomplete_max_visible(&self) -> i64;
    fn set_autocomplete_max_visible(&self, max_visible: i64);
    fn get_hide_thinking_block(&self) -> bool;
    fn set_hide_thinking_block(&self, hidden: bool);
    fn get_output_pad(&self) -> i64;
    fn set_output_pad(&self, pad: i64);
    fn get_enable_skill_commands(&self) -> bool;
    fn set_enable_skill_commands(&self, enabled: bool);
    fn get_collapse_changelog(&self) -> bool;
    fn set_collapse_changelog(&self, collapsed: bool);
    fn get_code_block_indent(&self) -> usize;
    fn get_quiet_startup(&self) -> QuietStartup;
    fn set_quiet_startup(&self, quiet: QuietStartup);
    fn get_mermaid_rendering_mode(&self) -> MermaidRenderingMode;
    fn set_mermaid_rendering_mode(&self, mode: MermaidRenderingMode);
    fn get_show_cache_miss_notices(&self) -> bool;
    fn set_show_cache_miss_notices(&self, shown: bool);
    fn get_show_terminal_progress(&self) -> bool;
    fn set_show_terminal_progress(&self, enabled: bool);
    fn get_show_images(&self) -> bool;
    fn set_show_images(&self, enabled: bool);
    fn get_image_width_cells(&self) -> Option<i64>;
    fn set_image_width_cells(&self, width: Option<i64>);
    fn get_image_auto_resize(&self) -> bool;
    fn set_image_auto_resize(&self, enabled: bool);
    fn get_block_images(&self) -> bool;
    fn set_block_images(&self, blocked: bool);
    fn get_theme(&self) -> Option<String>;
    fn set_theme(&self, theme: &str);
    fn get_last_changelog_version(&self) -> Option<String>;
    fn set_last_changelog_version(&self, version: &str);
    fn get_extension_changelog_version(&self, key: &str) -> Option<String>;
    fn set_extension_changelog_version(&self, key: &str, version: &str);
    fn get_http_idle_timeout_ms(&self) -> u64;
    fn set_http_idle_timeout_ms(&self, timeout_ms: u64);
    fn get_cache_warming_mode(&self) -> CacheWarmingMode;
    fn set_cache_warming_mode(&self, mode: CacheWarmingMode);
    fn get_transport(&self) -> pi_ai::Transport;
    fn set_transport(&self, transport: pi_ai::Transport);
    fn get_session_share_destination(&self) -> SessionShareDestination;
    fn set_session_share_destination(&self, destination: SessionShareDestination);
    fn get_default_provider(&self) -> Option<String>;
    fn get_default_model(&self) -> Option<String>;
    fn get_default_thinking_level(&self) -> Option<ModelThinkingLevel>;
    fn get_all_model_thinking_levels(&self) -> IndexMap<String, ModelThinkingLevel>;
    fn set_model_thinking_level(&self, provider: &str, model_id: &str, level: ModelThinkingLevel);
    fn remove_model_thinking_level(&self, provider: &str, model_id: &str);
    fn get_enabled_models(&self) -> Option<Vec<String>>;
    fn set_enabled_models(&self, patterns: Option<Vec<String>>);
    fn get_warnings(&self) -> WarningSettings;
    fn set_warnings(&self, warnings: WarningSettings);
    fn get_enable_install_telemetry(&self) -> bool;
    fn set_enable_install_telemetry(&self, enabled: bool);
    fn get_default_project_trust(&self) -> DefaultProjectTrust;
    fn set_default_project_trust(&self, trust: DefaultProjectTrust);
    fn get_double_escape_action(&self) -> DoubleEscapeAction;
    fn set_double_escape_action(&self, action: DoubleEscapeAction);
    fn get_tree_filter_mode(&self) -> TreeFilterMode;
    fn set_tree_filter_mode(&self, mode: TreeFilterMode);
    fn get_branch_summary_skip_prompt(&self) -> bool;
    fn get_external_editor_command(&self) -> Option<String>;
    fn get_or_create_device_id(&self) -> String;
    fn is_project_trusted(&self) -> bool;
    fn as_core_settings_manager(&self) -> Option<&crate::core::settings_manager::SettingsManager> {
        None
    }
}

pub trait InteractiveSessionManager: Send + Sync {
    fn get_cwd(&self) -> String;
    fn get_session_dir(&self) -> String;
    fn get_session_id(&self) -> String;
    fn get_session_file(&self) -> Option<String>;
    fn get_session_name(&self) -> Option<String>;
    fn is_persisted(&self) -> bool;
    fn uses_default_session_dir(&self) -> bool;
    fn get_leaf_id(&self) -> Option<String>;
    fn get_tree(&self) -> Vec<SessionTreeNode>;
    fn get_entries(&self) -> Vec<SessionEntry>;
    fn get_branch(&self) -> Vec<SessionEntry>;
    fn build_context_entries(&self) -> Vec<SessionEntry>;
    fn append_label_change(&self, entry_id: &str, label: Option<&str>);
    fn list_current_sessions(
        &self,
        on_progress: Option<SessionListProgress>,
        signal: AbortSignal,
    ) -> BoxFuture<Result<Vec<SessionInfo>>>;
    fn list_all_sessions(
        &self,
        on_progress: Option<SessionListProgress>,
        signal: AbortSignal,
    ) -> BoxFuture<Result<Vec<SessionInfo>>>;
    fn rename_session(&self, session_file: &str, next_name: Option<&str>) -> BoxFuture<Result<()>>;
}

#[derive(Clone, Debug)]
pub struct ProviderAuthInfo {
    pub id: String,
    pub name: String,
    pub has_oauth: bool,
    pub oauth_method_name: Option<String>,
    pub oauth_login_label: Option<String>,
    pub oauth_is_subscription: bool,
    pub has_api_key: bool,
    pub api_key_method_name: Option<String>,
    pub api_key_has_login: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ProviderAuthStatusInfo {
    pub configured: bool,
    pub label: Option<String>,
    pub source: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StoredCredentialInfo {
    pub provider_id: String,
    pub auth_type: AuthType,
}

#[async_trait]
pub trait InteractiveModelRuntime: ModelCatalogRuntime + Send + Sync {
    fn get_providers(&self) -> Vec<ProviderAuthInfo>;
    fn get_provider_auth_status(&self, provider_id: &str) -> ProviderAuthStatusInfo;
    fn is_using_oauth(&self, provider_id: &str) -> bool;
    async fn check_auth_type(&self, provider_id: &str) -> Option<AuthType>;
    async fn get_api_key(&self, provider_id: &str) -> Option<String>;
    async fn list_credentials(&self, signal: AbortSignal) -> Result<Vec<StoredCredentialInfo>>;
    async fn logout(&self, provider_id: &str, signal: AbortSignal) -> Result<()>;
    async fn refresh_providers(
        &self,
        providers: Vec<String>,
        signal: AbortSignal,
    ) -> Result<crate::modes::interactive::model_catalog_refresh::ModelCatalogRefreshResult>;
    async fn login(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        interaction: Arc<dyn pi_ai::auth::types::AuthInteraction>,
        signal: AbortSignal,
        device_id: String,
    ) -> Result<()>;
}

pub trait InteractiveResourceLoader: Send + Sync {
    fn get_skills(&self) -> (Vec<SkillInfo>, Vec<ResourceDiagnostic>);
    fn get_prompts(&self) -> (Vec<PromptTemplateInfo>, Vec<ResourceDiagnostic>);
    fn get_themes(&self) -> (Vec<Theme>, Vec<LoadedThemeResource>, Vec<ResourceDiagnostic>);
    fn get_extensions(&self) -> LoadedExtensionsResult;
    fn get_system_prompt_source(&self) -> Option<SourceInfo>;
    fn get_append_system_prompt_sources(&self) -> Vec<SourceInfo>;
    fn get_agents_files(&self) -> Vec<SourceInfo>;
}

pub trait InteractiveExtensionRunner: Send + Sync {
    fn get_registered_commands(&self) -> Vec<RegisteredExtensionCommand>;
    fn has_command(&self, name: &str) -> bool;
    fn get_command_diagnostics(&self) -> Vec<ResourceDiagnostic>;
    fn get_shortcut_diagnostics(&self) -> Vec<ResourceDiagnostic>;
    fn get_shortcuts(&self) -> IndexMap<String, RegisteredExtensionShortcut>;
    fn get_markdown_transformers(&self) -> Vec<MarkdownTransformer>;
    fn get_entry_renderer(&self, custom_type: &str) -> Option<EntryRenderer>;
    fn get_message_renderer(&self, custom_type: &str) -> Option<MessageRenderer>;
    fn has_user_bash_handlers(&self) -> bool;
    fn emit_user_bash(
        &self,
        command: &str,
        exclude_from_context: bool,
        cwd: &str,
    ) -> BoxFuture<Result<Option<BashExecutionResult>>>;
}

pub trait InteractiveFooterDataProvider: ReadonlyFooterDataProvider + Send + Sync {
    fn set_cwd(&self, cwd: &str);
    fn set_extension_status(&self, key: &str, text: Option<&str>);
    fn clear_extension_statuses(&self);
    fn set_available_provider_count(&self, count: usize);
    fn dispose(&self);
}

#[async_trait]
pub trait InteractiveSession: FooterSession + Send + Sync {
    fn session_manager(&self) -> Arc<dyn InteractiveSessionManager>;
    fn settings_manager(&self) -> Arc<dyn InteractiveSettingsManager>;
    fn model_runtime(&self) -> Arc<dyn InteractiveModelRuntime>;
    fn resource_loader(&self) -> Arc<dyn InteractiveResourceLoader>;
    fn extension_runner(&self) -> Arc<dyn InteractiveExtensionRunner>;
    fn as_footer_session(&self) -> Arc<dyn FooterSession>;
    fn as_share_session(&self) -> Option<Arc<dyn ShareSession>>;
    fn as_bug_report_session(&self) -> Option<Arc<dyn BugReportSession>>;

    fn model_thinking_level(&self) -> ModelThinkingLevel;
    fn scoped_models(&self) -> Vec<ScopedModelItem>;
    fn set_scoped_models(&self, models: Vec<ScopedModelItem>);
    fn auto_compaction_enabled(&self) -> bool;
    fn set_auto_compaction_enabled(&self, enabled: bool);
    fn steering_mode(&self) -> SteeringMode;
    fn set_steering_mode(&self, mode: SteeringMode);
    fn follow_up_mode(&self) -> SteeringMode;
    fn set_follow_up_mode(&self, mode: SteeringMode);
    fn set_transport(&self, transport: pi_ai::Transport);
    fn set_cache_warming_mode(&self, mode: CacheWarmingMode);
    fn cache_warming_status(&self) -> Option<CacheWarmingStatusSummary>;
    fn is_streaming(&self) -> bool;
    fn is_idle(&self) -> bool;
    fn is_compacting(&self) -> bool;
    fn is_bash_running(&self) -> bool;
    fn retry_attempt(&self) -> u32;
    fn pending_message_count(&self) -> usize;
    fn get_steering_messages(&self) -> Vec<String>;
    fn get_follow_up_messages(&self) -> Vec<String>;
    fn clear_queue(&self) -> ClearQueueResult;
    fn messages(&self) -> Vec<AgentMessage>;
    fn prompt_templates(&self) -> Vec<PromptTemplateInfo>;
    fn session_file(&self) -> Option<String>;
    fn signal(&self) -> Option<AbortSignal>;
    fn get_available_thinking_levels(&self) -> Vec<ModelThinkingLevel>;
    fn set_thinking_level(&self, level: ModelThinkingLevel, persist: bool) -> Result<()>;
    fn cycle_thinking_level(&self) -> Option<ModelThinkingLevel>;
    async fn set_model(&self, model: Model, persist: bool) -> Result<()>;
    async fn cycle_model(&self, direction: CycleDirection) -> Result<Option<CycleModelResult>>;
    async fn prompt(&self, text: &str, options: Option<InteractivePromptOptions>) -> Result<()>;
    async fn steer(&self, text: &str) -> Result<()>;
    async fn follow_up(&self, text: &str) -> Result<()>;
    async fn abort(&self);
    async fn wait_for_idle(&self);
    fn abort_compaction(&self);
    fn abort_branch_summary(&self);
    fn abort_retry(&self);
    fn abort_bash(&self);
    fn subscribe(&self, listener: Arc<dyn Fn(AgentSessionEvent) -> BoxFuture<()> + Send + Sync>) -> Unsubscribe;
    async fn bind_extensions(&self, bindings: InteractiveExtensionBindings) -> Result<()>;
    async fn reload(&self, before_session_start: Option<Arc<dyn Fn() + Send + Sync>>) -> Result<()>;
    async fn compact(&self, custom_instructions: Option<&str>) -> Result<CompactionSummaryResult>;
    async fn navigate_tree(&self, target_id: &str, options: NavigateTreeOptions) -> Result<NavigateTreeResult>;
    fn get_user_messages_for_forking(&self) -> Vec<UserMessageItem>;
    fn get_session_stats(&self) -> SessionStats;
    fn get_last_assistant_text(&self) -> Option<String>;
    fn set_session_name(&self, name: &str);
    fn export_to_jsonl(&self, output_path: Option<&str>) -> Result<String>;
    async fn export_to_html(&self, output_path: Option<&str>, theme_name: Option<&str>) -> Result<String>;
    async fn execute_bash(
        &self,
        command: &str,
        on_chunk: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
        exclude_from_context: bool,
    ) -> Result<BashExecutionResult>;
    fn record_bash_result(&self, command: &str, result: &BashExecutionResult, exclude_from_context: bool);
    fn detect_cache_miss(&self, message: &pi_ai::AssistantMessage) -> Option<CacheMissNotice>;
    fn compute_cache_waste(&self) -> (i64, usize, f64);
    fn get_usage_cost_breakdown(&self) -> Vec<UsageBreakdownEntry>;
    fn get_tool_renderers(&self, tool_name: &str) -> Option<ToolRenderers>;
}

#[async_trait]
pub trait InteractiveRuntimeHost: Send + Sync {
    fn session(&self) -> Arc<dyn InteractiveSession>;
    fn agent_dir(&self) -> String;
    fn diagnostics(&self) -> Vec<AgentSessionRuntimeDiagnostic>;
    fn model_fallback_message(&self) -> Option<String>;
    fn set_before_session_invalidate(&self, cb: Arc<dyn Fn() + Send + Sync>);
    fn set_rebind_session(&self, cb: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>);
    async fn new_session(&self) -> Result<CancelledResult>;
    async fn fork(&self, entry_id: &str, options: Option<RuntimeForkOptions>) -> Result<RuntimeForkResult>;
    async fn switch_session(
        &self,
        session_path: &str,
        options: Option<SwitchSessionOptions>,
    ) -> Result<CancelledResult>;
    async fn import_from_jsonl(&self, input_path: &str, cwd_override: Option<&str>) -> Result<CancelledResult>;
    async fn dispose(&self);
}

pub(crate) fn sync_tui_keybindings(kb: &KeybindingsManager) {
    pi_tui::set_keybindings(pi_tui::KeybindingsManager::new(
        KEYBINDINGS.clone(),
        Some(kb.get_user_bindings()),
    ));
}

/// Optional hooks allowing offline unit tests to override exit/signal behavior cleanly.
#[derive(Clone)]
pub struct InteractiveModeHooks {
    pub exit_process: Arc<dyn Fn(i32) + Send + Sync>,
    pub create_footer_data_provider: Arc<dyn Fn(&str) -> Arc<dyn InteractiveFooterDataProvider> + Send + Sync>,
    pub keybindings: Option<Arc<KeybindingsManager>>,
    pub keybindings_reload: Arc<dyn Fn(&KeybindingsManager) + Send + Sync>,
    pub clipboard_copy: Arc<dyn Fn(&str) -> BoxFuture<Result<()>> + Send + Sync>,
    pub clipboard_read_text: Arc<dyn Fn() -> BoxFuture<Result<Option<String>>> + Send + Sync>,
    pub clipboard_read_file_paths: Arc<dyn Fn() -> BoxFuture<Result<Option<Vec<String>>>> + Send + Sync>,
    pub clipboard_read_image:
        Arc<dyn Fn() -> BoxFuture<Result<Option<crate::utils::clipboard_image::ClipboardImage>>> + Send + Sync>,
    pub platform_is_windows: Option<bool>,
    pub suspend_process:
        Option<Arc<dyn Fn(Arc<dyn Fn() + Send + Sync>) -> std::result::Result<(), String> + Send + Sync>>,
    pub install_change_warning: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl Default for InteractiveModeHooks {
    fn default() -> Self {
        Self {
            exit_process: Arc::new(|code| std::process::exit(code)),
            create_footer_data_provider: Arc::new(|cwd| {
                Arc::new(crate::core::footer_data_provider::FooterDataProvider::new(cwd))
            }),
            keybindings: None,
            keybindings_reload: Arc::new(|kb| {
                kb.reload();
                sync_tui_keybindings(kb);
            }),
            clipboard_copy: Arc::new(|text| {
                let text = text.to_string();
                Box::pin(async move { crate::utils::clipboard::copy_to_clipboard(&text).await })
            }),
            clipboard_read_text: Arc::new(|| {
                Box::pin(async { Ok(crate::utils::clipboard::read_clipboard_text().await) })
            }),
            clipboard_read_file_paths: Arc::new(|| {
                Box::pin(async { Ok(crate::utils::clipboard::read_clipboard_file_paths().await) })
            }),
            clipboard_read_image: Arc::new(|| {
                Box::pin(async { Ok(crate::utils::clipboard_image::read_clipboard_image(None).await) })
            }),
            platform_is_windows: None,
            suspend_process: None,
            install_change_warning: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct InteractiveModeOptions {
    pub migrated_providers: Option<Vec<String>>,
    pub startup_diagnostics: Option<Vec<AgentSessionRuntimeDiagnostic>>,
    pub model_fallback_message: Option<String>,
    pub auto_trust_on_reload_cwd: Option<String>,
    pub initial_message: Option<String>,
    pub initial_images: Option<Vec<ImageContent>>,
    pub initial_messages: Option<Vec<String>>,
    pub verbose: Option<bool>,
    pub tui_mode: Option<TuiMode>,
    pub initial_theme_setting: Option<String>,
    pub terminal: Option<Arc<dyn Terminal>>,
    pub hooks: Option<InteractiveModeHooks>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionChangelogUpdate {
    pub name: String,
    pub version: String,
    pub markdown: String,
}

#[derive(Clone)]
pub enum ActiveEditor {
    Default(CustomEditor),
    Custom {
        component: Arc<dyn EditorComponent>,
        set_output_pad_unused: (),
    },
}

struct EditorComponentAdapter(Arc<dyn EditorComponent>);

impl Component for EditorComponentAdapter {
    fn render(&self, width: usize) -> Vec<String> {
        self.0.render(width)
    }

    fn handle_input(&self, data: &str) {
        self.0.handle_input(data);
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        self.0.wants_key_release()
    }

    fn invalidate(&self) {
        self.0.invalidate();
    }

    fn has_handle_input(&self) -> bool {
        self.0.has_handle_input()
    }

    fn is_container_handle_mouse(&self) -> bool {
        self.0.is_container_handle_mouse()
    }

    fn as_focusable(&self) -> Option<&dyn pi_tui::Focusable> {
        self.0.as_focusable()
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        self.0.container_children()
    }

    fn as_layout_component(&self) -> Option<&dyn pi_tui::layout_node::LayoutComponent> {
        self.0.as_layout_component()
    }
}

impl ActiveEditor {
    pub fn as_component(&self) -> Arc<dyn Component> {
        match self {
            Self::Default(ed) => Arc::new(ed.clone()),
            Self::Custom { component, .. } => Arc::new(EditorComponentAdapter(Arc::clone(component))),
        }
    }

    pub fn is_default(&self, default_ed: &CustomEditor) -> bool {
        let _ = default_ed;
        matches!(self, Self::Default(_))
    }

    pub fn get_text(&self) -> String {
        match self {
            Self::Default(ed) => ed.get_text(),
            Self::Custom { component, .. } => component.get_text(),
        }
    }

    pub fn get_expanded_text(&self) -> String {
        match self {
            Self::Default(ed) => ed.get_expanded_text(),
            Self::Custom { component, .. } => component.get_expanded_text(),
        }
    }

    pub fn set_text(&self, text: &str) {
        match self {
            Self::Default(ed) => ed.set_text(text),
            Self::Custom { component, .. } => component.set_text(text),
        }
    }

    pub fn insert_text_at_cursor(&self, text: &str) {
        match self {
            Self::Default(ed) => ed.insert_text_at_cursor(text),
            Self::Custom { component, .. } => component.insert_text_at_cursor(text),
        }
    }

    pub fn add_to_history(&self, text: &str) {
        match self {
            Self::Default(ed) => ed.add_to_history(text),
            Self::Custom { component, .. } => component.add_to_history(text),
        }
    }

    pub fn border_color(&self) -> Option<EditorBorderColor> {
        match self {
            Self::Default(ed) => Some(ed.border_color()),
            Self::Custom { component, .. } => component.border_color(),
        }
    }

    pub fn set_border_color(&self, color: EditorBorderColor) {
        match self {
            Self::Default(ed) => ed.set_border_color(color),
            Self::Custom { component, .. } => component.set_border_color(Some(color)),
        }
    }

    pub fn set_padding_x(&self, padding: f64) {
        match self {
            Self::Default(ed) => ed.set_padding_x(padding),
            Self::Custom { component, .. } => component.set_padding_x(padding),
        }
    }

    pub fn set_autocomplete_max_visible(&self, max_visible: f64) {
        match self {
            Self::Default(ed) => ed.set_autocomplete_max_visible(max_visible),
            Self::Custom { component, .. } => component.set_autocomplete_max_visible(max_visible),
        }
    }

    pub fn set_autocomplete_provider(&self, provider: Arc<dyn AutocompleteProvider + Send + Sync>) {
        match self {
            Self::Default(ed) => ed.set_autocomplete_provider(provider),
            Self::Custom { component, .. } => component.set_autocomplete_provider(provider),
        }
    }

    pub fn on_submit(&self) -> Option<EditorTextCallback> {
        match self {
            Self::Default(ed) => ed.on_submit(),
            Self::Custom { component, .. } => component.on_submit(),
        }
    }

    pub fn set_on_submit(&self, cb: Option<EditorTextCallback>) {
        match self {
            Self::Default(ed) => ed.set_on_submit(cb),
            Self::Custom { component, .. } => component.set_on_submit(cb),
        }
    }

    pub fn set_on_change(&self, cb: Option<EditorTextCallback>) {
        match self {
            Self::Default(ed) => ed.set_on_change(cb),
            Self::Custom { component, .. } => component.set_on_change(cb),
        }
    }

    pub fn get_cursor(&self) -> Option<EditorCursor> {
        match self {
            Self::Default(ed) => Some(ed.get_cursor()),
            Self::Custom { .. } => None,
        }
    }

    pub fn handle_input(&self, data: &str) {
        match self {
            Self::Default(ed) => ed.handle_input(data),
            Self::Custom { component, .. } => component.handle_input(data),
        }
    }

    pub fn embed_working_status(&self) -> bool {
        match self {
            Self::Default(ed) => ed.embed_working_status(),
            Self::Custom { .. } => false,
        }
    }

    pub fn set_working_status_indicator(&self, indicator: Option<StatusIndicator>) {
        if let Self::Default(ed) = self {
            ed.set_working_status_indicator(indicator);
        }
    }
}

pub const ANTHROPIC_SUBSCRIPTION_AUTH_WARNING: &str = "Anthropic subscription auth is active. Third-party harness usage draws from extra usage and is billed per token, not your Claude plan limits. Manage extra usage at https://claude.ai/settings/usage. Disable this warning in /settings.";

pub const RADIUS_LOGIN_INTRO: &str = "Radius is a service crafted for Pi by the builders of Pi, Earendil Works";

pub fn format_crash_extension_hint(extension_matches: Option<&[String]>) -> Option<String> {
    let matches: Vec<&str> = extension_matches
        .unwrap_or(&[])
        .iter()
        .map(String::as_str)
        .filter(|m| !m.is_empty())
        .collect();
    if matches.is_empty() {
        return None;
    }
    let quoted: Vec<String> = matches.iter().map(|m| format!("`{m}`")).collect();
    let labels = match quoted.len() {
        1 => quoted[0].clone(),
        2 => format!("{} and {}", quoted[0], quoted[1]),
        n => format!("{}, and {}", quoted[..n - 1].join(", "), quoted[n - 1]),
    };
    let noun = if matches.len() == 1 { "extension" } else { "extensions" };
    let pronoun = if matches.len() == 1 { "it" } else { "them" };
    Some(format!(
        "A stack frame came from loaded {noun} {labels}, which may be involved. Try disabling {pronoun} with `{0} config`, or run `{0} -ne` to confirm.",
        *APP_NAME
    ))
}

pub fn is_anthropic_subscription_auth_key(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|k| k.starts_with("sk-ant-oat"))
}

pub fn is_unknown_model(model: Option<&Model>) -> bool {
    model.is_some_and(|m| m.provider == "unknown" && m.id == "unknown" && m.api == "unknown")
}

pub fn quote_if_needed(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b'~' | b':' | b'@'))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn format_resume_command(session_manager: &dyn InteractiveSessionManager) -> Option<String> {
    format_resume_command_with_tty(session_manager, std::io::IsTerminal::is_terminal(&std::io::stdout()))
}

pub fn format_resume_command_with_tty(session_manager: &dyn InteractiveSessionManager, is_tty: bool) -> Option<String> {
    if !is_tty {
        return None;
    }
    if !session_manager.is_persisted() {
        return None;
    }
    let session_file = session_manager.get_session_file()?;
    if !pi_js::fs::exists(&session_file) {
        return None;
    }
    let mut args = vec![(*APP_NAME).to_string()];
    if !session_manager.uses_default_session_dir() {
        args.push("--session-dir".to_string());
        args.push(quote_if_needed(&session_manager.get_session_dir()));
    }
    args.push("--session".to_string());
    args.push(session_manager.get_session_id());
    Some(args.join(" "))
}

pub fn default_model_for_provider(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "anthropic" => Some("claude-sonnet-4-5"),
        "openai" => Some("gpt-5.1-codex"),
        "google" => Some("gemini-2.5-pro"),
        "openrouter" => Some("openai/gpt-5.1-codex"),
        "radius" => Some("balanced"),
        _ => None,
    }
}

pub fn llama_cpp_post_login_guidance(action_label: &str, loaded_model_count: usize) -> String {
    if loaded_model_count == 0 {
        format!("{action_label}. No llama.cpp models are loaded. Use /llama to load a model, then /model to select it.")
    } else {
        format!("{action_label}. Use /model to select a loaded llama.cpp model, or /llama to manage models.")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginProviderCompletionOption {
    pub id: String,
    pub name: String,
    pub auth_types: Vec<AuthType>,
    pub subscription: Option<bool>,
}

fn auth_type_order(auth_type: AuthType) -> u8 {
    match auth_type {
        AuthType::OAuth => 0,
        AuthType::ApiKey => 1,
    }
}

pub fn create_fuzzy_autocomplete_items<T, FSearch, FItem>(
    items: &[T],
    prefix: &str,
    get_search_text: FSearch,
    to_item: FItem,
) -> Option<Vec<AutocompleteItem>>
where
    T: Clone,
    FSearch: Fn(&T) -> String + Send + Sync + 'static,
    FItem: Fn(&T) -> AutocompleteItem,
{
    let filtered = fuzzy_filter(items, prefix, Arc::new(get_search_text));
    if filtered.is_empty() {
        None
    } else {
        Some(filtered.into_iter().map(|item| to_item(&item)).collect())
    }
}

pub fn get_login_provider_completion_options(
    provider_options: &[AuthSelectorProvider],
) -> Vec<LoginProviderCompletionOption> {
    let mut by_id: IndexMap<String, LoginProviderCompletionOption> = IndexMap::new();
    for provider in provider_options {
        if let Some(existing) = by_id.get_mut(&provider.id) {
            if !existing.auth_types.contains(&provider.auth_type) {
                existing.auth_types.push(provider.auth_type);
                existing.auth_types.sort_by_key(|a| auth_type_order(*a));
            }
        } else {
            by_id.insert(
                provider.id.clone(),
                LoginProviderCompletionOption {
                    id: provider.id.clone(),
                    name: provider.name.clone(),
                    auth_types: vec![provider.auth_type],
                    subscription: provider.subscription,
                },
            );
        }
    }
    by_id.into_values().collect()
}

pub fn get_login_provider_search_text(provider: &LoginProviderCompletionOption) -> String {
    let auth_types: Vec<String> = provider
        .auth_types
        .iter()
        .map(|auth_type| {
            let wire_name = match auth_type {
                AuthType::OAuth => "oauth",
                AuthType::ApiKey => "api_key",
            };
            format!(
                "{wire_name} {}",
                format_auth_selector_provider_type(*auth_type, provider.subscription)
            )
        })
        .collect();
    format!("{} {} {}", provider.id, provider.name, auth_types.join(" "))
}

pub fn format_login_provider_completion_description(provider: &LoginProviderCompletionOption) -> String {
    let auth_types: Vec<&str> = provider
        .auth_types
        .iter()
        .map(|auth_type| format_auth_selector_provider_type(*auth_type, provider.subscription))
        .collect();
    let joined = auth_types.join("/");
    if provider.name == provider.id {
        joined
    } else {
        format!("{} · {joined}", provider.name)
    }
}

#[derive(Clone)]
pub(crate) struct ChildTrackers {
    pub(crate) component: Arc<dyn Component>,
    pub(crate) expandable: Option<Arc<dyn Expandable>>,
    pub(crate) assistant: Option<AssistantMessageComponent>,
    pub(crate) tool_exec: Option<ToolExecutionComponent>,
    pub(crate) set_output_pad: Option<Arc<dyn Fn(i64) + Send + Sync>>,
}

#[derive(Clone)]
pub(crate) struct ExtensionDialogEntry {
    pub(crate) component: Arc<dyn Component>,
    pub(crate) dispose: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) blocked: Option<BlockedStatus>,
}

pub(crate) struct ExtensionInputSubscriptionEntry {
    pub(crate) id: usize,
    pub(crate) handler: pi_tui::tui::TuiInputListener,
    pub(crate) unsubscribe: Unsubscribe,
}

#[derive(Clone)]
pub(crate) struct ExtensionWidgetEntry {
    pub(crate) component: Arc<dyn Component>,
    pub(crate) dispose: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Clone)]
pub(crate) struct CacheWarmingNoticeEntry {
    pub(crate) component: ThemedText,
    pub(crate) total_cost: Arc<Mutex<f64>>,
    pub(crate) note: Option<String>,
}

pub(crate) struct InteractiveModeState {
    pub(crate) renderer: InteractiveTui,
    pub(crate) main_screen_render_state: Option<pi_tui::tui_main_screen::TuiMainScreenRenderState>,
    pub(crate) transcript_scroll_view: Option<pi_tui::ScrollView>,
    pub(crate) fullscreen_layout_root: Option<Arc<dyn Component>>,
    pub(crate) editor: ActiveEditor,
    pub(crate) editor_component_factory: Option<EditorFactory>,
    pub(crate) autocomplete_provider: Option<Arc<dyn AutocompleteProvider + Send + Sync>>,
    pub(crate) autocomplete_provider_wrappers: Vec<AutocompleteProviderFactory>,
    pub(crate) fd_path: Option<String>,
    pub(crate) active_selector_token: u64,
    pub(crate) next_selector_token: u64,
    pub(crate) active_selector_dispose: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) is_initialized: bool,
    pub(crate) is_shutting_down: bool,
    pub(crate) on_input_callback: Option<tokio::sync::oneshot::Sender<String>>,
    pub(crate) pending_user_inputs: std::collections::VecDeque<String>,
    pub(crate) signal_handles: Vec<tokio::task::JoinHandle<()>>,
    pub(crate) active_status_indicator: Option<StatusIndicator>,
    pub(crate) active_working_indicator_embedded: bool,
    pub(crate) working_message: Option<String>,
    pub(crate) working_visible: bool,
    pub(crate) working_indicator_options: Option<pi_tui::LoaderIndicatorOptions>,
    pub(crate) hidden_thinking_label: String,
    pub(crate) last_sigint_time_ms: f64,
    pub(crate) last_escape_time_ms: f64,
    pub(crate) changelog_markdown: Option<String>,
    pub(crate) extension_changelog_updates: Vec<ExtensionChangelogUpdate>,
    pub(crate) startup_notices_shown: bool,
    pub(crate) cache_warming_notice: Option<CacheWarmingNoticeEntry>,
    pub(crate) anthropic_subscription_warning_shown: bool,
    pub(crate) last_status_spacer: Option<Arc<dyn Component>>,
    pub(crate) last_status_text: Option<ThemedText>,
    pub(crate) last_status_text_component: Option<Arc<dyn Component>>,
    pub(crate) last_status_message: Arc<Mutex<String>>,
    pub(crate) managed_tool_status_started: bool,
    pub(crate) streaming_component: Option<AssistantMessageComponent>,
    pub(crate) streaming_component_arc: Option<Arc<dyn Component>>,
    pub(crate) entries_rendered_by_boundary_compaction: IndexSet<String>,
    pub(crate) streaming_message: Option<pi_ai::AssistantMessage>,
    pub(crate) tool_output_expanded: bool,
    pub(crate) hide_thinking_block: bool,
    pub(crate) output_pad: i64,
    pub(crate) skill_commands: IndexMap<String, String>,
    pub(crate) unsubscribe: Option<Unsubscribe>,
    pub(crate) is_bash_mode: bool,
    pub(crate) bash_component: Option<BashExecutionComponent>,
    pub(crate) pending_bash_components: Vec<(BashExecutionComponent, Arc<dyn Component>)>,
    pub(crate) auto_compaction_escape_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) retry_escape_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) compaction_queued_messages: Vec<CompactionQueuedMessage>,
    pub(crate) shutdown_requested: bool,
    pub(crate) bug_report_hint_shown: bool,
    pub(crate) install_change_warning_shown: bool,
    pub(crate) extension_dialogs: Vec<ExtensionDialogEntry>,
    pub(crate) next_blocking_overlay_id: usize,
    pub(crate) extension_overlays: Vec<(usize, Arc<dyn Fn() + Send + Sync>)>,
    pub(crate) next_overlay_entry_id: usize,
    pub(crate) extension_terminal_input_subscriptions: Vec<ExtensionInputSubscriptionEntry>,
    pub(crate) next_input_sub_id: usize,
    pub(crate) extension_widgets_above: IndexMap<String, ExtensionWidgetEntry>,
    pub(crate) extension_widgets_below: IndexMap<String, ExtensionWidgetEntry>,
    pub(crate) custom_footer: Option<(Arc<dyn Component>, Option<Arc<dyn Fn() + Send + Sync>>)>,
    pub(crate) built_in_header: Option<(Arc<dyn Component>, Option<Arc<dyn Expandable>>)>,
    pub(crate) custom_header: Option<(Arc<dyn Component>, Option<Arc<dyn Fn() + Send + Sync>>)>,
    pub(crate) options: InteractiveModeOptions,
    pub(crate) auto_trust_on_reload_cwd: Option<String>,
    pub(crate) default_editor_on_escape: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
    pub(crate) default_editor_on_ctrl_d: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
    pub(crate) default_editor_on_paste_image: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
    pub(crate) default_editor_on_extension_shortcut: Arc<Mutex<Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>>>,
    pub(crate) default_editor_actions: IndexMap<String, Arc<dyn Fn() + Send + Sync>>,
    pub(crate) resource_trackers: Vec<ChildTrackers>,
    pub(crate) chat_trackers: Vec<ChildTrackers>,
    pub(crate) pending_trackers: Vec<ChildTrackers>,
    pub(crate) pending_tool_components: IndexMap<String, ToolExecutionComponent>,
}

pub(crate) struct InteractiveModeInner {
    pub(crate) runtime_host: Arc<dyn InteractiveRuntimeHost>,
    pub(crate) ui: Arc<InteractiveTuiReference>,
    pub(crate) hooks: InteractiveModeHooks,
    pub(crate) header_container: Container,
    pub(crate) loaded_resources_container: Container,
    pub(crate) chat_container: Container,
    pub(crate) document_container: Container,
    pub(crate) pending_messages_container: Container,
    pub(crate) status_container: Container,
    pub(crate) widget_container_above: Container,
    pub(crate) widget_container_below: Container,
    pub(crate) editor_container: Container,
    pub(crate) footer_container: Container,
    pub(crate) default_editor: CustomEditor,
    pub(crate) footer: FooterComponent,
    pub(crate) footer_data_provider: Arc<dyn InteractiveFooterDataProvider>,
    pub(crate) keybindings: Arc<KeybindingsManager>,
    pub(crate) version: String,
    pub(crate) idle_status: crate::modes::interactive::components::status_indicator::IdleStatus,
    pub(crate) program_status: ProgramStatusReporter,
    pub(crate) theme_controller: InteractiveThemeController,
    pub(crate) mermaid_markdown_transformer: MarkdownTransformer,
    pub(crate) shutdown_notify: Arc<tokio::sync::Notify>,
    pub(crate) state: Mutex<InteractiveModeState>,
}

#[derive(Clone)]
pub struct InteractiveMode {
    pub(crate) inner: Arc<InteractiveModeInner>,
}

impl InteractiveMode {
    pub(crate) fn session(&self) -> Arc<dyn InteractiveSession> {
        self.inner.runtime_host.session()
    }

    pub(crate) fn session_manager(&self) -> Arc<dyn InteractiveSessionManager> {
        self.session().session_manager()
    }

    pub(crate) fn settings_manager(&self) -> Arc<dyn InteractiveSettingsManager> {
        self.session().settings_manager()
    }

    pub fn ui(&self) -> Arc<dyn TUI + Send + Sync> {
        Arc::clone(&self.inner.ui) as Arc<dyn TUI + Send + Sync>
    }

    pub fn document_container(&self) -> &Container {
        &self.inner.document_container
    }

    pub fn chat_container(&self) -> &Container {
        &self.inner.chat_container
    }

    pub fn loaded_resources_container(&self) -> &Container {
        &self.inner.loaded_resources_container
    }

    pub fn pending_messages_container(&self) -> &Container {
        &self.inner.pending_messages_container
    }

    pub fn editor_container(&self) -> &Container {
        &self.inner.editor_container
    }

    pub fn status_container(&self) -> &Container {
        &self.inner.status_container
    }

    pub fn header_container(&self) -> &Container {
        &self.inner.header_container
    }

    pub fn program_status(&self) -> &ProgramStatusReporter {
        &self.inner.program_status
    }

    pub fn default_editor(&self) -> &CustomEditor {
        &self.inner.default_editor
    }

    pub fn renderer(&self) -> InteractiveTui {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .renderer
            .clone()
    }

    pub fn active_status_indicator_kind(
        &self,
    ) -> Option<crate::modes::interactive::components::status_indicator::StatusIndicatorKind> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_status_indicator
            .as_ref()
            .map(|ind| ind.kind())
    }

    pub fn active_working_indicator_embedded(&self) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_working_indicator_embedded
    }

    pub fn pending_user_inputs(&self) -> Vec<String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_user_inputs
            .iter()
            .cloned()
            .collect()
    }

    pub fn signal_handler_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .signal_handles
            .len()
    }

    pub fn hide_thinking_block(&self) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .hide_thinking_block
    }
}

// ============================================================================
// Concrete Core Implementations for Interactive Traits
// ============================================================================

impl InteractiveFooterDataProvider for crate::core::footer_data_provider::FooterDataProvider {
    fn set_cwd(&self, cwd: &str) {
        crate::core::footer_data_provider::FooterDataProvider::set_cwd(self, cwd);
    }

    fn set_extension_status(&self, key: &str, text: Option<&str>) {
        crate::core::footer_data_provider::FooterDataProvider::set_extension_status(self, key, text);
    }

    fn clear_extension_statuses(&self) {
        crate::core::footer_data_provider::FooterDataProvider::clear_extension_statuses(self);
    }

    fn set_available_provider_count(&self, count: usize) {
        crate::core::footer_data_provider::FooterDataProvider::set_available_provider_count(self, count);
    }

    fn dispose(&self) {
        crate::core::footer_data_provider::FooterDataProvider::dispose(self);
    }
}

impl InteractiveSettingsManager for crate::core::settings_manager::SettingsManager {
    fn get_terminal_capability_overrides(&self) -> pi_tui::terminal_image::TerminalCapabilityOverrides {
        crate::core::settings_manager::SettingsManager::get_terminal_capability_overrides(self)
    }

    fn get_tui_mode(&self) -> TuiMode {
        crate::core::settings_manager::SettingsManager::get_tui_mode(self)
    }

    fn set_tui_mode(&self, mode: TuiMode) {
        crate::core::settings_manager::SettingsManager::set_tui_mode(self, mode);
    }

    fn get_show_hardware_cursor(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_show_hardware_cursor(self)
    }

    fn set_show_hardware_cursor(&self, enabled: bool) {
        crate::core::settings_manager::SettingsManager::set_show_hardware_cursor(self, enabled);
    }

    fn get_clear_on_shrink(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_clear_on_shrink(self)
    }

    fn set_clear_on_shrink(&self, enabled: bool) {
        crate::core::settings_manager::SettingsManager::set_clear_on_shrink(self, enabled);
    }

    fn get_fullscreen_copy_on_select(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_fullscreen_copy_on_select(self)
    }

    fn set_fullscreen_copy_on_select(&self, enabled: bool) {
        crate::core::settings_manager::SettingsManager::set_fullscreen_copy_on_select(self, enabled);
    }

    fn get_fullscreen_wheel_scroll_lines(&self) -> WheelScrollLines {
        crate::core::settings_manager::SettingsManager::get_fullscreen_wheel_scroll_lines(self)
    }

    fn set_fullscreen_wheel_scroll_lines(&self, lines: WheelScrollLines) {
        crate::core::settings_manager::SettingsManager::set_fullscreen_wheel_scroll_lines(self, lines);
    }

    fn get_fullscreen_exit_output(&self) -> FullscreenExitOutput {
        crate::core::settings_manager::SettingsManager::get_fullscreen_exit_output(self)
    }

    fn set_fullscreen_exit_output(&self, output: FullscreenExitOutput) {
        crate::core::settings_manager::SettingsManager::set_fullscreen_exit_output(self, output);
    }

    fn get_fullscreen_scrollbar(&self) -> ScrollViewScrollbar {
        crate::core::settings_manager::SettingsManager::get_fullscreen_scrollbar(self)
    }

    fn set_fullscreen_scrollbar(&self, mode: ScrollViewScrollbar) {
        crate::core::settings_manager::SettingsManager::set_fullscreen_scrollbar(self, mode);
    }

    fn get_editor_padding_x(&self) -> i64 {
        crate::core::settings_manager::SettingsManager::get_editor_padding_x(self)
    }

    fn set_editor_padding_x(&self, padding: i64) {
        crate::core::settings_manager::SettingsManager::set_editor_padding_x(self, padding);
    }

    fn get_autocomplete_max_visible(&self) -> i64 {
        crate::core::settings_manager::SettingsManager::get_autocomplete_max_visible(self)
    }

    fn set_autocomplete_max_visible(&self, max_visible: i64) {
        crate::core::settings_manager::SettingsManager::set_autocomplete_max_visible(self, max_visible);
    }

    fn get_hide_thinking_block(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_hide_thinking_block(self)
    }

    fn set_hide_thinking_block(&self, hidden: bool) {
        crate::core::settings_manager::SettingsManager::set_hide_thinking_block(self, hidden);
    }

    fn get_output_pad(&self) -> i64 {
        i64::from(crate::core::settings_manager::SettingsManager::get_output_pad(self))
    }

    fn set_output_pad(&self, pad: i64) {
        crate::core::settings_manager::SettingsManager::set_output_pad(self, u8::try_from(pad.max(0)).unwrap_or(1));
    }

    fn get_enable_skill_commands(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_enable_skill_commands(self)
    }

    fn set_enable_skill_commands(&self, enabled: bool) {
        crate::core::settings_manager::SettingsManager::set_enable_skill_commands(self, enabled);
    }

    fn get_collapse_changelog(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_collapse_changelog(self)
    }

    fn set_collapse_changelog(&self, collapsed: bool) {
        crate::core::settings_manager::SettingsManager::set_collapse_changelog(self, collapsed);
    }

    fn get_code_block_indent(&self) -> usize {
        crate::core::settings_manager::SettingsManager::get_code_block_indent(self)
            .chars()
            .count()
    }

    fn get_quiet_startup(&self) -> QuietStartup {
        crate::core::settings_manager::SettingsManager::get_quiet_startup(self).into()
    }

    fn set_quiet_startup(&self, quiet: QuietStartup) {
        crate::core::settings_manager::SettingsManager::set_quiet_startup(self, quiet.into());
    }

    fn get_mermaid_rendering_mode(&self) -> MermaidRenderingMode {
        crate::core::settings_manager::SettingsManager::get_mermaid_rendering_mode(self)
    }

    fn set_mermaid_rendering_mode(&self, mode: MermaidRenderingMode) {
        crate::core::settings_manager::SettingsManager::set_mermaid_rendering_mode(self, mode);
    }

    fn get_show_cache_miss_notices(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_show_cache_miss_notices(self)
    }

    fn set_show_cache_miss_notices(&self, shown: bool) {
        crate::core::settings_manager::SettingsManager::set_show_cache_miss_notices(self, shown);
    }

    fn get_show_terminal_progress(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_show_terminal_progress(self)
    }

    fn set_show_terminal_progress(&self, enabled: bool) {
        crate::core::settings_manager::SettingsManager::set_show_terminal_progress(self, enabled);
    }

    fn get_show_images(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_show_images(self)
    }

    fn set_show_images(&self, enabled: bool) {
        crate::core::settings_manager::SettingsManager::set_show_images(self, enabled);
    }

    fn get_image_width_cells(&self) -> Option<i64> {
        self.get_settings()
            .terminal
            .and_then(|t| t.image_width_cells)
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite())
            .map(|w| (w.floor() as i64).max(1))
    }

    fn set_image_width_cells(&self, width: Option<i64>) {
        self.set_image_width_cells_opt(width);
    }

    fn get_image_auto_resize(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_image_auto_resize(self)
    }

    fn set_image_auto_resize(&self, enabled: bool) {
        crate::core::settings_manager::SettingsManager::set_image_auto_resize(self, enabled);
    }

    fn get_block_images(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_block_images(self)
    }

    fn set_block_images(&self, blocked: bool) {
        crate::core::settings_manager::SettingsManager::set_block_images(self, blocked);
    }

    fn get_theme(&self) -> Option<String> {
        crate::core::settings_manager::SettingsManager::get_theme(self)
    }

    fn set_theme(&self, theme: &str) {
        crate::core::settings_manager::SettingsManager::set_theme(self, theme);
    }

    fn get_last_changelog_version(&self) -> Option<String> {
        crate::core::settings_manager::SettingsManager::get_last_changelog_version(self)
    }

    fn set_last_changelog_version(&self, version: &str) {
        crate::core::settings_manager::SettingsManager::set_last_changelog_version(self, version);
    }

    fn get_extension_changelog_version(&self, key: &str) -> Option<String> {
        crate::core::settings_manager::SettingsManager::get_extension_changelog_version(self, key)
    }

    fn set_extension_changelog_version(&self, key: &str, version: &str) {
        crate::core::settings_manager::SettingsManager::set_extension_changelog_version(self, key, version);
    }

    fn get_http_idle_timeout_ms(&self) -> u64 {
        crate::core::settings_manager::SettingsManager::get_http_idle_timeout_ms(self)
            .unwrap_or(crate::core::http_dispatcher::DEFAULT_HTTP_IDLE_TIMEOUT_MS)
    }

    fn set_http_idle_timeout_ms(&self, timeout_ms: u64) {
        let _ = crate::core::settings_manager::SettingsManager::set_http_idle_timeout_ms(self, timeout_ms as f64);
    }

    fn get_cache_warming_mode(&self) -> CacheWarmingMode {
        crate::core::settings_manager::SettingsManager::get_cache_warming_mode(self)
    }

    fn set_cache_warming_mode(&self, mode: CacheWarmingMode) {
        crate::core::settings_manager::SettingsManager::set_cache_warming_mode(self, mode);
    }

    fn get_transport(&self) -> pi_ai::Transport {
        crate::core::settings_manager::SettingsManager::get_transport(self)
    }

    fn set_transport(&self, transport: pi_ai::Transport) {
        crate::core::settings_manager::SettingsManager::set_transport(self, transport);
    }

    fn get_session_share_destination(&self) -> SessionShareDestination {
        crate::core::settings_manager::SettingsManager::get_session_share_destination(self)
    }

    fn set_session_share_destination(&self, destination: SessionShareDestination) {
        crate::core::settings_manager::SettingsManager::set_session_share_destination(self, destination);
    }

    fn get_default_provider(&self) -> Option<String> {
        crate::core::settings_manager::SettingsManager::get_default_provider(self)
    }

    fn get_default_model(&self) -> Option<String> {
        crate::core::settings_manager::SettingsManager::get_default_model(self)
    }

    fn get_default_thinking_level(&self) -> Option<ModelThinkingLevel> {
        crate::core::settings_manager::SettingsManager::get_default_thinking_level(self)
    }

    fn get_all_model_thinking_levels(&self) -> IndexMap<String, ModelThinkingLevel> {
        crate::core::settings_manager::SettingsManager::get_all_model_thinking_levels(self)
    }

    fn set_model_thinking_level(&self, provider: &str, model_id: &str, level: ModelThinkingLevel) {
        crate::core::settings_manager::SettingsManager::set_model_thinking_level(self, provider, model_id, level);
    }

    fn remove_model_thinking_level(&self, provider: &str, model_id: &str) {
        crate::core::settings_manager::SettingsManager::remove_model_thinking_level(self, provider, model_id);
    }

    fn get_enabled_models(&self) -> Option<Vec<String>> {
        crate::core::settings_manager::SettingsManager::get_enabled_models(self)
    }

    fn set_enabled_models(&self, patterns: Option<Vec<String>>) {
        crate::core::settings_manager::SettingsManager::set_enabled_models(self, patterns);
    }

    fn get_warnings(&self) -> WarningSettings {
        crate::core::settings_manager::SettingsManager::get_warnings(self)
    }

    fn set_warnings(&self, warnings: WarningSettings) {
        crate::core::settings_manager::SettingsManager::set_warnings(self, warnings);
    }

    fn get_enable_install_telemetry(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_enable_install_telemetry(self)
    }

    fn set_enable_install_telemetry(&self, enabled: bool) {
        crate::core::settings_manager::SettingsManager::set_enable_install_telemetry(self, enabled);
    }

    fn get_default_project_trust(&self) -> DefaultProjectTrust {
        crate::core::settings_manager::SettingsManager::get_default_project_trust(self)
    }

    fn set_default_project_trust(&self, trust: DefaultProjectTrust) {
        crate::core::settings_manager::SettingsManager::set_default_project_trust(self, trust);
    }

    fn get_double_escape_action(&self) -> DoubleEscapeAction {
        crate::core::settings_manager::SettingsManager::get_double_escape_action(self)
    }

    fn set_double_escape_action(&self, action: DoubleEscapeAction) {
        crate::core::settings_manager::SettingsManager::set_double_escape_action(self, action);
    }

    fn get_tree_filter_mode(&self) -> TreeFilterMode {
        crate::core::settings_manager::SettingsManager::get_tree_filter_mode(self)
    }

    fn set_tree_filter_mode(&self, mode: TreeFilterMode) {
        crate::core::settings_manager::SettingsManager::set_tree_filter_mode(self, mode);
    }

    fn get_branch_summary_skip_prompt(&self) -> bool {
        crate::core::settings_manager::SettingsManager::get_branch_summary_skip_prompt(self)
    }

    fn get_external_editor_command(&self) -> Option<String> {
        self.get_settings()
            .external_editor
            .map(|s| pi_js::str16::trim(&s).to_owned())
            .filter(|s| !s.is_empty())
    }

    fn get_or_create_device_id(&self) -> String {
        crate::core::settings_manager::SettingsManager::get_or_create_device_id(self)
    }

    fn is_project_trusted(&self) -> bool {
        crate::core::settings_manager::SettingsManager::is_project_trusted(self)
    }

    fn as_core_settings_manager(&self) -> Option<&crate::core::settings_manager::SettingsManager> {
        Some(self)
    }
}

impl InteractiveSessionManager for crate::core::session_manager::SessionManager {
    fn get_cwd(&self) -> String {
        crate::core::session_manager::SessionManager::get_cwd(self)
    }

    fn get_session_dir(&self) -> String {
        crate::core::session_manager::SessionManager::get_session_dir(self)
    }

    fn get_session_id(&self) -> String {
        crate::core::session_manager::SessionManager::get_session_id(self)
    }

    fn get_session_file(&self) -> Option<String> {
        crate::core::session_manager::SessionManager::get_session_file(self)
    }

    fn get_session_name(&self) -> Option<String> {
        crate::core::session_manager::SessionManager::get_session_name(self)
    }

    fn is_persisted(&self) -> bool {
        crate::core::session_manager::SessionManager::is_persisted(self)
    }

    fn uses_default_session_dir(&self) -> bool {
        crate::core::session_manager::SessionManager::uses_default_session_dir(self)
    }

    fn get_leaf_id(&self) -> Option<String> {
        crate::core::session_manager::SessionManager::get_leaf_id(self)
    }

    fn get_tree(&self) -> Vec<SessionTreeNode> {
        crate::core::session_manager::SessionManager::get_tree(self)
    }

    fn get_entries(&self) -> Vec<SessionEntry> {
        crate::core::session_manager::SessionManager::get_entries(self)
    }

    fn get_branch(&self) -> Vec<SessionEntry> {
        crate::core::session_manager::SessionManager::get_branch(self, None)
    }

    fn build_context_entries(&self) -> Vec<SessionEntry> {
        crate::core::session_manager::SessionManager::build_context_entries(self)
    }

    fn append_label_change(&self, entry_id: &str, label: Option<&str>) {
        let _ = crate::core::session_manager::SessionManager::append_label_change(self, entry_id, label);
    }

    fn list_current_sessions(
        &self,
        on_progress: Option<SessionListProgress>,
        signal: AbortSignal,
    ) -> BoxFuture<Result<Vec<SessionInfo>>> {
        let cwd = self.get_cwd();
        let session_dir = self.get_session_dir();
        Box::pin(async move {
            Self::list(
                &cwd,
                Some(&session_dir),
                on_progress
                    .as_deref()
                    .map(|cb| cb as crate::core::session_manager::SessionListProgress<'_>),
                Some(&signal),
            )
            .await
        })
    }

    fn list_all_sessions(
        &self,
        on_progress: Option<SessionListProgress>,
        signal: AbortSignal,
    ) -> BoxFuture<Result<Vec<SessionInfo>>> {
        let custom_dir = if self.uses_default_session_dir() {
            None
        } else {
            Some(self.get_session_dir())
        };
        Box::pin(async move {
            Self::list_all(
                custom_dir.as_deref(),
                on_progress
                    .as_deref()
                    .map(|cb| cb as crate::core::session_manager::SessionListProgress<'_>),
                Some(&signal),
            )
            .await
        })
    }

    fn rename_session(&self, session_file: &str, next_name: Option<&str>) -> BoxFuture<Result<()>> {
        let sm = self.clone();
        let path = session_file.to_string();
        let name = next_name.unwrap_or("").to_string();
        Box::pin(async move {
            if sm.get_session_file().as_deref() == Some(path.as_str()) {
                sm.append_session_info(&name)?;
            } else {
                let opened = Self::open(&path, None, None)?;
                opened.append_session_info(&name)?;
            }
            Ok(())
        })
    }
}

struct SignalAuthInteraction {
    inner: Arc<dyn pi_ai::auth::types::AuthInteraction>,
    signal: AbortSignal,
}

#[async_trait]
impl pi_ai::auth::types::AuthInteraction for SignalAuthInteraction {
    fn signal(&self) -> Option<&AbortSignal> {
        Some(&self.signal)
    }

    async fn prompt(&self, prompt: pi_ai::auth::types::AuthPrompt) -> Result<String> {
        self.inner.prompt(prompt).await
    }

    fn notify(&self, event: pi_ai::auth::types::AuthEvent) {
        self.inner.notify(event);
    }
}

#[async_trait]
impl InteractiveModelRuntime for Arc<crate::core::model_runtime::ModelRuntime> {
    fn get_providers(&self) -> Vec<ProviderAuthInfo> {
        crate::core::model_runtime::ModelRuntime::get_providers(self)
            .into_iter()
            .map(|p| {
                let auth = p.auth();
                let oauth = auth.oauth.as_ref();
                let api_key = auth.api_key.as_ref();
                ProviderAuthInfo {
                    id: p.id().to_owned(),
                    name: p.name().to_owned(),
                    has_oauth: oauth.is_some(),
                    oauth_method_name: oauth.map(|o| o.name().to_owned()),
                    oauth_login_label: oauth.and_then(|o| o.login_label().map(str::to_owned)),
                    oauth_is_subscription: oauth.is_some_and(|o| o.is_subscription()),
                    has_api_key: api_key.is_some(),
                    api_key_method_name: api_key.map(|a| a.name().to_owned()),
                    api_key_has_login: api_key.is_some_and(|a| a.supports_login()),
                }
            })
            .collect()
    }

    fn get_provider_auth_status(&self, provider_id: &str) -> ProviderAuthStatusInfo {
        let status = crate::core::model_runtime::ModelRuntime::get_provider_auth_status(self, provider_id);
        let source = status.source.map(|src| {
            match src {
                crate::core::provider_composer::AuthStatusSource::Stored => "stored",
                crate::core::provider_composer::AuthStatusSource::Runtime => "runtime",
                crate::core::provider_composer::AuthStatusSource::Environment => "environment",
                crate::core::provider_composer::AuthStatusSource::Fallback => "fallback",
                crate::core::provider_composer::AuthStatusSource::ModelsJsonKey => "models_json_key",
                crate::core::provider_composer::AuthStatusSource::ModelsJsonCommand => "models_json_command",
            }
            .to_owned()
        });
        ProviderAuthStatusInfo {
            configured: status.configured,
            label: status.label,
            source,
        }
    }

    fn is_using_oauth(&self, provider_id: &str) -> bool {
        crate::core::model_runtime::ModelRuntime::is_using_oauth(self, provider_id)
    }

    async fn check_auth_type(&self, provider_id: &str) -> Option<AuthType> {
        self.check_auth(provider_id, None)
            .await
            .ok()
            .flatten()
            .map(|c| c.auth_type)
    }

    async fn get_api_key(&self, provider_id: &str) -> Option<String> {
        let auth = self.get_auth_provider_id(provider_id, None).await.ok().flatten()?;
        crate::cli::auth_command::get_auth_credential(Some(&auth))
    }

    async fn list_credentials(&self, signal: AbortSignal) -> Result<Vec<StoredCredentialInfo>> {
        let creds = crate::core::model_runtime::ModelRuntime::list_credentials(
            self,
            Some(pi_ai::AuthOperationOptions { signal: Some(signal) }),
        )
        .await?;
        Ok(creds
            .into_iter()
            .map(|c| StoredCredentialInfo {
                provider_id: c.provider_id,
                auth_type: c.credential_type,
            })
            .collect())
    }

    async fn logout(&self, provider_id: &str, signal: AbortSignal) -> Result<()> {
        crate::core::model_runtime::ModelRuntime::logout(
            self,
            provider_id,
            Some(pi_ai::AuthOperationOptions { signal: Some(signal) }),
        )
        .await
    }

    async fn refresh_providers(
        &self,
        providers: Vec<String>,
        signal: AbortSignal,
    ) -> Result<crate::modes::interactive::model_catalog_refresh::ModelCatalogRefreshResult> {
        let res = crate::core::model_runtime::ModelRuntime::refresh(
            self,
            Some(pi_ai::ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(providers),
                force: Some(true),
                signal: Some(signal),
            }),
        )
        .await?;
        Ok(
            crate::modes::interactive::model_catalog_refresh::ModelCatalogRefreshResult {
                aborted: res.aborted,
                errors: res.errors.into_iter().map(|(k, e)| (k, e.to_string())).collect(),
            },
        )
    }

    async fn login(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        interaction: Arc<dyn pi_ai::auth::types::AuthInteraction>,
        signal: AbortSignal,
        device_id: String,
    ) -> Result<()> {
        let wrapped = Arc::new(SignalAuthInteraction {
            inner: interaction,
            signal,
        });
        let options = pi_ai::auth::types::LoginOptions {
            get_device_id: Some(Arc::new(move || device_id.clone())),
        };
        crate::core::model_runtime::ModelRuntime::login(self, provider_id, auth_type, wrapped, Some(options)).await?;
        Ok(())
    }
}

pub(crate) fn core_source_info_to_interactive(info: &crate::core::source_info::SourceInfo) -> SourceInfo {
    let scope = match info.scope {
        crate::core::source_info::SourceScope::User => "user",
        crate::core::source_info::SourceScope::Project => "project",
        crate::core::source_info::SourceScope::Temporary => "temporary",
    };
    let origin = match info.origin {
        crate::core::source_info::SourceOrigin::Package => "package",
        crate::core::source_info::SourceOrigin::TopLevel => "top-level",
    };
    SourceInfo {
        path: info.path.clone(),
        source: info.source.clone(),
        scope: scope.to_owned(),
        origin: origin.to_owned(),
        base_dir: info.base_dir.clone(),
        changelog_path: info.changelog_path.clone(),
    }
}

pub(crate) fn core_diag_to_interactive(diag: &crate::core::diagnostics::ResourceDiagnostic) -> ResourceDiagnostic {
    let diagnostic_type = match diag.r#type {
        crate::core::diagnostics::ResourceDiagnosticType::Warning => DiagnosticLevel::Warning,
        crate::core::diagnostics::ResourceDiagnosticType::Error => DiagnosticLevel::Error,
        crate::core::diagnostics::ResourceDiagnosticType::Collision => DiagnosticLevel::Collision,
    };
    ResourceDiagnostic {
        diagnostic_type,
        message: diag.message.clone(),
        path: diag.path.clone(),
        collision: diag.collision.as_ref().map(|c| ResourceCollision {
            name: c.name.clone(),
            winner_path: c.winner_path.clone(),
            loser_path: c.loser_path.clone(),
        }),
    }
}

fn core_loader_skills(
    loader: &dyn crate::core::resource_loader::ResourceLoader,
) -> (Vec<SkillInfo>, Vec<ResourceDiagnostic>) {
    let (skills, diagnostics) = loader.get_skills();
    (
        skills
            .into_iter()
            .map(|s| SkillInfo {
                name: s.name,
                description: s.description,
                file_path: s.file_path,
                source_info: Some(core_source_info_to_interactive(&s.source_info)),
            })
            .collect(),
        diagnostics.iter().map(core_diag_to_interactive).collect(),
    )
}

fn core_loader_prompts(
    loader: &dyn crate::core::resource_loader::ResourceLoader,
) -> (Vec<PromptTemplateInfo>, Vec<ResourceDiagnostic>) {
    let (prompts, diagnostics) = loader.get_prompts();
    (
        prompts
            .into_iter()
            .map(|p| PromptTemplateInfo {
                name: p.name,
                description: if p.description.is_empty() {
                    None
                } else {
                    Some(p.description)
                },
                argument_hint: p.argument_hint,
                file_path: p.file_path,
                source_info: Some(core_source_info_to_interactive(&p.source_info)),
            })
            .collect(),
        diagnostics.iter().map(core_diag_to_interactive).collect(),
    )
}

fn core_loader_themes(
    loader: &dyn crate::core::resource_loader::ResourceLoader,
) -> (Vec<Theme>, Vec<LoadedThemeResource>, Vec<ResourceDiagnostic>) {
    let (core_themes, diagnostics) = loader.get_themes();
    let mut themes = Vec::new();
    let mut resources = Vec::new();
    for ct in core_themes {
        if let Some(tui_theme) = ct.to_tui_theme() {
            themes.push(tui_theme.clone());
            resources.push(LoadedThemeResource {
                theme: tui_theme,
                source_path: ct.source_path,
                source_info: ct.source_info.as_ref().map(core_source_info_to_interactive),
            });
        }
    }
    (
        themes,
        resources,
        diagnostics.iter().map(core_diag_to_interactive).collect(),
    )
}

fn core_loader_extensions(loader: &dyn crate::core::resource_loader::ResourceLoader) -> LoadedExtensionsResult {
    let ext_res = loader.get_extensions();
    LoadedExtensionsResult {
        extensions: ext_res
            .extensions
            .into_iter()
            .map(|ext| LoadedExtensionInfo {
                path: ext.path,
                hidden: ext.hidden,
                source_info: Some(core_source_info_to_interactive(&ext.source_info)),
            })
            .collect(),
        errors: ext_res
            .errors
            .into_iter()
            .map(|e| ExtensionLoadMessage {
                path: e.path,
                message: e.error,
            })
            .collect(),
        warnings: ext_res
            .warnings
            .into_iter()
            .map(|w| ExtensionLoadMessage {
                path: w.path,
                message: w.warning,
            })
            .collect(),
    }
}

pub struct InteractiveResourceLoaderAdapter(pub Arc<dyn crate::core::resource_loader::ResourceLoader>);

impl InteractiveResourceLoader for InteractiveResourceLoaderAdapter {
    fn get_skills(&self) -> (Vec<SkillInfo>, Vec<ResourceDiagnostic>) {
        core_loader_skills(self.0.as_ref())
    }

    fn get_prompts(&self) -> (Vec<PromptTemplateInfo>, Vec<ResourceDiagnostic>) {
        core_loader_prompts(self.0.as_ref())
    }

    fn get_themes(&self) -> (Vec<Theme>, Vec<LoadedThemeResource>, Vec<ResourceDiagnostic>) {
        core_loader_themes(self.0.as_ref())
    }

    fn get_extensions(&self) -> LoadedExtensionsResult {
        core_loader_extensions(self.0.as_ref())
    }

    fn get_system_prompt_source(&self) -> Option<SourceInfo> {
        self.0.get_system_prompt_source().map(|p| SourceInfo {
            path: p.path,
            ..SourceInfo::default()
        })
    }

    fn get_append_system_prompt_sources(&self) -> Vec<SourceInfo> {
        self.0
            .get_append_system_prompt_sources()
            .into_iter()
            .map(|p| SourceInfo {
                path: p.path,
                ..SourceInfo::default()
            })
            .collect()
    }

    fn get_agents_files(&self) -> Vec<SourceInfo> {
        self.0
            .get_agents_files()
            .into_iter()
            .map(|f| SourceInfo {
                path: f.path,
                ..SourceInfo::default()
            })
            .collect()
    }
}

impl InteractiveResourceLoader for crate::core::resource_loader::DefaultResourceLoader {
    fn get_skills(&self) -> (Vec<SkillInfo>, Vec<ResourceDiagnostic>) {
        core_loader_skills(self)
    }

    fn get_prompts(&self) -> (Vec<PromptTemplateInfo>, Vec<ResourceDiagnostic>) {
        core_loader_prompts(self)
    }

    fn get_themes(&self) -> (Vec<Theme>, Vec<LoadedThemeResource>, Vec<ResourceDiagnostic>) {
        core_loader_themes(self)
    }

    fn get_extensions(&self) -> LoadedExtensionsResult {
        core_loader_extensions(self)
    }

    fn get_system_prompt_source(&self) -> Option<SourceInfo> {
        crate::core::resource_loader::ResourceLoader::get_system_prompt_source(self).map(|p| SourceInfo {
            path: p.path,
            ..SourceInfo::default()
        })
    }

    fn get_append_system_prompt_sources(&self) -> Vec<SourceInfo> {
        crate::core::resource_loader::ResourceLoader::get_append_system_prompt_sources(self)
            .into_iter()
            .map(|p| SourceInfo {
                path: p.path,
                ..SourceInfo::default()
            })
            .collect()
    }

    fn get_agents_files(&self) -> Vec<SourceInfo> {
        crate::core::resource_loader::ResourceLoader::get_agents_files(self)
            .into_iter()
            .map(|f| SourceInfo {
                path: f.path,
                ..SourceInfo::default()
            })
            .collect()
    }
}

struct InteractiveExtensionRunnerAdapter {
    runner: Option<crate::core::extensions::runner::ExtensionRunner>,
    session: crate::core::agent_session::AgentSession,
}

impl InteractiveExtensionRunner for InteractiveExtensionRunnerAdapter {
    fn get_registered_commands(&self) -> Vec<RegisteredExtensionCommand> {
        let Some(ref runner) = self.runner else {
            return Vec::new();
        };
        runner
            .get_registered_commands()
            .into_iter()
            .map(|c| RegisteredExtensionCommand {
                name: c.command.name,
                invocation_name: c.invocation_name,
                description: c.command.description,
                source_info: core_source_info_to_interactive(&c.command.source_info),
                get_argument_completions: c.command.get_argument_completions,
            })
            .collect()
    }

    fn has_command(&self, name: &str) -> bool {
        self.runner.as_ref().is_some_and(|r| r.get_command(name).is_some())
    }

    fn get_command_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        self.runner
            .as_ref()
            .map(|r| {
                r.get_command_diagnostics()
                    .iter()
                    .map(core_diag_to_interactive)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn get_shortcut_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        self.runner
            .as_ref()
            .map(|r| {
                r.get_shortcut_diagnostics()
                    .iter()
                    .map(core_diag_to_interactive)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn get_shortcuts(&self) -> IndexMap<String, RegisteredExtensionShortcut> {
        let Some(ref runner) = self.runner else {
            return IndexMap::new();
        };
        let agent_dir = self.session.settings_manager().get_agent_dir();
        let kb = KeybindingsManager::create(agent_dir.as_deref());
        let resolved = kb.get_resolved_bindings();
        runner
            .get_shortcuts(&resolved)
            .into_iter()
            .map(|(k, sc)| {
                let runner_c = runner.clone();
                let ext_path = sc.extension_path.clone();
                let handler_inner = sc.handler;
                let handler: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync> = Arc::new(move || {
                    let runner_call = runner_c.clone();
                    let ext_path_call = ext_path.clone();
                    let h = Arc::clone(&handler_inner);
                    Box::pin(async move {
                        let ctx = runner_call.create_context();
                        if let Err(err) = h(ctx).await {
                            runner_call.emit_error(crate::core::extensions::types::ExtensionError {
                                extension_path: ext_path_call,
                                event: "shortcut".to_owned(),
                                error: err.to_string(),
                                stack: None,
                            });
                        }
                    })
                });
                (
                    k.clone(),
                    RegisteredExtensionShortcut {
                        key: sc.shortcut,
                        description: sc.description,
                        extension_path: sc.extension_path,
                        handler,
                    },
                )
            })
            .collect()
    }

    fn get_markdown_transformers(&self) -> Vec<MarkdownTransformer> {
        self.runner
            .as_ref()
            .map(crate::core::extensions::runner::ExtensionRunner::get_markdown_transformers)
            .unwrap_or_default()
    }

    fn get_entry_renderer(&self, custom_type: &str) -> Option<EntryRenderer> {
        self.runner.as_ref().and_then(|r| r.get_entry_renderer(custom_type))
    }

    fn get_message_renderer(&self, custom_type: &str) -> Option<MessageRenderer> {
        self.runner.as_ref().and_then(|r| r.get_message_renderer(custom_type))
    }

    fn has_user_bash_handlers(&self) -> bool {
        self.runner.as_ref().is_some_and(|r| r.has_handlers("user_bash"))
    }

    fn emit_user_bash(
        &self,
        command: &str,
        exclude_from_context: bool,
        cwd: &str,
    ) -> BoxFuture<Result<Option<BashExecutionResult>>> {
        let Some(runner) = self.runner.clone() else {
            return Box::pin(async { Ok(None) });
        };
        let session = self.session.clone();
        let command = command.to_owned();
        let cwd = cwd.to_owned();
        Box::pin(async move {
            let res = runner
                .emit_user_bash(crate::core::extensions::types::UserBashEvent {
                    command,
                    exclude_from_context,
                    cwd,
                })
                .await?;
            match res {
                None => Ok(None),
                Some(crate::core::extensions::types::UserBashEventResult::Result { result }) => {
                    Ok(Some(BashExecutionResult {
                        output: result.output,
                        exit_code: result.exit_code.map(i64::from),
                        cancelled: result.cancelled,
                        truncated: result.truncated,
                        full_output_path: result.full_output_path,
                    }))
                }
                Some(crate::core::extensions::types::UserBashEventResult::Operations { operations }) => {
                    session
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .pending_user_bash_operations = Some(operations);
                    Ok(None)
                }
            }
        })
    }
}

struct SendSyncTuiWrapper(Arc<dyn TUI>);

impl Component for SendSyncTuiWrapper {
    fn render(&self, width: usize) -> Vec<String> {
        self.0.render(width)
    }

    fn handle_input(&self, data: &str) {
        self.0.handle_input(data);
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        self.0.wants_key_release()
    }

    fn invalidate(&self) {
        self.0.invalidate();
    }

    fn has_handle_input(&self) -> bool {
        self.0.has_handle_input()
    }

    fn is_container_handle_mouse(&self) -> bool {
        self.0.is_container_handle_mouse()
    }

    fn as_focusable(&self) -> Option<&dyn pi_tui::Focusable> {
        self.0.as_focusable()
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        self.0.container_children()
    }

    fn as_layout_component(&self) -> Option<&dyn pi_tui::layout_node::LayoutComponent> {
        self.0.as_layout_component()
    }
}

#[async_trait]
impl TUI for SendSyncTuiWrapper {
    fn mode(&self) -> pi_tui::TuiMode {
        self.0.mode()
    }

    fn children(&self) -> Vec<Arc<dyn Component>> {
        self.0.children()
    }

    fn set_children(&self, children: Vec<Arc<dyn Component>>) {
        self.0.set_children(children);
    }

    fn terminal(&self) -> Arc<dyn Terminal> {
        self.0.terminal()
    }

    fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.0.on_debug()
    }

    fn set_on_debug(&self, on_debug: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.0.set_on_debug(on_debug);
    }

    fn full_redraws(&self) -> i64 {
        self.0.full_redraws()
    }

    fn add_child(&self, component: Arc<dyn Component>) {
        self.0.add_child(component);
    }

    fn remove_child(&self, component: &Arc<dyn Component>) {
        self.0.remove_child(component);
    }

    fn clear(&self) {
        self.0.clear();
    }

    fn get_show_hardware_cursor(&self) -> bool {
        self.0.get_show_hardware_cursor()
    }

    fn set_show_hardware_cursor(&self, enabled: bool) {
        self.0.set_show_hardware_cursor(enabled);
    }

    fn get_clear_on_shrink(&self) -> bool {
        self.0.get_clear_on_shrink()
    }

    fn set_clear_on_shrink(&self, enabled: bool) {
        self.0.set_clear_on_shrink(enabled);
    }

    fn set_focus(&self, component: Option<Arc<dyn Component>>) {
        self.0.set_focus(component);
    }

    fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
        self.0.show_overlay(component, options)
    }

    fn hide_overlay(&self) {
        self.0.hide_overlay();
    }

    fn has_overlay(&self) -> bool {
        self.0.has_overlay()
    }

    fn start(&self) {
        self.0.start();
    }

    fn stop(&self, options: Option<pi_tui::tui::TuiStopOptions>) {
        self.0.stop(options);
    }

    fn render_now(&self, force: Option<bool>) {
        self.0.render_now(force);
    }

    fn request_render(&self, force: Option<bool>) {
        self.0.request_render(force);
    }

    fn add_input_listener(&self, listener: pi_tui::tui::TuiInputListener) -> Unsubscribe {
        self.0.add_input_listener(listener)
    }

    fn remove_input_listener(&self, listener: &pi_tui::tui::TuiInputListener) {
        self.0.remove_input_listener(listener);
    }

    fn on_terminal_color_scheme_change(
        &self,
        cb: Arc<dyn Fn(pi_tui::TerminalColorScheme) + Send + Sync>,
    ) -> Unsubscribe {
        self.0.on_terminal_color_scheme_change(cb)
    }

    fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        self.0.set_terminal_color_scheme_notifications(enabled);
    }

    async fn query_terminal_colors(&self, options: pi_tui::tui::QueryTerminalColorsOptions) -> pi_tui::TerminalColors {
        self.0.query_terminal_colors(options).await
    }
}

struct InteractiveUiContextAdapter(Arc<dyn ExtensionUiContext>);

#[async_trait]
impl crate::core::extensions::types::ExtensionUIContext for InteractiveUiContextAdapter {
    async fn select(
        &self,
        title: &str,
        options: Vec<String>,
        opts: Option<crate::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> Result<Option<String>> {
        let mapped = opts.map(|o| ExtensionUIDialogOptions {
            timeout_ms: o.timeout.map(|v| v as i64),
            signal: o.signal,
        });
        Ok(self.0.select(title.to_owned(), options, mapped).await)
    }

    async fn confirm(
        &self,
        title: &str,
        message: &str,
        opts: Option<crate::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> Result<bool> {
        let mapped = opts.map(|o| ExtensionUIDialogOptions {
            timeout_ms: o.timeout.map(|v| v as i64),
            signal: o.signal,
        });
        Ok(self.0.confirm(title.to_owned(), message.to_owned(), mapped).await)
    }

    async fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<crate::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> Result<Option<String>> {
        let mapped = opts.map(|o| ExtensionUIDialogOptions {
            timeout_ms: o.timeout.map(|v| v as i64),
            signal: o.signal,
        });
        Ok(self
            .0
            .input(title.to_owned(), placeholder.map(str::to_owned), mapped)
            .await)
    }

    fn notify(&self, message: &str, notify_type: Option<crate::core::extensions::types::NotifyType>) {
        let kind = notify_type.map(|t| match t {
            crate::core::extensions::types::NotifyType::Info => NotifyKind::Info,
            crate::core::extensions::types::NotifyType::Warning => NotifyKind::Warning,
            crate::core::extensions::types::NotifyType::Error => NotifyKind::Error,
        });
        self.0.notify(message, kind);
    }

    fn on_terminal_input(&self, handler: crate::core::extensions::types::TerminalInputHandler) -> Unsubscribe {
        let wrapped: pi_tui::tui::TuiInputListener = Arc::new(move |data| {
            let consume = handler(data).is_some_and(|r| r.consume.unwrap_or(false));
            Some(pi_tui::tui::TuiInputListenerResult {
                consume: Some(consume),
                data: None,
            })
        });
        self.0.on_terminal_input(wrapped)
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        self.0.set_status(key, text);
    }

    fn set_working_message(&self, message: Option<&str>) {
        self.0.set_working_message(message);
    }

    fn set_working_visible(&self, visible: bool) {
        self.0.set_working_visible(visible);
    }

    fn set_working_indicator(&self, options: Option<crate::core::extensions::types::WorkingIndicatorOptions>) {
        let mapped = options.map(|o| pi_tui::LoaderIndicatorOptions {
            frames: o.frames,
            interval_ms: o.interval_ms,
        });
        self.0.set_working_indicator(mapped);
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        self.0.set_hidden_thinking_label(label);
    }

    fn set_widget_lines(
        &self,
        key: &str,
        content: Option<Vec<String>>,
        options: Option<crate::core::extensions::types::ExtensionWidgetOptions>,
    ) {
        let mapped_opts = options.map(|o| ExtensionWidgetOptions {
            placement: match o.placement {
                Some(crate::core::extensions::types::WidgetPlacement::BelowEditor) => WidgetPlacement::BelowEditor,
                _ => WidgetPlacement::AboveEditor,
            },
        });
        self.0.set_widget(key, content.map(WidgetContent::Lines), mapped_opts);
    }

    fn set_widget_factory(
        &self,
        key: &str,
        factory: Option<crate::core::extensions::types::WidgetFactory>,
        options: Option<crate::core::extensions::types::ExtensionWidgetOptions>,
    ) {
        let mapped_opts = options.map(|o| ExtensionWidgetOptions {
            placement: match o.placement {
                Some(crate::core::extensions::types::WidgetPlacement::BelowEditor) => WidgetPlacement::BelowEditor,
                _ => WidgetPlacement::AboveEditor,
            },
        });
        let mapped_factory = factory.map(|f| {
            WidgetContent::Factory(Arc::new(move |tui, th| {
                f(
                    tui as Arc<dyn TUI>,
                    crate::core::extensions::types::Theme::new(Arc::new(th)),
                )
            }))
        });
        self.0.set_widget(key, mapped_factory, mapped_opts);
    }

    fn set_footer(&self, factory: Option<crate::core::extensions::types::FooterFactory>) {
        let mapped = factory.map(|f| {
            Arc::new(move |tui: Arc<dyn TUI + Send + Sync>, th, footer_data| {
                f(
                    tui as Arc<dyn TUI>,
                    crate::core::extensions::types::Theme::new(Arc::new(th)),
                    footer_data,
                )
            }) as FooterFactory
        });
        self.0.set_footer(mapped);
    }

    fn set_header(&self, factory: Option<crate::core::extensions::types::HeaderFactory>) {
        let mapped = factory.map(|f| {
            Arc::new(move |tui: Arc<dyn TUI + Send + Sync>, th| {
                f(
                    tui as Arc<dyn TUI>,
                    crate::core::extensions::types::Theme::new(Arc::new(th)),
                )
            }) as HeaderFactory
        });
        self.0.set_header(mapped);
    }

    fn set_title(&self, title: &str) {
        self.0.set_title(title);
    }

    async fn custom(
        &self,
        factory: crate::core::extensions::types::CustomUiFactory,
        options: Option<crate::core::extensions::types::ExtensionCustomOptions>,
    ) -> Result<Value> {
        let mapped_factory: CustomUiFactory = Box::new(move |tui, th, kb, done| {
            factory(
                tui as Arc<dyn TUI>,
                crate::core::extensions::types::Theme::new(Arc::new(th)),
                kb.to_tui_manager(),
                done,
            )
        });
        let mapped_opts = options.map(|o| {
            let resolved_overlay_options = o.overlay_options.map(|src| match src {
                crate::core::extensions::types::OverlayOptionsSource::Static(opts) => opts,
                crate::core::extensions::types::OverlayOptionsSource::Dynamic(f) => f(),
            });
            ExtensionCustomOptions {
                overlay: o.overlay.unwrap_or(false),
                overlay_options: resolved_overlay_options,
                on_handle: o.on_handle,
                blocked: None,
            }
        });
        self.0.custom(mapped_factory, mapped_opts).await
    }

    fn paste_to_editor(&self, text: &str) {
        self.0.paste_to_editor(text);
    }

    fn set_editor_text(&self, text: &str) {
        self.0.set_editor_text(text);
    }

    fn get_editor_text(&self) -> String {
        self.0.get_editor_text()
    }

    async fn editor(&self, title: &str, prefill: Option<&str>) -> Result<Option<String>> {
        Ok(self.0.editor(title.to_owned(), prefill.map(str::to_owned)).await)
    }

    fn add_autocomplete_provider(&self, factory: crate::core::extensions::types::AutocompleteProviderFactory) {
        struct AutocompleteBridge(Arc<dyn AutocompleteProvider>);

        #[async_trait]
        impl AutocompleteProvider for AutocompleteBridge {
            fn trigger_characters(&self) -> Option<Vec<String>> {
                self.0.trigger_characters()
            }

            fn set_trigger_characters(&self, trigger_characters: Option<Vec<String>>) {
                self.0.set_trigger_characters(trigger_characters);
            }

            async fn get_suggestions(
                &self,
                lines: &[String],
                cursor_line: i64,
                cursor_col: i64,
                options: pi_tui::autocomplete::GetSuggestionsOptions,
            ) -> Option<pi_tui::autocomplete::AutocompleteSuggestions> {
                self.0.get_suggestions(lines, cursor_line, cursor_col, options).await
            }

            fn apply_completion(
                &self,
                lines: &[String],
                cursor_line: i64,
                cursor_col: i64,
                item: &AutocompleteItem,
                prefix: &str,
            ) -> pi_tui::autocomplete::ApplyCompletionResult {
                self.0.apply_completion(lines, cursor_line, cursor_col, item, prefix)
            }

            fn should_trigger_file_completion(&self, lines: &[String], cursor_line: i64, cursor_col: i64) -> bool {
                self.0.should_trigger_file_completion(lines, cursor_line, cursor_col)
            }
        }

        let wrapped: AutocompleteProviderFactory = Arc::new(move |prev| {
            let next = factory(prev as Arc<dyn AutocompleteProvider>);
            Arc::new(AutocompleteBridge(next))
        });
        ExtensionUiContext::add_autocomplete_provider(&*self.0, wrapped);
    }

    fn set_editor_component(&self, factory: Option<crate::core::extensions::types::EditorFactory>) {
        let mapped = factory.map(|f| {
            Arc::new(
                move |tui: Arc<dyn TUI + Send + Sync>, editor_theme, kb: KeybindingsManager| {
                    f(tui as Arc<dyn TUI>, editor_theme, kb.to_tui_manager())
                },
            ) as EditorFactory
        });
        self.0.set_editor_component(mapped);
    }

    fn get_editor_component(&self) -> Option<crate::core::extensions::types::EditorFactory> {
        let inner = self.0.get_editor_component()?;
        Some(Arc::new(move |tui: Arc<dyn TUI>, editor_theme, tui_kb| {
            let wrapped: Arc<dyn TUI + Send + Sync> = Arc::new(SendSyncTuiWrapper(tui));
            let kb = KeybindingsManager::new(Some(tui_kb.get_user_bindings()), None);
            inner(wrapped, editor_theme, kb)
        }))
    }

    fn theme(&self) -> crate::core::extensions::types::Theme {
        crate::core::extensions::types::Theme::new(Arc::new(self.0.theme()))
    }

    fn get_all_themes(&self) -> Vec<crate::core::extensions::types::ThemeInfo> {
        self.0
            .get_all_themes()
            .into_iter()
            .map(|t| crate::core::extensions::types::ThemeInfo {
                name: t.name,
                path: t.path,
            })
            .collect()
    }

    fn get_theme(&self, name: &str) -> Option<crate::core::extensions::types::Theme> {
        self.0
            .get_theme(name)
            .map(|t| crate::core::extensions::types::Theme::new(Arc::new(t)))
    }

    fn set_theme_by_name(&self, name: &str) -> crate::core::extensions::types::SetThemeResult {
        let res = self.0.set_theme_name(name);
        crate::core::extensions::types::SetThemeResult {
            success: res.success,
            error: res.error,
        }
    }

    fn set_theme(
        &self,
        theme: crate::core::extensions::types::Theme,
    ) -> crate::core::extensions::types::SetThemeResult {
        let Some(interactive_theme) = theme.0.to_interactive_theme() else {
            return crate::core::extensions::types::SetThemeResult {
                success: false,
                error: Some("Theme object is not a TUI Theme instance".to_owned()),
            };
        };
        let res = self.0.set_theme_instance(interactive_theme);
        crate::core::extensions::types::SetThemeResult {
            success: res.success,
            error: res.error,
        }
    }

    fn get_tools_expanded(&self) -> bool {
        self.0.get_tools_expanded()
    }

    fn set_tools_expanded(&self, expanded: bool) {
        self.0.set_tools_expanded(expanded);
    }
}

#[async_trait]
impl InteractiveSession for crate::core::agent_session::AgentSession {
    fn session_manager(&self) -> Arc<dyn InteractiveSessionManager> {
        Arc::new(self.session_manager().clone())
    }

    fn settings_manager(&self) -> Arc<dyn InteractiveSettingsManager> {
        Arc::new(self.settings_manager().clone())
    }

    fn model_runtime(&self) -> Arc<dyn InteractiveModelRuntime> {
        Arc::new(Arc::new(self.model_runtime().clone()))
    }

    fn resource_loader(&self) -> Arc<dyn InteractiveResourceLoader> {
        Arc::new(InteractiveResourceLoaderAdapter(Arc::clone(self.resource_loader())))
    }

    fn extension_runner(&self) -> Arc<dyn InteractiveExtensionRunner> {
        Arc::new(InteractiveExtensionRunnerAdapter {
            runner: self.extension_runner(),
            session: self.clone(),
        })
    }

    fn as_footer_session(&self) -> Arc<dyn FooterSession> {
        Arc::new(self.clone())
    }

    fn as_share_session(&self) -> Option<Arc<dyn ShareSession>> {
        Some(Arc::new(self.clone()))
    }

    fn as_bug_report_session(&self) -> Option<Arc<dyn BugReportSession>> {
        Some(Arc::new(self.clone()))
    }

    fn model_thinking_level(&self) -> ModelThinkingLevel {
        self.thinking_level()
    }

    fn scoped_models(&self) -> Vec<ScopedModelItem> {
        self.scoped_models()
            .into_iter()
            .map(|entry| ScopedModelItem {
                model: entry.model,
                thinking_level: entry.thinking_level.map(|lvl| lvl.to_string()),
            })
            .collect()
    }

    fn set_scoped_models(&self, models: Vec<ScopedModelItem>) {
        let mapped = models
            .into_iter()
            .map(|item| crate::core::sdk::ScopedModelEntry {
                model: item.model,
                thinking_level: item
                    .thinking_level
                    .as_deref()
                    .and_then(crate::cli::args::parse_thinking_level),
            })
            .collect();
        crate::core::agent_session::AgentSession::set_scoped_models(self, mapped);
    }

    fn auto_compaction_enabled(&self) -> bool {
        crate::core::agent_session::AgentSession::auto_compaction_enabled(self)
    }

    fn set_auto_compaction_enabled(&self, enabled: bool) {
        crate::core::agent_session::AgentSession::set_auto_compaction_enabled(self, enabled);
    }

    fn steering_mode(&self) -> SteeringMode {
        match crate::core::agent_session::AgentSession::steering_mode(self) {
            pi_agent_core::QueueMode::All => SteeringMode::All,
            pi_agent_core::QueueMode::OneAtATime => SteeringMode::OneAtATime,
        }
    }

    fn set_steering_mode(&self, mode: SteeringMode) {
        let mapped = match mode {
            SteeringMode::All => pi_agent_core::QueueMode::All,
            SteeringMode::OneAtATime => pi_agent_core::QueueMode::OneAtATime,
        };
        crate::core::agent_session::AgentSession::set_steering_mode(self, mapped);
    }

    fn follow_up_mode(&self) -> SteeringMode {
        match crate::core::agent_session::AgentSession::follow_up_mode(self) {
            pi_agent_core::QueueMode::All => SteeringMode::All,
            pi_agent_core::QueueMode::OneAtATime => SteeringMode::OneAtATime,
        }
    }

    fn set_follow_up_mode(&self, mode: SteeringMode) {
        let mapped = match mode {
            SteeringMode::All => pi_agent_core::QueueMode::All,
            SteeringMode::OneAtATime => pi_agent_core::QueueMode::OneAtATime,
        };
        crate::core::agent_session::AgentSession::set_follow_up_mode(self, mapped);
    }

    fn set_transport(&self, transport: pi_ai::Transport) {
        self.settings_manager().set_transport(transport);
    }

    fn set_cache_warming_mode(&self, mode: CacheWarmingMode) {
        crate::core::agent_session::AgentSession::set_cache_warming_mode(self, mode);
    }

    fn cache_warming_status(&self) -> Option<CacheWarmingStatusSummary> {
        let status = crate::core::agent_session::AgentSession::cache_warming_status(self)?;
        let decision = status.decision;
        Some(CacheWarmingStatusSummary {
            status_text: crate::core::cache_warmer::format_cache_warming_status(&status, None),
            economics_available: decision.is_some_and(|d| d.economics_available),
            miss_cost: decision.map_or(0.0, |d| d.miss_cost),
            warm_cost: decision.map_or(0.0, |d| d.warm_cost),
        })
    }

    fn is_streaming(&self) -> bool {
        crate::core::agent_session::AgentSession::is_streaming(self)
    }

    fn is_idle(&self) -> bool {
        crate::core::agent_session::AgentSession::is_idle(self)
    }

    fn is_compacting(&self) -> bool {
        crate::core::agent_session::AgentSession::is_compacting(self)
    }

    fn is_bash_running(&self) -> bool {
        crate::core::agent_session::AgentSession::is_bash_running(self)
    }

    fn retry_attempt(&self) -> u32 {
        crate::core::agent_session::AgentSession::retry_attempt(self)
    }

    fn pending_message_count(&self) -> usize {
        crate::core::agent_session::AgentSession::pending_message_count(self)
    }

    fn get_steering_messages(&self) -> Vec<String> {
        crate::core::agent_session::AgentSession::get_steering_messages(self)
    }

    fn get_follow_up_messages(&self) -> Vec<String> {
        crate::core::agent_session::AgentSession::get_follow_up_messages(self)
    }

    fn clear_queue(&self) -> ClearQueueResult {
        let res = crate::core::agent_session::AgentSession::clear_queue(self);
        ClearQueueResult {
            steering: res.steering,
            follow_up: res.follow_up,
        }
    }

    fn messages(&self) -> Vec<AgentMessage> {
        crate::core::agent_session::AgentSession::messages(self)
    }

    fn prompt_templates(&self) -> Vec<PromptTemplateInfo> {
        crate::core::agent_session::AgentSession::prompt_templates(self)
            .into_iter()
            .map(|p| PromptTemplateInfo {
                name: p.name,
                description: if p.description.is_empty() {
                    None
                } else {
                    Some(p.description)
                },
                argument_hint: p.argument_hint,
                file_path: p.file_path,
                source_info: Some(core_source_info_to_interactive(&p.source_info)),
            })
            .collect()
    }

    fn session_file(&self) -> Option<String> {
        crate::core::agent_session::AgentSession::session_file(self)
    }

    fn signal(&self) -> Option<AbortSignal> {
        self.agent().signal()
    }

    fn get_available_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        crate::core::agent_session::AgentSession::get_available_thinking_levels(self)
    }

    fn set_thinking_level(&self, level: ModelThinkingLevel, persist: bool) -> Result<()> {
        crate::core::agent_session::AgentSession::set_thinking_level(
            self,
            level,
            Some(crate::core::agent_session::ModelMutationOptions { persist: Some(persist) }),
        );
        Ok(())
    }

    fn cycle_thinking_level(&self) -> Option<ModelThinkingLevel> {
        crate::core::agent_session::AgentSession::cycle_thinking_level(self, None)
    }

    async fn set_model(&self, model: Model, persist: bool) -> Result<()> {
        crate::core::agent_session::AgentSession::set_model(
            self,
            model,
            Some(crate::core::agent_session::ModelMutationOptions { persist: Some(persist) }),
        )
        .await
    }

    async fn cycle_model(&self, direction: CycleDirection) -> Result<Option<CycleModelResult>> {
        let dir = match direction {
            CycleDirection::Forward => crate::core::agent_session::CycleDirection::Forward,
            CycleDirection::Backward => crate::core::agent_session::CycleDirection::Backward,
        };
        let res = crate::core::agent_session::AgentSession::cycle_model(self, Some(dir), None).await?;
        Ok(res.map(|r| CycleModelResult {
            model: r.model,
            thinking_level: r.thinking_level,
        }))
    }

    async fn prompt(&self, text: &str, options: Option<InteractivePromptOptions>) -> Result<()> {
        let mapped = options.map(|o| crate::core::agent_session::PromptOptions {
            expand_prompt_templates: None,
            streaming_behavior: o.streaming_behavior.map(|b| match b {
                CompactionQueueMode::Steer => crate::core::agent_session::StreamingBehavior::Steer,
                CompactionQueueMode::FollowUp => crate::core::agent_session::StreamingBehavior::FollowUp,
            }),
            images: o.images,
            source: Some(crate::core::extensions::types::InputSource::Interactive),
            preflight_result: None,
        });
        crate::core::agent_session::AgentSession::prompt(self, text, mapped).await
    }

    async fn steer(&self, text: &str) -> Result<()> {
        let opts = crate::core::agent_session::QueueInputOptions {
            source: Some(crate::core::extensions::types::InputSource::Interactive),
        };
        crate::core::agent_session::AgentSession::steer(self, text, None, Some(opts))
            .await
            .map(|_| ())
    }

    async fn follow_up(&self, text: &str) -> Result<()> {
        let opts = crate::core::agent_session::QueueInputOptions {
            source: Some(crate::core::extensions::types::InputSource::Interactive),
        };
        crate::core::agent_session::AgentSession::follow_up(self, text, None, Some(opts))
            .await
            .map(|_| ())
    }

    async fn abort(&self) {
        crate::core::agent_session::AgentSession::abort(self).await;
    }

    async fn wait_for_idle(&self) {
        crate::core::agent_session::AgentSession::wait_for_idle(self).await;
    }

    fn abort_compaction(&self) {
        crate::core::agent_session::AgentSession::abort_compaction(self);
    }

    fn abort_branch_summary(&self) {
        crate::core::agent_session::AgentSession::abort_branch_summary(self);
    }

    fn abort_retry(&self) {
        crate::core::agent_session::AgentSession::abort_retry(self);
    }

    fn abort_bash(&self) {
        crate::core::agent_session::AgentSession::abort_bash(self);
    }

    fn subscribe(&self, listener: Arc<dyn Fn(AgentSessionEvent) -> BoxFuture<()> + Send + Sync>) -> Unsubscribe {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentSessionEvent>();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                while let Some(ev) = rx.recv().await {
                    listener(ev).await;
                }
            });
        }
        crate::core::agent_session::AgentSession::subscribe(self, move |ev| {
            let _ = tx.send(ev);
        })
    }

    async fn bind_extensions(&self, bindings: InteractiveExtensionBindings) -> Result<()> {
        let on_error = bindings.on_error;
        let wait_for_idle = bindings.wait_for_idle;
        let new_session = bindings.new_session;
        let fork = bindings.fork;
        let navigate_tree = bindings.navigate_tree;
        let switch_session = bindings.switch_session;
        let reload = bindings.reload;

        let actions = crate::core::extensions::types::ExtensionCommandContextActions {
            wait_for_idle: Arc::new(move || {
                let f = Arc::clone(&wait_for_idle);
                Box::pin(async move {
                    f().await;
                    Ok(())
                })
            }),
            new_session: Arc::new(move |_opts| {
                let f = Arc::clone(&new_session);
                Box::pin(async move {
                    let r = f().await?;
                    Ok(crate::core::extensions::types::CancelledResult { cancelled: r.cancelled })
                })
            }),
            fork: Arc::new(move |entry_id, opts| {
                let f = Arc::clone(&fork);
                let id = entry_id.to_owned();
                let mapped = opts.map(|o| RuntimeForkOptions {
                    at_leaf: o.position == Some(crate::core::extensions::types::ForkPosition::At),
                });
                Box::pin(async move {
                    let r = f(id, mapped).await?;
                    Ok(crate::core::extensions::types::CancelledResult { cancelled: r.cancelled })
                })
            }),
            navigate_tree: Arc::new(move |target_id, opts| {
                let f = Arc::clone(&navigate_tree);
                let id = target_id.to_owned();
                let mapped = opts.map_or_else(NavigateTreeOptions::default, |o| NavigateTreeOptions {
                    summarize: o.summarize.unwrap_or(false),
                    custom_instructions: o.custom_instructions,
                    replace_instructions: o.replace_instructions,
                    label: o.label,
                });
                Box::pin(async move {
                    let r = f(id, mapped).await?;
                    Ok(crate::core::extensions::types::CancelledResult { cancelled: r.cancelled })
                })
            }),
            switch_session: Arc::new(move |session_path, _opts| {
                let f = Arc::clone(&switch_session);
                let path = session_path.to_owned();
                Box::pin(async move {
                    let r = f(path).await?;
                    Ok(crate::core::extensions::types::CancelledResult { cancelled: r.cancelled })
                })
            }),
            reload: Arc::new(move || {
                let f = Arc::clone(&reload);
                Box::pin(async move { f().await })
            }),
        };

        let ext_bindings = crate::core::agent_session::ExtensionBindings {
            ui_context: Some(Arc::new(InteractiveUiContextAdapter(bindings.ui_context))),
            mode: Some(crate::core::extensions::types::ExtensionMode::Tui),
            command_context_actions: Some(actions),
            abort_handler: Some(bindings.abort_handler),
            shutdown_handler: Some(bindings.shutdown_handler),
            on_error: Some(Arc::new(move |err| {
                on_error(&err.extension_path, &err.error, err.stack.as_deref());
            })),
        };
        crate::core::agent_session::AgentSession::bind_extensions(self, ext_bindings).await
    }

    async fn reload(&self, before_session_start: Option<Arc<dyn Fn() + Send + Sync>>) -> Result<()> {
        let opts = before_session_start.map(|cb| crate::core::agent_session::ReloadOptions {
            before_session_start: Some(Box::new(move || {
                Box::pin(async move {
                    cb();
                    Ok(())
                })
            })),
        });
        crate::core::agent_session::AgentSession::reload(self, opts).await
    }

    async fn compact(&self, custom_instructions: Option<&str>) -> Result<CompactionSummaryResult> {
        let res = crate::core::agent_session::AgentSession::compact(self, custom_instructions).await?;
        Ok(CompactionSummaryResult {
            summary: res.summary,
            tokens_before: res.tokens_before,
            usage: res.usage,
        })
    }

    async fn navigate_tree(&self, target_id: &str, options: NavigateTreeOptions) -> Result<NavigateTreeResult> {
        let res = crate::core::agent_session::AgentSession::navigate_tree(
            self,
            target_id,
            Some(crate::core::agent_session::NavigateTreeOptions {
                summarize: Some(options.summarize),
                custom_instructions: options.custom_instructions,
                replace_instructions: options.replace_instructions,
                label: options.label,
            }),
        )
        .await?;
        Ok(NavigateTreeResult {
            aborted: res.aborted.unwrap_or(false),
            cancelled: res.cancelled,
            editor_text: res.editor_text,
        })
    }

    fn get_user_messages_for_forking(&self) -> Vec<UserMessageItem> {
        crate::core::agent_session::AgentSession::get_user_messages_for_forking(self)
            .into_iter()
            .map(|m| UserMessageItem {
                id: m.entry_id,
                text: m.text,
                timestamp: None,
            })
            .collect()
    }

    fn get_session_stats(&self) -> SessionStats {
        let s = crate::core::agent_session::AgentSession::get_session_stats(self);
        SessionStats {
            session_file: s.session_file,
            session_id: s.session_id,
            total_messages: s.total_messages.max(0) as usize,
            user_messages: s.user_messages.max(0) as usize,
            assistant_messages: s.assistant_messages.max(0) as usize,
            tool_calls: s.tool_calls.max(0) as usize,
            tool_results: s.tool_results.max(0) as usize,
            input_tokens: s.tokens.input,
            output_tokens: s.tokens.output,
            cache_read_tokens: s.tokens.cache_read,
            cache_write_tokens: s.tokens.cache_write,
            total_tokens: s.tokens.total,
            cost: s.cost,
        }
    }

    fn get_last_assistant_text(&self) -> Option<String> {
        crate::core::agent_session::AgentSession::get_last_assistant_text(self)
    }

    fn set_session_name(&self, name: &str) {
        crate::core::agent_session::AgentSession::set_session_name(self, name);
    }

    fn export_to_jsonl(&self, output_path: Option<&str>) -> Result<String> {
        crate::core::agent_session::AgentSession::export_to_jsonl(self, output_path)
    }

    async fn export_to_html(&self, output_path: Option<&str>, theme_name: Option<&str>) -> Result<String> {
        let opts = theme_name.map(|t| crate::core::agent_session::ExportHtmlOptions {
            theme_name: Some(t.to_owned()),
        });
        crate::core::agent_session::AgentSession::export_to_html(self, output_path, opts).await
    }

    async fn execute_bash(
        &self,
        command: &str,
        on_chunk: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
        exclude_from_context: bool,
    ) -> Result<BashExecutionResult> {
        let pending_ops = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_user_bash_operations
            .take();
        let res = crate::core::agent_session::AgentSession::execute_bash(
            self,
            command,
            Some(on_chunk),
            Some(crate::core::agent_session::ExecuteBashOptions {
                exclude_from_context: Some(exclude_from_context),
                id: None,
                operations: pending_ops,
            }),
        )
        .await?;
        Ok(BashExecutionResult {
            output: res.output,
            exit_code: res.exit_code.map(i64::from),
            cancelled: res.cancelled,
            truncated: res.truncated,
            full_output_path: res.full_output_path,
        })
    }

    fn record_bash_result(&self, command: &str, result: &BashExecutionResult, exclude_from_context: bool) {
        let core_res = crate::core::bash_executor::BashResult {
            output: result.output.clone(),
            exit_code: result.exit_code.and_then(|c| i32::try_from(c).ok()),
            cancelled: result.cancelled,
            truncated: result.truncated,
            full_output_path: result.full_output_path.clone(),
        };
        crate::core::agent_session::AgentSession::record_bash_result(
            self,
            command,
            &core_res,
            Some(crate::core::agent_session::RecordBashOptions {
                exclude_from_context: Some(exclude_from_context),
            }),
        );
    }

    fn detect_cache_miss(&self, message: &pi_ai::AssistantMessage) -> Option<CacheMissNotice> {
        crate::core::cache_stats::detect_cache_miss(
            &self.session_manager().get_entries(),
            message,
            self.model_runtime(),
        )
    }

    fn compute_cache_waste(&self) -> (i64, usize, f64) {
        let waste =
            crate::core::cache_stats::compute_cache_waste(&self.session_manager().get_entries(), self.model_runtime());
        (waste.missed_tokens, waste.miss_count, waste.missed_cost)
    }

    fn get_usage_cost_breakdown(&self) -> Vec<UsageBreakdownEntry> {
        crate::core::usage_totals::get_usage_cost_breakdown(&self.session_manager().get_entries())
            .into_iter()
            .map(|e| UsageBreakdownEntry {
                key: e.key,
                cost: e.cost,
                tokens: e.tokens,
            })
            .collect()
    }

    fn get_tool_renderers(&self, tool_name: &str) -> Option<ToolRenderers> {
        self.get_tool_definition(tool_name)
            .map(|def| ToolRenderers::from_tool_definition(&def))
    }
}

#[async_trait]
impl InteractiveRuntimeHost for crate::core::agent_session_runtime::AgentSessionRuntime {
    fn session(&self) -> Arc<dyn InteractiveSession> {
        Arc::new(crate::core::agent_session_runtime::AgentSessionRuntime::session(self))
    }

    fn agent_dir(&self) -> String {
        self.services().agent_dir
    }

    fn diagnostics(&self) -> Vec<AgentSessionRuntimeDiagnostic> {
        crate::core::agent_session_runtime::AgentSessionRuntime::diagnostics(self)
            .into_iter()
            .map(|d| AgentSessionRuntimeDiagnostic {
                diagnostic_type: match d.r#type {
                    crate::core::diagnostics::AgentSessionRuntimeDiagnosticLevel::Info => DiagnosticLevel::Info,
                    crate::core::diagnostics::AgentSessionRuntimeDiagnosticLevel::Warning => DiagnosticLevel::Warning,
                    crate::core::diagnostics::AgentSessionRuntimeDiagnosticLevel::Error => DiagnosticLevel::Error,
                },
                message: d.message,
            })
            .collect()
    }

    fn model_fallback_message(&self) -> Option<String> {
        crate::core::agent_session_runtime::AgentSessionRuntime::model_fallback_message(self)
    }

    fn set_before_session_invalidate(&self, cb: Arc<dyn Fn() + Send + Sync>) {
        crate::core::agent_session_runtime::AgentSessionRuntime::set_before_session_invalidate(self, Some(cb));
    }

    fn set_rebind_session(&self, cb: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) {
        crate::core::agent_session_runtime::AgentSessionRuntime::set_rebind_session(
            self,
            Some(Arc::new(move |_session| {
                let cb = Arc::clone(&cb);
                Box::pin(async move {
                    cb().await;
                    Ok(())
                })
            })),
        );
    }

    async fn new_session(&self) -> Result<CancelledResult> {
        let r = crate::core::agent_session_runtime::AgentSessionRuntime::new_session(self, None).await?;
        Ok(CancelledResult { cancelled: r.cancelled })
    }

    async fn fork(&self, entry_id: &str, options: Option<RuntimeForkOptions>) -> Result<RuntimeForkResult> {
        let mapped = options.map(|o| crate::core::agent_session_runtime::RuntimeForkOptions {
            position: Some(if o.at_leaf {
                crate::core::extensions::types::ForkPosition::At
            } else {
                crate::core::extensions::types::ForkPosition::Before
            }),
            with_session: None,
        });
        let r = crate::core::agent_session_runtime::AgentSessionRuntime::fork(self, entry_id, mapped).await?;
        Ok(RuntimeForkResult {
            cancelled: r.cancelled,
            selected_text: r.selected_text,
        })
    }

    async fn switch_session(
        &self,
        session_path: &str,
        options: Option<SwitchSessionOptions>,
    ) -> Result<CancelledResult> {
        let mapped = options.map(|o| crate::core::agent_session_runtime::SwitchSessionOptions {
            cwd_override: o.cwd_override,
            with_session: None,
            project_trust_context_factory: None,
        });
        let r =
            crate::core::agent_session_runtime::AgentSessionRuntime::switch_session(self, session_path, mapped).await?;
        Ok(CancelledResult { cancelled: r.cancelled })
    }

    async fn import_from_jsonl(&self, input_path: &str, cwd_override: Option<&str>) -> Result<CancelledResult> {
        let r =
            crate::core::agent_session_runtime::AgentSessionRuntime::import_from_jsonl(self, input_path, cwd_override)
                .await?;
        Ok(CancelledResult { cancelled: r.cancelled })
    }

    async fn dispose(&self) {
        crate::core::agent_session_runtime::AgentSessionRuntime::dispose(self).await;
    }
}

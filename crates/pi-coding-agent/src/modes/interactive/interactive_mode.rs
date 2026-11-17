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
    AutocompleteItem, Component, Container, EditorComponent, KeybindingsManager, ScrollViewScrollbar, Terminal,
    TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType, WheelScrollLines, fuzzy_filter,
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

// WIRE(@tui-w3): Replace local runtime/session/config/settings/extension mirror traits with concrete
// crate::{config, core::*, utils::*, extensions::mcp::config} imports once Stage 3A merges onto main.

pub const APP_NAME_LOCAL: &str = "pi";
pub const APP_TITLE_LOCAL: &str = "π";
pub const CONFIG_DIR_NAME_LOCAL: &str = ".pi";
pub const VERSION_LOCAL: &str = env!("CARGO_PKG_VERSION");

pub fn get_agent_dir_local() -> String {
    if let Some(dir) = pi_js::env::var("PI_CODING_AGENT_DIR").filter(|s| !s.is_empty()) {
        return dir;
    }
    pi_js::path::join(&[&pi_js::env::home_dir(), CONFIG_DIR_NAME_LOCAL, "agent"])
}

pub fn get_auth_path_local() -> String {
    pi_js::path::join(&[&get_agent_dir_local(), "auth.json"])
}

pub fn get_debug_log_path_local() -> String {
    pi_js::path::join(&[&get_agent_dir_local(), "debug.log"])
}

pub fn get_docs_path_local() -> String {
    pi_js::path::join(&[&get_agent_dir_local(), "docs"])
}

pub fn get_changelog_path_local() -> String {
    pi_js::path::join(&[&get_agent_dir_local(), "CHANGELOG.md"])
}

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
                    app: Some(APP_NAME_LOCAL.to_string()),
                    kind: Some(blocked.kind),
                    message: Some(blocked.message.clone()),
                }
            } else if st.compacting {
                ProgramStatus {
                    state: ProgramStatusState::Working,
                    app: Some(APP_NAME_LOCAL.to_string()),
                    kind: None,
                    message: Some("Compacting context".to_string()),
                }
            } else if st.run_active {
                ProgramStatus {
                    state: ProgramStatusState::Working,
                    app: Some(APP_NAME_LOCAL.to_string()),
                    kind: None,
                    message: session_name,
                }
            } else {
                let mut resting = st.resting_status.clone();
                resting.app = Some(APP_NAME_LOCAL.to_string());
                if resting.state == ProgramStatusState::Done {
                    resting.message = session_name;
                }
                resting
            };
            if status.app.is_none() {
                status.app = Some(APP_NAME_LOCAL.to_string());
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

#[derive(Clone, Debug)]
pub struct CacheMissNotice {
    pub missed_tokens: i64,
    pub missed_cost: f64,
    pub model_changed: bool,
    pub idle_ms: i64,
}

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
    fn on_branch_change(&self, cb: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe;
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
        on_chunk: Arc<dyn Fn(&str) + Send + Sync>,
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

/// Optional hooks allowing offline unit tests to override exit/signal behavior cleanly.
#[derive(Clone)]
pub struct InteractiveModeHooks {
    pub exit_process: Arc<dyn Fn(i32) + Send + Sync>,
    pub create_footer_data_provider: Arc<dyn Fn(&str) -> Arc<dyn InteractiveFooterDataProvider> + Send + Sync>,
    pub keybindings: Option<Arc<KeybindingsManager>>,
    pub keybindings_reload: Arc<dyn Fn(&KeybindingsManager) + Send + Sync>,
    pub clipboard_copy: Arc<dyn Fn(&str) -> BoxFuture<Result<()>> + Send + Sync>,
    pub clipboard_read_text: Arc<dyn Fn() -> BoxFuture<Result<Option<String>>> + Send + Sync>,
    pub platform_is_windows: Option<bool>,
    pub suspend_process:
        Option<Arc<dyn Fn(Arc<dyn Fn() + Send + Sync>) -> std::result::Result<(), String> + Send + Sync>>,
    pub install_change_warning: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl Default for InteractiveModeHooks {
    fn default() -> Self {
        Self {
            exit_process: Arc::new(|code| std::process::exit(code)),
            create_footer_data_provider: Arc::new(|cwd| Arc::new(DefaultFooterDataProvider::new(cwd))),
            keybindings: None,
            keybindings_reload: Arc::new(|_| {}),
            clipboard_copy: Arc::new(|_| Box::pin(async { Ok(()) })),
            clipboard_read_text: Arc::new(|| Box::pin(async { Ok(None) })),
            platform_is_windows: None,
            suspend_process: None,
            install_change_warning: None,
        }
    }
}

struct DefaultFooterDataProvider {
    cwd: Mutex<String>,
    extension_statuses: Mutex<IndexMap<String, String>>,
    available_provider_count: Mutex<usize>,
    branch_listeners: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

impl DefaultFooterDataProvider {
    fn new(cwd: &str) -> Self {
        Self {
            cwd: Mutex::new(cwd.to_string()),
            extension_statuses: Mutex::new(IndexMap::new()),
            available_provider_count: Mutex::new(0),
            branch_listeners: Mutex::new(Vec::new()),
        }
    }
}

impl ReadonlyFooterDataProvider for DefaultFooterDataProvider {
    fn get_git_branch(&self) -> Option<String> {
        let _cwd = self.cwd.lock().unwrap_or_else(|e| e.into_inner()).clone();
        None
    }

    fn get_extension_statuses(&self) -> IndexMap<String, String> {
        self.extension_statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn get_available_provider_count(&self) -> usize {
        *self.available_provider_count.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl InteractiveFooterDataProvider for DefaultFooterDataProvider {
    fn set_cwd(&self, cwd: &str) {
        *self.cwd.lock().unwrap_or_else(|e| e.into_inner()) = cwd.to_string();
    }

    fn set_extension_status(&self, key: &str, text: Option<&str>) {
        let mut map = self.extension_statuses.lock().unwrap_or_else(|e| e.into_inner());
        match text {
            Some(t) => {
                map.insert(key.to_string(), t.to_string());
            }
            None => {
                map.shift_remove(key);
            }
        }
    }

    fn clear_extension_statuses(&self) {
        self.extension_statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    fn set_available_provider_count(&self, count: usize) {
        *self.available_provider_count.lock().unwrap_or_else(|e| e.into_inner()) = count;
    }

    fn on_branch_change(&self, cb: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        self.branch_listeners.lock().unwrap_or_else(|e| e.into_inner()).push(cb);
        Unsubscribe::noop()
    }

    fn dispose(&self) {
        self.branch_listeners.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
        "A stack frame came from loaded {noun} {labels}, which may be involved. Try disabling {pronoun} with `{APP_NAME_LOCAL} config`, or run `{APP_NAME_LOCAL} -ne` to confirm."
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
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        return None;
    }
    if !session_manager.is_persisted() {
        return None;
    }
    let session_file = session_manager.get_session_file()?;
    if !pi_js::fs::exists(&session_file) {
        return None;
    }
    let mut args = vec![APP_NAME_LOCAL.to_string()];
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
    #[allow(dead_code)] // WIRE(@tui-w3): used by show_managed_tool_status once ensure_tool lands on main.
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
}

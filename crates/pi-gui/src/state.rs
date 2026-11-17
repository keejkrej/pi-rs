//! Shared GUI state, actions, launch configuration, and view models for `pi-gui`.

use indexmap::IndexMap;
use rune::TextEdit;
use rune::el::Tx;
use rune_kit::{FileNode, FileTree, PaneState, TreeModel};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bridge::{BridgeEvent, BridgeSender};
use crate::theme::{LayoutMetrics, ThemePreference};

#[derive(Debug, Error)]
pub enum PiGuiError {
    #[error("bridge unavailable: {0}")]
    BridgeUnavailable(String),
    #[error("launch error: {0}")]
    Launch(String),
}

/// CLI / launch options passed from `pi --gui`, `pi gui`, or direct `pi-gui` invocation.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LaunchConfig {
    pub cwd: String,
    pub session: Option<String>,
    pub session_dir: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub theme: Option<ThemePreference>,
    pub scene: Option<String>,
    pub demo: bool,
    pub reduced_motion: bool,
    pub extra_args: Vec<String>,
}

impl LaunchConfig {
    #[must_use]
    pub fn from_rune_args() -> Self {
        Self::parse_args(rune::extra_args(), &pi_js::env::cwd())
    }

    #[must_use]
    pub fn parse_args<S: AsRef<str>>(args: &[S], default_cwd: &str) -> Self {
        let mut cfg = Self {
            cwd: default_cwd.to_string(),
            ..Self::default()
        };
        let mut i = 0usize;
        while i < args.len() {
            let arg = args[i].as_ref();
            match arg {
                "--demo" => {
                    cfg.demo = true;
                    i += 1;
                    continue;
                }
                "--reduced-motion" => {
                    cfg.reduced_motion = true;
                    i += 1;
                    continue;
                }
                _ => {}
            }
            if let Some((flag, val)) = arg.split_once('=')
                && cfg.apply_kv(flag, val)
            {
                i += 1;
                continue;
            }
            if let Some(val) = args.get(i + 1).map(AsRef::as_ref)
                && cfg.apply_kv(arg, val)
            {
                i += 2;
                continue;
            }
            cfg.extra_args.push(arg.to_string());
            i += 1;
        }
        cfg
    }

    fn apply_kv(&mut self, flag: &str, val: &str) -> bool {
        match flag {
            "--cwd" => self.cwd = val.to_string(),
            "--session" => self.session = Some(val.to_string()),
            "--session-dir" => self.session_dir = Some(val.to_string()),
            "--provider" => self.provider = Some(val.to_string()),
            "--model" => self.model = Some(val.to_string()),
            "--thinking" => self.thinking = Some(val.to_string()),
            "--theme" => self.theme = parse_theme_arg(val),
            "--scene" => self.scene = Some(val.to_string()),
            _ => return false,
        }
        true
    }
}

fn parse_theme_arg(raw: &str) -> Option<ThemePreference> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "dark" => Some(ThemePreference::Dark),
        "light" => Some(ThemePreference::Light),
        "system" | "auto" => Some(ThemePreference::System),
        _ => None,
    }
}

/// Primary sidebar navigation tabs.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum NavRailTab {
    #[default]
    Sessions,
    Files,
    Extensions,
    Settings,
}

impl NavRailTab {
    pub const ALL: [Self; 4] = [Self::Sessions, Self::Files, Self::Extensions, Self::Settings];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sessions => "Threads",
            Self::Files => "Files",
            Self::Extensions => "Plugins",
            Self::Settings => "Settings",
        }
    }
}

/// Right-hand inspector tabs modeled after Waku's workspace surfaces.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum InspectorTab {
    #[default]
    Diff,
    Files,
    File,
    Terminal,
    Tools,
    Plugins,
}

impl InspectorTab {
    pub const ALL: [Self; 6] = [
        Self::Diff,
        Self::Files,
        Self::File,
        Self::Terminal,
        Self::Tools,
        Self::Plugins,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Diff => "Changes",
            Self::Files => "Files",
            Self::File => "Editor",
            Self::Terminal => "Terminal",
            Self::Tools => "Tools",
            Self::Plugins => "Plugins",
        }
    }
}

/// Modal or popover overlays mounted in [`crate::overlays::view`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OverlayKind {
    CommandPalette,
    ModelPicker,
    SessionPicker,
    BranchPicker,
    FilePicker,
    Settings,
    Auth,
    ExtensionDialog(ExtensionDialogView),
    Confirm {
        title: String,
        message: String,
        action_id: String,
    },
    ImagePreview {
        path: String,
        title: String,
    },
}

/// Named deterministic scenes for headless snapshot/script testing (`--scene <id>`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DemoScene {
    Default,
    Empty,
    Streaming,
    DiffReview,
    Overlays,
    Settings,
    Plugins,
    LightTheme,
}

impl DemoScene {
    pub const ALL: [Self; 8] = [
        Self::Default,
        Self::Empty,
        Self::Streaming,
        Self::DiffReview,
        Self::Overlays,
        Self::Settings,
        Self::Plugins,
        Self::LightTheme,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Empty => "empty",
            Self::Streaming => "streaming",
            Self::DiffReview => "diff-review",
            Self::Overlays => "overlays",
            Self::Settings => "settings",
            Self::Plugins => "plugins",
            Self::LightTheme => "light-theme",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "dark-session" => Some(Self::Default),
            "light-session" => Some(Self::LightTheme),
            "diff-inspector" => Some(Self::DiffReview),
            "command-palette" => Some(Self::Overlays),
            "settings-plugins" => Some(Self::Settings),
            _ => Self::ALL.into_iter().find(|scene| scene.id() == id),
        }
    }

    pub fn apply(self, state: &mut PiGuiState) {
        state.demo_scene = Some(self);
        if self != Self::Empty {
            let bundle = crate::models::build_demo_bundle(&state.config.cwd);
            state.threads = bundle.threads;
            state.active_thread_id = bundle.active_thread_id;
            state.messages = bundle.messages;
            state.diff = bundle.diff;
            state.apply_files_snapshot(bundle.files);
            state.open_file = bundle.open_file;
            state.terminal = bundle.terminal;
            state.plugins = bundle.plugins;
            state.auth_providers = bundle.auth_providers;
            state.models = bundle.models;
            state.active_model = bundle.active_model;
            state.context_usage = bundle.context_usage;
        }
        match self {
            Self::Default => {
                state.nav_tab = NavRailTab::Sessions;
                state.theme_preference = ThemePreference::Dark;
                state.overlay = None;
            }
            Self::Empty => {
                state.threads.clear();
                state.active_thread_id = None;
                state.messages.clear();
                state.active_stream = None;
                state.overlay = None;
            }
            Self::Streaming => {
                state.active_stream = Some(ActiveStreamState {
                    turn_id: "turn-demo".to_string(),
                    partial_text: "Streaming tool result and markdown synthesis...".to_string(),
                    partial_thinking: Some("Checking workspace crates and Rune tokens".to_string()),
                    started_ms: 0,
                });
            }
            Self::DiffReview => {
                state.inspector_tab = InspectorTab::Diff;
                state.inspector_pane.set_open(true);
            }
            Self::Overlays => state.overlay = Some(OverlayKind::CommandPalette),
            Self::Settings => {
                state.nav_tab = NavRailTab::Settings;
                state.overlay = Some(OverlayKind::Settings);
            }
            Self::Plugins => {
                state.nav_tab = NavRailTab::Extensions;
                state.inspector_tab = InspectorTab::Plugins;
                state.inspector_pane.set_open(true);
            }
            Self::LightTheme => state.theme_preference = ThemePreference::Light,
        }
    }
}

/// Complete scene table supporting legacy names and Wave 4 `--scene <id>` aliases.
pub static SCENE_ENTRIES: &[(&str, fn(&mut PiGuiState))] = &[
    (DemoScene::Default.id(), |s| DemoScene::Default.apply(s)),
    (DemoScene::Empty.id(), |s| DemoScene::Empty.apply(s)),
    (DemoScene::Streaming.id(), |s| DemoScene::Streaming.apply(s)),
    (DemoScene::DiffReview.id(), |s| DemoScene::DiffReview.apply(s)),
    (DemoScene::Overlays.id(), |s| DemoScene::Overlays.apply(s)),
    (DemoScene::Settings.id(), |s| DemoScene::Settings.apply(s)),
    (DemoScene::Plugins.id(), |s| DemoScene::Plugins.apply(s)),
    (DemoScene::LightTheme.id(), |s| DemoScene::LightTheme.apply(s)),
    ("dark-session", |s| DemoScene::Default.apply(s)),
    ("light-session", |s| DemoScene::LightTheme.apply(s)),
    ("diff-inspector", |s| DemoScene::DiffReview.apply(s)),
    ("command-palette", |s| DemoScene::Overlays.apply(s)),
    ("settings-plugins", |s| DemoScene::Settings.apply(s)),
];

pub use crate::models::*;

/// Pure UI actions dispatched from Rune event handlers or key bindings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PiGuiAction {
    SelectNavTab(NavRailTab),
    SelectInspectorTab(InspectorTab),
    ToggleSidebar,
    ToggleInspector,
    OpenOverlay(OverlayKind),
    CloseOverlay,
    SetThemePreference(ThemePreference),
    SelectThread(String),
    NewThread,
    DeleteThread(String),
    RenameThread { id: String, title: String },
    SubmitPrompt,
    AbortRun,
    QueueSteering { text: String, follow_up: bool },
    SelectModel { provider: String, model_id: String },
    CycleThinkingLevel,
    ToggleToolExpanded(String),
    OpenFile(String),
    SelectDiffFile(String),
    TogglePlugin { id: String, enabled: bool },
    ReloadPlugins,
    LoginProvider(String),
    LogoutProvider(String),
    RunTerminalCommand(String),
    ClearTerminal,
    RespondExtensionDialog { request_id: String, value: Option<String> },
    DismissToast(u64),
    LoadDemoScene(DemoScene),
    SetStatusBanner(Option<String>),
    CompactSession,
}

/// Root UI state owned by Rune's window runtime.
pub struct PiGuiState {
    pub config: LaunchConfig,
    pub theme_preference: ThemePreference,
    pub nav_tab: NavRailTab,
    pub inspector_tab: InspectorTab,
    pub overlay: Option<OverlayKind>,
    pub sidebar_pane: PaneState,
    pub inspector_pane: PaneState,

    pub threads: Vec<ThreadSummary>,
    pub active_thread_id: Option<String>,
    pub messages: Vec<MessageViewModel>,
    pub active_stream: Option<ActiveStreamState>,
    pub steering_queue: Vec<SteeringQueueItem>,

    pub composer_edit: TextEdit,
    pub search_edit: TextEdit,
    pub palette_edit: TextEdit,
    pub terminal_edit: TextEdit,
    pub file_tree: TreeModel<FileNode<()>>,
    pub files: Vec<FileTreeEntryView>,
    pub diff: DiffSummary,
    pub open_file: Option<OpenFileView>,
    pub terminal: TerminalSessionView,

    pub plugins: Vec<PluginViewModel>,
    pub auth_providers: Vec<AuthProviderViewModel>,
    pub models: Vec<ModelViewModel>,
    pub active_model: Option<ModelViewModel>,
    pub context_usage: ContextUsageView,
    pub settings_flags: IndexMap<String, bool>,

    pub toasts: Vec<ToastMessage>,
    pub next_toast_id: u64,
    pub status_banner: Option<String>,
    pub reduced_motion: bool,
    pub demo_scene: Option<DemoScene>,
    pub bridge: Option<BridgeSender>,
    pub pending_events: Vec<BridgeEvent>,
}

impl PiGuiState {
    #[must_use]
    pub fn new(config: LaunchConfig) -> Self {
        let reduced_motion = config.reduced_motion;
        let theme_preference = config.theme.unwrap_or_default();
        let cwd = config.cwd.clone();
        let file_tree = FileTree::new()
            .compact_folders(false)
            .build(std::iter::empty::<(&str, ())>());
        let mut state = Self {
            config,
            theme_preference,
            nav_tab: NavRailTab::Sessions,
            inspector_tab: InspectorTab::Diff,
            overlay: None,
            sidebar_pane: PaneState::new(LayoutMetrics::DEFAULT_SIDEBAR_WIDTH)
                .range(LayoutMetrics::SIDEBAR_MIN_WIDTH, LayoutMetrics::SIDEBAR_MAX_WIDTH),
            inspector_pane: PaneState::new(LayoutMetrics::DEFAULT_RIGHT_PANEL_WIDTH).range(
                LayoutMetrics::RIGHT_PANEL_MIN_WIDTH,
                LayoutMetrics::RIGHT_PANEL_MAX_WIDTH,
            ),
            threads: Vec::new(),
            active_thread_id: None,
            messages: Vec::new(),
            active_stream: None,
            steering_queue: Vec::new(),
            composer_edit: TextEdit::default(),
            search_edit: TextEdit::default(),
            palette_edit: TextEdit::default(),
            terminal_edit: TextEdit::default(),
            file_tree,
            files: Vec::new(),
            diff: DiffSummary::default(),
            open_file: None,
            terminal: TerminalSessionView {
                cwd,
                running: false,
                last_exit_code: None,
                lines: Vec::new(),
            },
            plugins: Vec::new(),
            auth_providers: Vec::new(),
            models: Vec::new(),
            active_model: None,
            context_usage: ContextUsageView {
                thinking_level: "medium".to_string(),
                ..ContextUsageView::default()
            },
            settings_flags: IndexMap::new(),
            toasts: Vec::new(),
            next_toast_id: 1,
            status_banner: None,
            reduced_motion,
            demo_scene: None,
            bridge: None,
            pending_events: Vec::new(),
        };
        if let Some(scene) = state.config.scene.as_deref().and_then(DemoScene::from_id) {
            scene.apply(&mut state);
        } else if state.config.demo {
            DemoScene::Default.apply(&mut state);
        }
        state
    }

    pub fn apply_files_snapshot(&mut self, files: Vec<FileTreeEntryView>) {
        let paths: Vec<(&str, ())> = files
            .iter()
            .filter(|f| !f.is_dir)
            .map(|f| (f.path.as_str(), ()))
            .collect();
        self.file_tree = FileTree::new().compact_folders(false).build(paths);
        self.files = files;
    }

    pub fn push_toast(&mut self, message: impl Into<String>, tone: ToastTone) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toasts.push(ToastMessage {
            id,
            message: message.into(),
            tone,
        });
    }

    fn send_cmd(&self, cmd: crate::bridge::BridgeCommand) {
        if let Some(bridge) = &self.bridge {
            bridge.send(cmd);
        }
    }

    pub fn dispatch(&mut self, action: PiGuiAction, tx: Option<&mut Tx>) {
        use crate::bridge::BridgeCommand;
        match action {
            PiGuiAction::SelectNavTab(tab) => {
                self.nav_tab = tab;
                if tab == NavRailTab::Settings {
                    self.overlay = Some(OverlayKind::Settings);
                }
            }
            PiGuiAction::SelectInspectorTab(tab) => {
                self.inspector_tab = tab;
                self.inspector_pane.set_open(true);
            }
            PiGuiAction::ToggleSidebar => {
                let open = self.sidebar_pane.is_open();
                self.sidebar_pane.set_open(!open);
            }
            PiGuiAction::ToggleInspector => {
                let open = self.inspector_pane.is_open();
                self.inspector_pane.set_open(!open);
            }
            PiGuiAction::OpenOverlay(kind) => self.overlay = Some(kind),
            PiGuiAction::CloseOverlay => self.overlay = None,
            PiGuiAction::SetThemePreference(pref) => self.theme_preference = pref,
            PiGuiAction::SelectThread(id) => {
                self.active_thread_id = Some(id.clone());
                self.send_cmd(BridgeCommand::SwitchSession { id });
            }
            PiGuiAction::NewThread => self.send_cmd(BridgeCommand::NewSession),
            PiGuiAction::DeleteThread(id) => {
                self.threads.retain(|t| t.id != id);
                if self.active_thread_id.as_deref() == Some(id.as_str()) {
                    self.active_thread_id = self.threads.first().map(|t| t.id.clone());
                }
            }
            PiGuiAction::RenameThread { id, title } => {
                if let Some(thread) = self.threads.iter_mut().find(|t| t.id == id) {
                    thread.title = title;
                }
            }
            PiGuiAction::SubmitPrompt => {
                let text = self.composer_edit.text.trim().to_string();
                if !text.is_empty() {
                    self.send_cmd(BridgeCommand::Prompt { text });
                    self.composer_edit.clear();
                }
            }
            PiGuiAction::AbortRun => self.send_cmd(BridgeCommand::Abort),
            PiGuiAction::QueueSteering { text, follow_up } => {
                let id = format!("steer-{}", self.steering_queue.len() + 1);
                self.steering_queue.push(SteeringQueueItem {
                    id,
                    text: text.clone(),
                    is_follow_up: follow_up,
                });
                self.send_cmd(BridgeCommand::Prompt { text });
            }
            PiGuiAction::SelectModel { provider, model_id } => {
                if let Some(found) = self
                    .models
                    .iter()
                    .find(|m| m.provider == provider && m.id == model_id)
                    .cloned()
                {
                    self.active_model = Some(found);
                }
                self.send_cmd(BridgeCommand::SelectModel { provider, model_id });
                self.overlay = None;
            }
            PiGuiAction::CycleThinkingLevel => {
                let next = match self.context_usage.thinking_level.as_str() {
                    "off" => "minimal",
                    "minimal" => "low",
                    "low" => "medium",
                    "medium" => "high",
                    "high" => "xhigh",
                    _ => "off",
                };
                self.context_usage.thinking_level = next.to_string();
                self.send_cmd(BridgeCommand::SetThinkingLevel {
                    level: next.to_string(),
                });
            }
            PiGuiAction::ToggleToolExpanded(tool_id) => {
                for msg in &mut self.messages {
                    for block in &mut msg.blocks {
                        if let ContentBlockView::ToolCall(tc) = block
                            && tc.id == tool_id
                        {
                            tc.expanded = !tc.expanded;
                        }
                    }
                }
            }
            PiGuiAction::OpenFile(path) => {
                self.inspector_tab = InspectorTab::File;
                self.inspector_pane.set_open(true);
                self.send_cmd(BridgeCommand::ReadFile { path });
            }
            PiGuiAction::SelectDiffFile(path) => {
                self.diff.selected_path = Some(path);
                self.inspector_tab = InspectorTab::Diff;
                self.inspector_pane.set_open(true);
            }
            PiGuiAction::TogglePlugin { id, enabled } => {
                if let Some(plugin) = self.plugins.iter_mut().find(|p| p.id == id) {
                    plugin.enabled = enabled;
                }
            }
            PiGuiAction::ReloadPlugins => self.send_cmd(BridgeCommand::ReloadPlugins),
            PiGuiAction::CompactSession => self.send_cmd(BridgeCommand::CompactSession),
            PiGuiAction::LoginProvider(provider) => self.send_cmd(BridgeCommand::LoginProvider { provider }),
            PiGuiAction::LogoutProvider(provider) => self.send_cmd(BridgeCommand::LogoutProvider { provider }),
            PiGuiAction::RunTerminalCommand(command) => {
                self.terminal.running = true;
                self.send_cmd(BridgeCommand::RunShellCommand { command });
            }
            PiGuiAction::ClearTerminal => self.terminal.lines.clear(),
            PiGuiAction::RespondExtensionDialog { request_id, value } => {
                self.send_cmd(BridgeCommand::ExtensionDialogResponse { request_id, value });
                self.overlay = None;
            }
            PiGuiAction::DismissToast(id) => self.toasts.retain(|t| t.id != id),
            PiGuiAction::LoadDemoScene(scene) => {
                scene.apply(self);
                if let Some(tx) = tx {
                    tx.settle();
                }
            }
            PiGuiAction::SetStatusBanner(banner) => self.status_banner = banner,
        }
    }

    /// Applies an event sent from the background `pi-coding-agent` bridge. Returns `true` if visible state changed.
    pub fn apply_bridge_event(&mut self, event: BridgeEvent) -> bool {
        match event {
            BridgeEvent::SessionLoaded {
                session_id,
                messages,
                models,
                active_model,
                plugins,
                auth_providers,
                context_usage,
            } => {
                self.active_thread_id = Some(session_id);
                self.messages = messages;
                self.models = models;
                self.active_model = active_model;
                self.plugins = plugins;
                self.auth_providers = auth_providers;
                self.context_usage = context_usage;
                self.active_stream = None;
            }
            BridgeEvent::ThreadsUpdated(threads) => self.threads = threads,
            BridgeEvent::StreamStarted { turn_id, started_ms } => {
                self.active_stream = Some(ActiveStreamState {
                    turn_id,
                    partial_text: String::new(),
                    partial_thinking: None,
                    started_ms,
                });
            }
            BridgeEvent::StreamTextDelta(delta) => match &mut self.active_stream {
                Some(stream) => stream.partial_text.push_str(&delta),
                None => return false,
            },
            BridgeEvent::StreamThinkingDelta(delta) => match &mut self.active_stream {
                Some(stream) => stream.partial_thinking.get_or_insert_with(String::new).push_str(&delta),
                None => return false,
            },
            BridgeEvent::MessageAppended(msg) => self.messages.push(msg),
            BridgeEvent::StreamFinished => self.active_stream = None,
            BridgeEvent::DiffUpdated(diff) => self.diff = diff,
            BridgeEvent::FilesUpdated(files) => self.apply_files_snapshot(files),
            BridgeEvent::PluginsUpdated(plugins) => self.plugins = plugins,
            BridgeEvent::ModelsUpdated { models, active_model } => {
                self.models = models;
                self.active_model = active_model;
            }
            BridgeEvent::ContextUpdated(usage) => self.context_usage = usage,
            BridgeEvent::MessagesReplaced(messages) => self.messages = messages,
            BridgeEvent::FileOpened(file) => self.open_file = Some(file),
            BridgeEvent::TerminalOutput(line) => self.terminal.lines.push(line),
            BridgeEvent::TerminalFinished { exit_code } => {
                self.terminal.running = false;
                self.terminal.last_exit_code = exit_code;
            }
            BridgeEvent::ExtensionDialogRequested(dialog) => {
                self.overlay = Some(OverlayKind::ExtensionDialog(dialog));
            }
            BridgeEvent::Toast { message, tone } => self.push_toast(message, tone),
            BridgeEvent::Error(err) => {
                self.status_banner = Some(err.clone());
                self.push_toast(err, ToastTone::Error);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_config_parses_cli_flags() {
        let cfg = LaunchConfig::parse_args(
            &[
                "--cwd",
                "/tmp/repo",
                "--session=sess-1",
                "--provider",
                "anthropic",
                "--model=claude-sonnet-4",
                "--thinking",
                "high",
                "--theme=light",
                "--demo",
                "--reduced-motion",
            ],
            "/fallback",
        );
        assert_eq!(cfg.cwd, "/tmp/repo");
        assert_eq!(cfg.session.as_deref(), Some("sess-1"));
        assert_eq!(cfg.provider.as_deref(), Some("anthropic"));
        assert_eq!(cfg.model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(cfg.thinking.as_deref(), Some("high"));
        assert_eq!(cfg.theme, Some(ThemePreference::Light));
        assert!(cfg.demo);
        assert!(cfg.reduced_motion);
        assert_eq!(DemoScene::from_id("dark-session"), Some(DemoScene::Default));
        assert_eq!(DemoScene::from_id("diff-inspector"), Some(DemoScene::DiffReview));
        assert!(SCENE_ENTRIES.iter().any(|(id, _)| *id == "settings-plugins"));
    }

    #[test]
    fn dispatch_cycles_thinking_and_applies_demo_scenes() {
        let mut st = PiGuiState::new(LaunchConfig::parse_args(&["--demo"], "/repo"));
        assert_eq!(st.demo_scene, Some(DemoScene::Default));
        assert_eq!(st.context_usage.thinking_level, "medium");

        st.dispatch(PiGuiAction::CycleThinkingLevel, None);
        assert_eq!(st.context_usage.thinking_level, "high");

        st.dispatch(PiGuiAction::LoadDemoScene(DemoScene::LightTheme), None);
        assert_eq!(st.theme_preference, ThemePreference::Light);

        st.composer_edit.replace_text("hello world");
        st.dispatch(PiGuiAction::SubmitPrompt, None);
        assert!(st.composer_edit.text.is_empty());
    }
}

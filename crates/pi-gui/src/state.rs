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

/// Settings navigation categories rendered in the 2-pane `Settings` view.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum SettingsPage {
    #[default]
    General,
    Appearance,
    Providers,
    Skills,
    Usage,
}

impl SettingsPage {
    pub const ALL: [Self; 5] = [
        Self::General,
        Self::Appearance,
        Self::Providers,
        Self::Skills,
        Self::Usage,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Providers => "providers",
            Self::Skills => "skills",
            Self::Usage => "usage",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Providers => "Providers",
            Self::Skills => "Skills",
            Self::Usage => "Usage",
        }
    }
}

/// Sidebar grouping mode selectable via `Sidebar options`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum SidebarGrouping {
    #[default]
    Updated,
    Project,
}

impl SidebarGrouping {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Updated => "Updated",
            Self::Project => "Project",
        }
    }
}

/// Sidebar chronological ordering selectable via `Sidebar options`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum SidebarOrdering {
    #[default]
    Newest,
    Oldest,
}

impl SidebarOrdering {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Newest => "Newest",
            Self::Oldest => "Oldest",
        }
    }
}

/// Selected category in the anchored Model Picker left rail.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum ModelPickerTab {
    All,
    Favorites,
    Provider(String),
}

impl Default for ModelPickerTab {
    fn default() -> Self {
        Self::All
    }
}

/// Workspace execution mode in the footer workspace picker.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum WorkspaceMode {
    #[default]
    Local,
    NewWorktree,
}

/// Active diff source displayed in the right-panel Changes toolbar.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum DiffSource {
    #[default]
    Uncommitted,
    Unstaged,
    Staged,
    Committed,
}

impl DiffSource {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Uncommitted => "Uncommitted",
            Self::Unstaged => "Unstaged",
            Self::Staged => "Staged",
            Self::Committed => "Committed",
        }
    }

    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Uncommitted => Self::Unstaged,
            Self::Unstaged => Self::Staged,
            Self::Staged => Self::Committed,
            Self::Committed => Self::Uncommitted,
        }
    }
}

/// Right-hand inspector tabs modeled after Waku's right panel surfaces + Pi's approved `Tree` view.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum InspectorTab {
    #[default]
    Changes,
    Files,
    Terminal,
    Tree,
}

impl InspectorTab {
    pub const ALL: [Self; 4] = [Self::Changes, Self::Files, Self::Terminal, Self::Tree];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Files => "files",
            Self::Terminal => "terminal",
            Self::Tree => "tree",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Changes => "Changes",
            Self::Files => "Files",
            Self::Terminal => "Terminal",
            Self::Tree => "Tree",
        }
    }
}

/// Anchored popovers pinned relative to sidebar header, composer chips, or workspace footer chips.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum PopoverKind {
    SidebarOptions,
    ModelPicker,
    ThinkingPicker,
    ProjectPicker,
    WorkspacePicker,
    BranchPicker,
    ContextUsage,
}

/// Centered modal overlays mounted in [`crate::overlays::view`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OverlayKind {
    CommandPalette,
    CommitDialog,
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

/// Safe virtual workspace path used by deterministic offline `--demo` / `--scene` snapshots.
pub const DEMO_WORKSPACE_CWD: &str = "~/workspace/pi-rs";

/// Named deterministic scenes for headless snapshot/script testing (`--scene <id>`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DemoScene {
    Default,
    Empty,
    LightTheme,
    SidebarCollapsed,
    RightPanelOpen,
    BusyQueued,
    Toast,
    Overflow,
    PopoverSidebarOptions,
    PopoverModelPicker,
    PopoverModelPickerFavorites,
    PopoverModelPickerSearchCleared,
    PopoverThinkingPicker,
    PopoverProjectPicker,
    PopoverWorkspacePicker,
    PopoverBranchPicker,
    PopoverBranchPickerCreate,
    PopoverContextUsage,
    RightPanelChanges,
    RightPanelFiles,
    RightPanelTerminal,
    RightPanelTree,
    CommandPalette,
    CommandPaletteFiltered,
    CommandPaletteEmpty,
    DialogCommit,
    SettingsGeneral,
    SettingsAppearance,
    SettingsProviders,
    SettingsSkills,
    SettingsUsage,
}

impl DemoScene {
    pub const ALL: [Self; 31] = [
        Self::Default,
        Self::Empty,
        Self::LightTheme,
        Self::SidebarCollapsed,
        Self::RightPanelOpen,
        Self::BusyQueued,
        Self::Toast,
        Self::Overflow,
        Self::PopoverSidebarOptions,
        Self::PopoverModelPicker,
        Self::PopoverModelPickerFavorites,
        Self::PopoverModelPickerSearchCleared,
        Self::PopoverThinkingPicker,
        Self::PopoverProjectPicker,
        Self::PopoverWorkspacePicker,
        Self::PopoverBranchPicker,
        Self::PopoverBranchPickerCreate,
        Self::PopoverContextUsage,
        Self::RightPanelChanges,
        Self::RightPanelFiles,
        Self::RightPanelTerminal,
        Self::RightPanelTree,
        Self::CommandPalette,
        Self::CommandPaletteFiltered,
        Self::CommandPaletteEmpty,
        Self::DialogCommit,
        Self::SettingsGeneral,
        Self::SettingsAppearance,
        Self::SettingsProviders,
        Self::SettingsSkills,
        Self::SettingsUsage,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Default => "workspace-populated",
            Self::Empty => "workspace-empty",
            Self::LightTheme => "workspace-light",
            Self::SidebarCollapsed => "workspace-sidebar-collapsed",
            Self::RightPanelOpen => "workspace-right-panel-open",
            Self::BusyQueued => "workspace-busy-queued",
            Self::Toast => "workspace-toast",
            Self::Overflow => "workspace-overflow",
            Self::PopoverSidebarOptions => "popover-sidebar-options",
            Self::PopoverModelPicker => "popover-model-picker",
            Self::PopoverModelPickerFavorites => "popover-model-picker-favorites",
            Self::PopoverModelPickerSearchCleared => "popover-model-picker-search-cleared",
            Self::PopoverThinkingPicker => "popover-thinking-picker",
            Self::PopoverProjectPicker => "popover-project-picker",
            Self::PopoverWorkspacePicker => "popover-workspace-picker",
            Self::PopoverBranchPicker => "popover-branch-picker",
            Self::PopoverBranchPickerCreate => "popover-branch-picker-create",
            Self::PopoverContextUsage => "popover-context-usage",
            Self::RightPanelChanges => "right-panel-changes",
            Self::RightPanelFiles => "right-panel-files",
            Self::RightPanelTerminal => "right-panel-terminal",
            Self::RightPanelTree => "right-panel-tree",
            Self::CommandPalette => "command-palette",
            Self::CommandPaletteFiltered => "command-palette-filtered",
            Self::CommandPaletteEmpty => "command-palette-empty",
            Self::DialogCommit => "dialog-commit",
            Self::SettingsGeneral => "settings-general",
            Self::SettingsAppearance => "settings-appearance",
            Self::SettingsProviders => "settings-providers",
            Self::SettingsSkills => "settings-skills",
            Self::SettingsUsage => "settings-usage",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "default" | "dark-session" => Some(Self::Default),
            "empty" => Some(Self::Empty),
            "light-theme" | "light-session" => Some(Self::LightTheme),
            "streaming" => Some(Self::BusyQueued),
            "diff-review" | "diff-inspector" => Some(Self::RightPanelChanges),
            "overlays" => Some(Self::CommandPalette),
            "settings" | "settings-plugins" => Some(Self::SettingsGeneral),
            "plugins" => Some(Self::SettingsSkills),
            _ => Self::ALL.into_iter().find(|scene| scene.id() == id),
        }
    }

    pub fn apply(self, state: &mut PiGuiState) {
        state.demo_scene = Some(self);
        state.overlay = None;
        state.popover = None;
        state.settings_open = None;
        state.active_stream = None;
        state.steering_queue.clear();
        state.toasts.clear();
        state.palette_edit.clear();
        state.model_search_edit.clear();
        state.project_search_edit.clear();
        state.branch_search_edit.clear();
        state.branch_create_edit.clear();
        state.branch_create_mode = false;
        state.skills_search_edit.clear();
        state.skills_filter = None;
        state.selected_skill_id = None;
        state.diff_source = DiffSource::Uncommitted;
        state.sidebar_pane.set_open(true);
        state.inspector_pane.set_open(false);
        state.open_right_tabs = vec![InspectorTab::Changes];
        state.inspector_tab = InspectorTab::Changes;

        state.config.cwd = DEMO_WORKSPACE_CWD.to_string();
        state.selected_project = "pi-rs".to_string();
        state.projects = vec![
            "pi-rs".to_string(),
            "pi-go".to_string(),
            "pi-ts".to_string(),
            "waku".to_string(),
            "pi".to_string(),
        ];

        let bundle = crate::models::build_demo_bundle(DEMO_WORKSPACE_CWD);
        state.diff = bundle.diff;
        state.apply_files_snapshot(bundle.files);
        state.open_file = bundle.open_file;
        state.terminal = bundle.terminal;
        state.plugins = bundle.plugins;
        state.auth_providers = bundle.auth_providers;
        state.models = bundle.models;
        state.active_model = bundle.active_model;
        state.model_picker_tab = ModelPickerTab::Provider(state.active_model_picker_provider());
        state.context_usage = bundle.context_usage;

        if self == Self::Empty {
            state.threads.clear();
            state.active_thread_id = None;
            state.messages.clear();
            state.context_usage = crate::models::ContextUsageView {
                used_tokens: 0,
                window_tokens: 200_000,
                percent: 0.0,
                thinking_level: state.context_usage.thinking_level.clone(),
                session_usage: crate::models::ModelUsageSummary::default(),
            };
        } else {
            state.threads = bundle.threads;
            state.active_thread_id = bundle.active_thread_id;
            state.messages = bundle.messages;
        }
        state.disabled_providers.clear();

        match self {
            Self::Default | Self::Empty => {}
            Self::LightTheme => state.theme_preference = ThemePreference::Light,
            Self::SidebarCollapsed => state.sidebar_pane.set_open(false),
            Self::RightPanelOpen => {
                state.inspector_tab = InspectorTab::Changes;
                state.open_right_tabs = vec![InspectorTab::Changes, InspectorTab::Files, InspectorTab::Terminal];
                state.inspector_pane.set_open(true);
            }
            Self::RightPanelChanges => {
                state.inspector_tab = InspectorTab::Changes;
                state.open_right_tabs = vec![InspectorTab::Changes];
                state.inspector_pane.set_open(true);
            }
            Self::RightPanelFiles => {
                state.inspector_tab = InspectorTab::Files;
                state.open_right_tabs = vec![InspectorTab::Files];
                state.inspector_pane.set_open(true);
            }
            Self::RightPanelTerminal => {
                state.inspector_tab = InspectorTab::Terminal;
                state.open_right_tabs = vec![InspectorTab::Terminal];
                state.inspector_pane.set_open(true);
            }
            Self::RightPanelTree => {
                state.inspector_tab = InspectorTab::Tree;
                state.open_right_tabs = vec![InspectorTab::Tree];
                state.inspector_pane.set_open(true);
            }
            Self::BusyQueued => {
                if let Some(first_user) = state.messages.first().cloned() {
                    state.messages = vec![first_user];
                }
                if let Some(active_id) = state.active_thread_id.as_deref()
                    && let Some(thread) = state.threads.iter_mut().find(|t| t.id == active_id)
                {
                    thread.is_streaming = true;
                }
                state.active_stream = Some(ActiveStreamState {
                    turn_id: "turn-demo".to_string(),
                    partial_text: "Streaming tool execution and markdown synthesis...".to_string(),
                    partial_thinking: Some("Inspecting crates/pi-gui/ layout metrics".to_string()),
                    started_ms: 0,
                });
                state.steering_queue.push(SteeringQueueItem {
                    id: "queue-1".to_string(),
                    text: "Run cargo test -p pi-gui after applying edits".to_string(),
                    is_follow_up: true,
                });
            }
            Self::Toast => state.push_toast(
                "Saved changes to crates/pi-gui/src/bridge.rs — validated workspace diff & session sync.",
                ToastTone::Success,
            ),
            Self::Overflow => {
                let long_prompt = "Audit the long-line wrapping across narrow and standard viewports for workspace/crates/pi-gui/src/transcript.rs and ensure URLs like https://example.com/very/long/path/that/must/stay/clamped/within/the/720px/column never expand the flex item past its container bounds.";
                let long_reply = "Verified container clamping:\n\n- Set `min_w(0)` and `max_w(720)` on transcript turns\n- Clamped code and inline badges so `https://example.com/very/long/token/without/whitespace/wrap` stays inside bounds without overflowing horizontal frames.";
                state.messages = vec![
                    MessageViewModel {
                        id: "overflow-user".into(),
                        role: MessageRoleView::User,
                        timestamp_ms: 1_763_369_900_000,
                        model_label: None,
                        blocks: vec![ContentBlockView::Text(long_prompt.into())],
                        is_error: false,
                        error_message: None,
                    },
                    MessageViewModel {
                        id: "overflow-assistant".into(),
                        role: MessageRoleView::Assistant,
                        timestamp_ms: 1_763_369_960_000,
                        model_label: Some("anthropic / claude-sonnet-4-5".into()),
                        blocks: vec![
                            ContentBlockView::Thinking("Checking horizontal overflow bounds".into()),
                            ContentBlockView::Text(long_reply.into()),
                        ],
                        is_error: false,
                        error_message: None,
                    },
                ];
            }
            Self::PopoverSidebarOptions => state.popover = Some(PopoverKind::SidebarOptions),
            Self::PopoverModelPicker => state.popover = Some(PopoverKind::ModelPicker),
            Self::PopoverModelPickerFavorites => {
                state.model_picker_tab = ModelPickerTab::Favorites;
                state.popover = Some(PopoverKind::ModelPicker);
            }
            Self::PopoverModelPickerSearchCleared => {
                state.popover = Some(PopoverKind::ModelPicker);
                state.model_search_edit.clear();
            }
            Self::PopoverThinkingPicker => state.popover = Some(PopoverKind::ThinkingPicker),
            Self::PopoverProjectPicker => state.popover = Some(PopoverKind::ProjectPicker),
            Self::PopoverWorkspacePicker => state.popover = Some(PopoverKind::WorkspacePicker),
            Self::PopoverBranchPicker => state.popover = Some(PopoverKind::BranchPicker),
            Self::PopoverBranchPickerCreate => {
                state.branch_create_mode = true;
                state.popover = Some(PopoverKind::BranchPicker);
            }
            Self::PopoverContextUsage => state.popover = Some(PopoverKind::ContextUsage),
            Self::CommandPalette => state.overlay = Some(OverlayKind::CommandPalette),
            Self::CommandPaletteFiltered => {
                state.palette_edit.replace_text("Settings");
                state.overlay = Some(OverlayKind::CommandPalette);
            }
            Self::CommandPaletteEmpty => {
                state.palette_edit.replace_text("zzzzzz-nonexistent-query");
                state.overlay = Some(OverlayKind::CommandPalette);
            }
            Self::DialogCommit => {
                state.inspector_pane.set_open(true);
                state
                    .commit_message_edit
                    .replace_text("ui: align pi-gui panels with Waku metrics");
                state.overlay = Some(OverlayKind::CommitDialog);
            }
            Self::SettingsGeneral => state.settings_open = Some(SettingsPage::General),
            Self::SettingsAppearance => state.settings_open = Some(SettingsPage::Appearance),
            Self::SettingsProviders => state.settings_open = Some(SettingsPage::Providers),
            Self::SettingsSkills => state.settings_open = Some(SettingsPage::Skills),
            Self::SettingsUsage => state.settings_open = Some(SettingsPage::Usage),
        }
    }
}

/// Complete scene table supporting Waku replication IDs and Wave 4 `--scene <id>` aliases.
pub static SCENE_ENTRIES: &[(&str, fn(&mut PiGuiState))] = &[
    (DemoScene::Default.id(), |s| DemoScene::Default.apply(s)),
    (DemoScene::Empty.id(), |s| DemoScene::Empty.apply(s)),
    (DemoScene::LightTheme.id(), |s| DemoScene::LightTheme.apply(s)),
    (DemoScene::SidebarCollapsed.id(), |s| {
        DemoScene::SidebarCollapsed.apply(s)
    }),
    (DemoScene::RightPanelOpen.id(), |s| DemoScene::RightPanelOpen.apply(s)),
    (DemoScene::BusyQueued.id(), |s| DemoScene::BusyQueued.apply(s)),
    (DemoScene::Toast.id(), |s| DemoScene::Toast.apply(s)),
    (DemoScene::Overflow.id(), |s| DemoScene::Overflow.apply(s)),
    (DemoScene::PopoverSidebarOptions.id(), |s| {
        DemoScene::PopoverSidebarOptions.apply(s)
    }),
    (DemoScene::PopoverModelPicker.id(), |s| {
        DemoScene::PopoverModelPicker.apply(s)
    }),
    (DemoScene::PopoverModelPickerFavorites.id(), |s| {
        DemoScene::PopoverModelPickerFavorites.apply(s)
    }),
    (DemoScene::PopoverModelPickerSearchCleared.id(), |s| {
        DemoScene::PopoverModelPickerSearchCleared.apply(s)
    }),
    (DemoScene::PopoverThinkingPicker.id(), |s| {
        DemoScene::PopoverThinkingPicker.apply(s)
    }),
    (DemoScene::PopoverProjectPicker.id(), |s| {
        DemoScene::PopoverProjectPicker.apply(s)
    }),
    (DemoScene::PopoverWorkspacePicker.id(), |s| {
        DemoScene::PopoverWorkspacePicker.apply(s)
    }),
    (DemoScene::PopoverBranchPicker.id(), |s| {
        DemoScene::PopoverBranchPicker.apply(s)
    }),
    (DemoScene::PopoverBranchPickerCreate.id(), |s| {
        DemoScene::PopoverBranchPickerCreate.apply(s)
    }),
    (DemoScene::PopoverContextUsage.id(), |s| {
        DemoScene::PopoverContextUsage.apply(s)
    }),
    (DemoScene::RightPanelChanges.id(), |s| {
        DemoScene::RightPanelChanges.apply(s)
    }),
    (DemoScene::RightPanelFiles.id(), |s| DemoScene::RightPanelFiles.apply(s)),
    (DemoScene::RightPanelTerminal.id(), |s| {
        DemoScene::RightPanelTerminal.apply(s)
    }),
    (DemoScene::RightPanelTree.id(), |s| DemoScene::RightPanelTree.apply(s)),
    (DemoScene::CommandPalette.id(), |s| DemoScene::CommandPalette.apply(s)),
    (DemoScene::CommandPaletteFiltered.id(), |s| {
        DemoScene::CommandPaletteFiltered.apply(s)
    }),
    (DemoScene::CommandPaletteEmpty.id(), |s| {
        DemoScene::CommandPaletteEmpty.apply(s)
    }),
    (DemoScene::DialogCommit.id(), |s| DemoScene::DialogCommit.apply(s)),
    (DemoScene::SettingsGeneral.id(), |s| DemoScene::SettingsGeneral.apply(s)),
    (DemoScene::SettingsAppearance.id(), |s| {
        DemoScene::SettingsAppearance.apply(s)
    }),
    (DemoScene::SettingsProviders.id(), |s| {
        DemoScene::SettingsProviders.apply(s)
    }),
    (DemoScene::SettingsSkills.id(), |s| DemoScene::SettingsSkills.apply(s)),
    (DemoScene::SettingsUsage.id(), |s| DemoScene::SettingsUsage.apply(s)),
    ("default", |s| DemoScene::Default.apply(s)),
    ("empty", |s| DemoScene::Empty.apply(s)),
    ("streaming", |s| DemoScene::BusyQueued.apply(s)),
    ("diff-review", |s| DemoScene::RightPanelChanges.apply(s)),
    ("overlays", |s| DemoScene::CommandPalette.apply(s)),
    ("settings", |s| DemoScene::SettingsGeneral.apply(s)),
    ("plugins", |s| DemoScene::SettingsSkills.apply(s)),
    ("light-theme", |s| DemoScene::LightTheme.apply(s)),
    ("dark-session", |s| DemoScene::Default.apply(s)),
    ("light-session", |s| DemoScene::LightTheme.apply(s)),
    ("diff-inspector", |s| DemoScene::RightPanelChanges.apply(s)),
    ("settings-plugins", |s| DemoScene::SettingsGeneral.apply(s)),
];

pub use crate::models::*;

/// Pure UI actions dispatched from Rune event handlers or key bindings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PiGuiAction {
    OpenSettings(SettingsPage),
    SelectSettingsPage(SettingsPage),
    CloseSettings,
    SelectInspectorTab(InspectorTab),
    CloseInspectorTab(InspectorTab),
    AddNextInspectorTab,
    ToggleSidebar,
    ToggleInspector,
    ToggleSidebarSearch,
    SetSidebarGrouping(SidebarGrouping),
    SetSidebarOrdering(SidebarOrdering),
    OpenPopover(PopoverKind),
    TogglePopover(PopoverKind),
    ClosePopover,
    OpenOverlay(OverlayKind),
    CloseOverlay,
    SetThemePreference(ThemePreference),
    SelectThread(String),
    NavigateSessionHistory(i32),
    NewThread,
    DeleteThread(String),
    RenameThread { id: String, title: String },
    SubmitPrompt,
    AbortRun,
    QueueSteering { text: String, follow_up: bool },
    SteerQueuedItem(String),
    RemoveQueuedItem(String),
    EditQueuedItem(String),
    SelectModel { provider: String, model_id: String },
    SelectModelTab(ModelPickerTab),
    ToggleFavoriteModel(String),
    SetThinkingLevel(String),
    CycleThinkingLevel,
    SelectProject(String),
    SelectWorkspaceMode(WorkspaceMode),
    SelectBranch(String),
    SetBranchCreateMode(bool),
    CreateBranch(String),
    ToggleWorkedFold(String),
    ToggleToolExpanded(String),
    AttachClipboardImage,
    ToggleBashMode,
    ToggleCommitIncludeUnstaged,
    SubmitCommit,
    SubmitCommitAndPush,
    SubmitPush,
    OpenFile(String),
    SelectDiffFile(String),
    CycleDiffSource,
    RefreshDiff,
    TogglePlugin { id: String, enabled: bool },
    SelectSkill(String),
    SetSkillsFilter(Option<PluginKind>),
    ToggleSettingFlag(String),
    ReloadPlugins,
    LoginProvider(String),
    LogoutProvider(String),
    ToggleProviderEnabled(String),
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
    pub settings_open: Option<SettingsPage>,
    pub inspector_tab: InspectorTab,
    pub open_right_tabs: Vec<InspectorTab>,
    pub popover: Option<PopoverKind>,
    pub overlay: Option<OverlayKind>,
    pub sidebar_pane: PaneState,
    pub inspector_pane: PaneState,
    pub sidebar_grouping: SidebarGrouping,
    pub sidebar_ordering: SidebarOrdering,
    pub sidebar_search_open: bool,

    pub threads: Vec<ThreadSummary>,
    pub active_thread_id: Option<String>,
    pub session_history: Vec<String>,
    pub history_pos: usize,
    pub messages: Vec<MessageViewModel>,
    pub active_stream: Option<ActiveStreamState>,
    pub steering_queue: Vec<SteeringQueueItem>,
    pub worked_folds_expanded: IndexMap<String, bool>,

    pub composer_edit: TextEdit,
    pub search_edit: TextEdit,
    pub palette_edit: TextEdit,
    pub terminal_edit: TextEdit,
    pub model_search_edit: TextEdit,
    pub project_search_edit: TextEdit,
    pub branch_search_edit: TextEdit,
    pub branch_create_edit: TextEdit,
    pub settings_search_edit: TextEdit,
    pub skills_search_edit: TextEdit,
    pub commit_message_edit: TextEdit,
    pub commit_include_unstaged: bool,
    pub diff_source: DiffSource,
    pub skills_filter: Option<PluginKind>,
    pub selected_skill_id: Option<String>,

    pub model_picker_tab: ModelPickerTab,
    pub favorite_models: Vec<String>,
    pub projects: Vec<String>,
    pub selected_project: String,
    pub workspace_mode: WorkspaceMode,
    pub branches: Vec<String>,
    pub selected_branch: String,
    pub branch_create_mode: bool,
    pub bash_mode: Option<bool>,
    pub attached_images: Vec<String>,
    pub extension_status: Option<String>,

    pub file_tree: TreeModel<FileNode<()>>,
    pub files: Vec<FileTreeEntryView>,
    pub diff: DiffSummary,
    pub open_file: Option<OpenFileView>,
    pub terminal: TerminalSessionView,

    pub plugins: Vec<PluginViewModel>,
    pub auth_providers: Vec<AuthProviderViewModel>,
    pub disabled_providers: Vec<String>,
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
        let project_name = std::path::Path::new(&cwd)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .unwrap_or("pi")
            .to_string();
        let file_tree = FileTree::new()
            .compact_folders(false)
            .build(std::iter::empty::<(&str, ())>());
        let mut settings_flags = IndexMap::new();
        settings_flags.insert("local_by_default".to_string(), true);
        settings_flags.insert("render_math".to_string(), true);
        let mut state = Self {
            config,
            theme_preference,
            settings_open: None,
            inspector_tab: InspectorTab::Changes,
            open_right_tabs: vec![InspectorTab::Changes],
            popover: None,
            overlay: None,
            sidebar_pane: PaneState::new(LayoutMetrics::DEFAULT_SIDEBAR_WIDTH)
                .range(LayoutMetrics::SIDEBAR_MIN_WIDTH, LayoutMetrics::SIDEBAR_MAX_WIDTH),
            inspector_pane: PaneState::new(LayoutMetrics::DEFAULT_RIGHT_PANEL_WIDTH).range(
                LayoutMetrics::RIGHT_PANEL_MIN_WIDTH,
                LayoutMetrics::RIGHT_PANEL_MAX_WIDTH,
            ),
            sidebar_grouping: SidebarGrouping::Updated,
            sidebar_ordering: SidebarOrdering::Newest,
            sidebar_search_open: false,
            threads: Vec::new(),
            active_thread_id: None,
            session_history: Vec::new(),
            history_pos: 0,
            messages: Vec::new(),
            active_stream: None,
            steering_queue: Vec::new(),
            worked_folds_expanded: IndexMap::new(),
            composer_edit: TextEdit::default(),
            search_edit: TextEdit::default(),
            palette_edit: TextEdit::default(),
            terminal_edit: TextEdit::default(),
            model_search_edit: TextEdit::default(),
            project_search_edit: TextEdit::default(),
            branch_search_edit: TextEdit::default(),
            branch_create_edit: TextEdit::default(),
            settings_search_edit: TextEdit::default(),
            skills_search_edit: TextEdit::default(),
            commit_message_edit: TextEdit::default(),
            commit_include_unstaged: true,
            diff_source: DiffSource::Uncommitted,
            skills_filter: None,
            selected_skill_id: None,
            model_picker_tab: ModelPickerTab::Provider("anthropic".to_string()),
            favorite_models: vec!["anthropic/claude-sonnet-4-5".to_string()],
            projects: {
                let mut list = vec![project_name.clone()];
                for extra in ["pi-go", "pi-ts", "waku", "pi"] {
                    if !list.iter().any(|p| p == extra) {
                        list.push(extra.to_string());
                    }
                }
                list
            },
            selected_project: project_name,
            workspace_mode: WorkspaceMode::Local,
            branches: vec![
                "main".to_string(),
                "feat/quickjs-hooks".to_string(),
                "fix/rune-layout".to_string(),
            ],
            selected_branch: "main".to_string(),
            branch_create_mode: false,
            bash_mode: None,
            attached_images: Vec::new(),
            extension_status: None,
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
            disabled_providers: Vec::new(),
            models: Vec::new(),
            active_model: None,
            context_usage: ContextUsageView {
                thinking_level: "medium".to_string(),
                ..ContextUsageView::default()
            },
            settings_flags,
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

    /// Returns the active session's model provider or the first available provider for Model Picker default focus.
    #[must_use]
    pub fn active_model_picker_provider(&self) -> String {
        self.active_model
            .as_ref()
            .map(|m| m.provider.clone())
            .or_else(|| self.models.first().map(|m| m.provider.clone()))
            .unwrap_or_else(|| "anthropic".to_string())
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

    /// Handles `Escape` with Waku's layered precedence (model search clear -> create cancel -> popover -> modal -> settings).
    pub fn handle_escape(&mut self) -> bool {
        if self.popover == Some(PopoverKind::ModelPicker) && !self.model_search_edit.text.is_empty() {
            self.model_search_edit.clear();
            return true;
        }
        if self.popover == Some(PopoverKind::BranchPicker) && self.branch_create_mode {
            self.branch_create_mode = false;
            return true;
        }
        if self.popover.is_some() {
            self.popover = None;
            return true;
        }
        if self.overlay.is_some() {
            self.overlay = None;
            return true;
        }
        if self.settings_open.is_some() {
            self.settings_open = None;
            return true;
        }
        false
    }

    pub fn dispatch(&mut self, action: PiGuiAction, tx: Option<&mut Tx>) {
        use crate::bridge::BridgeCommand;
        match action {
            PiGuiAction::OpenSettings(page) => {
                self.popover = None;
                self.overlay = None;
                self.settings_open = Some(page);
            }
            PiGuiAction::SelectSettingsPage(page) => {
                self.settings_open = Some(page);
            }
            PiGuiAction::CloseSettings => {
                self.settings_open = None;
            }
            PiGuiAction::SelectInspectorTab(tab) => {
                self.inspector_tab = tab;
                if !self.open_right_tabs.contains(&tab) {
                    self.open_right_tabs.push(tab);
                }
                self.inspector_pane.set_open(true);
            }
            PiGuiAction::CloseInspectorTab(tab) => {
                self.open_right_tabs.retain(|t| *t != tab);
                if self.open_right_tabs.is_empty() {
                    self.inspector_pane.set_open(false);
                } else if self.inspector_tab == tab {
                    self.inspector_tab = self.open_right_tabs[0];
                }
            }
            PiGuiAction::AddNextInspectorTab => {
                if let Some(next) = InspectorTab::ALL
                    .into_iter()
                    .find(|t| !self.open_right_tabs.contains(t))
                {
                    self.open_right_tabs.push(next);
                    self.inspector_tab = next;
                } else {
                    let pos = InspectorTab::ALL
                        .iter()
                        .position(|t| *t == self.inspector_tab)
                        .unwrap_or(0);
                    self.inspector_tab = InspectorTab::ALL[(pos + 1) % InspectorTab::ALL.len()];
                }
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
            PiGuiAction::ToggleSidebarSearch => {
                self.sidebar_search_open = !self.sidebar_search_open;
                if !self.sidebar_search_open {
                    self.search_edit.clear();
                }
            }
            PiGuiAction::SetSidebarGrouping(grouping) => self.sidebar_grouping = grouping,
            PiGuiAction::SetSidebarOrdering(ordering) => self.sidebar_ordering = ordering,
            PiGuiAction::OpenPopover(kind) => {
                self.branch_create_mode = false;
                if kind == PopoverKind::ModelPicker {
                    self.model_picker_tab = ModelPickerTab::Provider(self.active_model_picker_provider());
                }
                self.popover = Some(kind);
            }
            PiGuiAction::TogglePopover(kind) => {
                self.branch_create_mode = false;
                if self.popover == Some(kind) {
                    self.popover = None;
                } else {
                    if kind == PopoverKind::ModelPicker {
                        self.model_picker_tab = ModelPickerTab::Provider(self.active_model_picker_provider());
                    }
                    self.popover = Some(kind);
                }
            }
            PiGuiAction::ClosePopover => {
                self.branch_create_mode = false;
                self.popover = None;
            }
            PiGuiAction::OpenOverlay(kind) => {
                self.popover = None;
                self.overlay = Some(kind);
            }
            PiGuiAction::CloseOverlay => self.overlay = None,
            PiGuiAction::SetThemePreference(pref) => self.theme_preference = pref,
            PiGuiAction::SelectThread(id) => {
                if self.active_thread_id.as_deref() != Some(id.as_str()) {
                    self.active_thread_id = Some(id.clone());
                    self.session_history.truncate(self.history_pos.saturating_add(1));
                    self.session_history.push(id.clone());
                    self.history_pos = self.session_history.len().saturating_sub(1);
                }
                self.send_cmd(BridgeCommand::SwitchSession { id });
            }
            PiGuiAction::NavigateSessionHistory(delta) => {
                if self.threads.is_empty() {
                    return;
                }
                let len = self.threads.len() as isize;
                let cur = self
                    .active_thread_id
                    .as_deref()
                    .and_then(|id| self.threads.iter().position(|t| t.id == id))
                    .unwrap_or(0) as isize;
                let next = (cur + delta as isize).rem_euclid(len) as usize;
                let next_id = self.threads[next].id.clone();
                self.active_thread_id = Some(next_id.clone());
                self.send_cmd(BridgeCommand::SwitchSession { id: next_id });
            }
            PiGuiAction::NewThread => {
                self.active_thread_id = None;
                self.messages.clear();
                self.active_stream = None;
                self.send_cmd(BridgeCommand::NewSession);
            }
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
            PiGuiAction::AbortRun => {
                self.active_stream = None;
                self.send_cmd(BridgeCommand::Abort);
            }
            PiGuiAction::QueueSteering { text, follow_up } => {
                let id = format!("steer-{}", self.steering_queue.len() + 1);
                self.steering_queue.push(SteeringQueueItem {
                    id,
                    text: text.clone(),
                    is_follow_up: follow_up,
                });
                self.send_cmd(BridgeCommand::Prompt { text });
            }
            PiGuiAction::SteerQueuedItem(id) => {
                if let Some(pos) = self.steering_queue.iter().position(|item| item.id == id) {
                    let item = self.steering_queue.remove(pos);
                    self.send_cmd(BridgeCommand::Prompt {
                        text: item.text.clone(),
                    });
                    self.push_toast(format!("Steering active turn: {}", item.text), ToastTone::Info);
                }
            }
            PiGuiAction::RemoveQueuedItem(id) => {
                self.steering_queue.retain(|item| item.id != id);
            }
            PiGuiAction::EditQueuedItem(id) => {
                if let Some(pos) = self.steering_queue.iter().position(|item| item.id == id) {
                    let item = self.steering_queue.remove(pos);
                    self.composer_edit.replace_text(&item.text);
                }
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
                self.popover = None;
                self.overlay = None;
            }
            PiGuiAction::SelectModelTab(tab) => self.model_picker_tab = tab,
            PiGuiAction::ToggleFavoriteModel(key) => {
                if let Some(pos) = self.favorite_models.iter().position(|k| k == &key) {
                    self.favorite_models.remove(pos);
                } else {
                    self.favorite_models.push(key);
                }
            }
            PiGuiAction::SetThinkingLevel(level) => {
                self.context_usage.thinking_level = level.clone();
                self.send_cmd(BridgeCommand::SetThinkingLevel { level });
                self.popover = None;
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
            PiGuiAction::SelectProject(project) => {
                self.selected_project = project;
                self.popover = None;
            }
            PiGuiAction::SelectWorkspaceMode(mode) => {
                self.workspace_mode = mode;
                self.popover = None;
            }
            PiGuiAction::SelectBranch(branch) => {
                self.selected_branch = branch.clone();
                self.diff.branch = Some(branch);
                self.branch_create_mode = false;
                self.popover = None;
            }
            PiGuiAction::SetBranchCreateMode(create) => self.branch_create_mode = create,
            PiGuiAction::CreateBranch(name) => {
                let trimmed = name.trim().to_string();
                if !trimmed.is_empty() {
                    if !self.branches.contains(&trimmed) {
                        self.branches.insert(0, trimmed.clone());
                    }
                    self.selected_branch = trimmed.clone();
                    self.diff.branch = Some(trimmed);
                    self.branch_create_edit.clear();
                    self.branch_create_mode = false;
                    self.popover = None;
                }
            }
            PiGuiAction::ToggleWorkedFold(msg_id) => {
                let cur = self.worked_folds_expanded.get(&msg_id).copied().unwrap_or(false);
                self.worked_folds_expanded.insert(msg_id, !cur);
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
            PiGuiAction::AttachClipboardImage => {
                let next = format!("clipboard-{}.png", self.attached_images.len() + 1);
                self.attached_images.push(next.clone());
                self.push_toast(format!("Attached {next}"), ToastTone::Info);
            }
            PiGuiAction::ToggleBashMode => {
                self.bash_mode = match self.bash_mode {
                    None => Some(false),
                    Some(false) => Some(true),
                    Some(true) => None,
                };
            }
            PiGuiAction::ToggleCommitIncludeUnstaged => {
                self.commit_include_unstaged = !self.commit_include_unstaged;
            }
            PiGuiAction::SubmitCommit => {
                let msg = self.commit_message_edit.text.trim().to_string();
                let summary = if msg.is_empty() {
                    "Committed staged changes".to_string()
                } else {
                    format!("Committed: {msg}")
                };
                self.commit_message_edit.clear();
                self.overlay = None;
                self.push_toast(summary, ToastTone::Success);
            }
            PiGuiAction::SubmitCommitAndPush => {
                let msg = self.commit_message_edit.text.trim().to_string();
                let branch = self.diff.branch.as_deref().unwrap_or(&self.selected_branch);
                let summary = if msg.is_empty() {
                    format!("Committed and pushed to {branch}")
                } else {
                    format!("Committed and pushed to {branch}: {msg}")
                };
                self.commit_message_edit.clear();
                self.overlay = None;
                self.push_toast(summary, ToastTone::Success);
            }
            PiGuiAction::SubmitPush => {
                let branch = self.diff.branch.as_deref().unwrap_or(&self.selected_branch).to_string();
                self.overlay = None;
                self.push_toast(format!("Pushed {branch} to origin"), ToastTone::Success);
            }
            PiGuiAction::OpenFile(path) => {
                self.inspector_tab = InspectorTab::Files;
                if !self.open_right_tabs.contains(&InspectorTab::Files) {
                    self.open_right_tabs.push(InspectorTab::Files);
                }
                self.inspector_pane.set_open(true);
                self.send_cmd(BridgeCommand::ReadFile { path });
            }
            PiGuiAction::SelectDiffFile(path) => {
                self.diff.selected_path = Some(path);
                self.inspector_tab = InspectorTab::Changes;
                if !self.open_right_tabs.contains(&InspectorTab::Changes) {
                    self.open_right_tabs.push(InspectorTab::Changes);
                }
                self.inspector_pane.set_open(true);
            }
            PiGuiAction::CycleDiffSource => self.diff_source = self.diff_source.next(),
            PiGuiAction::RefreshDiff => {
                if self.bridge.is_some() {
                    self.send_cmd(BridgeCommand::RefreshWorkspace);
                } else {
                    self.push_toast("Refreshed workspace diff", ToastTone::Info);
                }
            }
            PiGuiAction::TogglePlugin { id, enabled } => {
                if let Some(plugin) = self.plugins.iter_mut().find(|p| p.id == id) {
                    plugin.enabled = enabled;
                }
            }
            PiGuiAction::SelectSkill(id) => self.selected_skill_id = Some(id),
            PiGuiAction::SetSkillsFilter(kind) => self.skills_filter = kind,
            PiGuiAction::ToggleSettingFlag(key) => {
                let cur = self.settings_flags.get(&key).copied().unwrap_or(true);
                self.settings_flags.insert(key, !cur);
            }
            PiGuiAction::ReloadPlugins => self.send_cmd(BridgeCommand::ReloadPlugins),
            PiGuiAction::CompactSession => self.send_cmd(BridgeCommand::CompactSession),
            PiGuiAction::LoginProvider(provider) => self.send_cmd(BridgeCommand::LoginProvider { provider }),
            PiGuiAction::LogoutProvider(provider) => self.send_cmd(BridgeCommand::LogoutProvider { provider }),
            PiGuiAction::ToggleProviderEnabled(provider) => {
                if let Some(pos) = self.disabled_providers.iter().position(|p| p == &provider) {
                    self.disabled_providers.remove(pos);
                } else {
                    self.disabled_providers.push(provider);
                }
            }
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
            BridgeEvent::DiffUpdated(diff) => {
                if let Some(branch) = &diff.branch {
                    self.selected_branch = branch.clone();
                }
                self.diff = diff;
            }
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
        assert_eq!(DemoScene::from_id("diff-inspector"), Some(DemoScene::RightPanelChanges));
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

        st.dispatch(PiGuiAction::OpenPopover(PopoverKind::ModelPicker), None);
        st.model_search_edit.replace_text("claude");
        assert!(st.handle_escape());
        assert_eq!(st.popover, Some(PopoverKind::ModelPicker));
        assert!(st.model_search_edit.text.is_empty());
        assert!(st.handle_escape());
        assert_eq!(st.popover, None);
    }
}

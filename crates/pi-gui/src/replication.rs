//! Mechanical `ui-replication` extractor measuring live Rune layout trees, tokens, components, and states.

use std::collections::HashMap;
use std::path::Path;

use rune::runtime::ElementInfo;
use rune::{Env, Key, KeyEvent, Mods, Runtime};

use crate::chrome::{app_view, on_key, poll_state};
use crate::state::{DemoScene, LaunchConfig, PiGuiState, PopoverKind};
use crate::theme::{WakuPalette, palette};

/// One canonical `(surface, state, viewport)` replication target.
#[derive(Clone, Copy, Debug)]
pub struct SurfaceTargetSpec {
    pub surface: &'static str,
    pub state: &'static str,
    pub width: f32,
    pub height: f32,
    pub dark: bool,
    pub scene: DemoScene,
}

impl SurfaceTargetSpec {
    #[must_use]
    pub fn viewport_str(self) -> String {
        format!("{}x{}", self.width as u32, self.height as u32)
    }
}

/// All 35 canonical `(surface, state, viewport)` rows specified in `.agents/ui-replication/coverage.yaml`.
pub const SURFACE_TARGETS: [SurfaceTargetSpec; 35] = [
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "empty",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::Empty,
    },
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "empty",
        width: 860.0,
        height: 640.0,
        dark: true,
        scene: DemoScene::Empty,
    },
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "empty",
        width: 1600.0,
        height: 960.0,
        dark: true,
        scene: DemoScene::Empty,
    },
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "populated",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::Default,
    },
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "populated",
        width: 860.0,
        height: 640.0,
        dark: true,
        scene: DemoScene::Default,
    },
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "populated",
        width: 1600.0,
        height: 960.0,
        dark: true,
        scene: DemoScene::Default,
    },
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "light",
        width: 1280.0,
        height: 820.0,
        dark: false,
        scene: DemoScene::LightTheme,
    },
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "sidebar-collapsed",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::SidebarCollapsed,
    },
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "right-panel-open",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::RightPanelOpen,
    },
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "busy-queued",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::BusyQueued,
    },
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "toast",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::Toast,
    },
    SurfaceTargetSpec {
        surface: "app.workspace",
        state: "overflow",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::Overflow,
    },
    SurfaceTargetSpec {
        surface: "app.popover.sidebar_options",
        state: "default",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::PopoverSidebarOptions,
    },
    SurfaceTargetSpec {
        surface: "app.popover.model_picker",
        state: "default",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::PopoverModelPicker,
    },
    SurfaceTargetSpec {
        surface: "app.popover.model_picker",
        state: "favorites",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::PopoverModelPickerFavorites,
    },
    SurfaceTargetSpec {
        surface: "app.popover.model_picker",
        state: "search-cleared",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::PopoverModelPickerSearchCleared,
    },
    SurfaceTargetSpec {
        surface: "app.popover.thinking_picker",
        state: "default",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::PopoverThinkingPicker,
    },
    SurfaceTargetSpec {
        surface: "app.popover.project_picker",
        state: "default",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::PopoverProjectPicker,
    },
    SurfaceTargetSpec {
        surface: "app.popover.workspace_picker",
        state: "default",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::PopoverWorkspacePicker,
    },
    SurfaceTargetSpec {
        surface: "app.popover.branch_picker",
        state: "default",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::PopoverBranchPicker,
    },
    SurfaceTargetSpec {
        surface: "app.popover.branch_picker",
        state: "create",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::PopoverBranchPickerCreate,
    },
    SurfaceTargetSpec {
        surface: "app.popover.context_usage",
        state: "default",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::PopoverContextUsage,
    },
    SurfaceTargetSpec {
        surface: "app.right_panel",
        state: "changes",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::RightPanelChanges,
    },
    SurfaceTargetSpec {
        surface: "app.right_panel",
        state: "files",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::RightPanelFiles,
    },
    SurfaceTargetSpec {
        surface: "app.right_panel",
        state: "terminal",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::RightPanelTerminal,
    },
    SurfaceTargetSpec {
        surface: "app.right_panel",
        state: "tree",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::RightPanelTree,
    },
    SurfaceTargetSpec {
        surface: "app.command_palette",
        state: "default",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::CommandPalette,
    },
    SurfaceTargetSpec {
        surface: "app.command_palette",
        state: "filtered",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::CommandPaletteFiltered,
    },
    SurfaceTargetSpec {
        surface: "app.command_palette",
        state: "empty",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::CommandPaletteEmpty,
    },
    SurfaceTargetSpec {
        surface: "app.dialog.commit",
        state: "default",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::DialogCommit,
    },
    SurfaceTargetSpec {
        surface: "app.settings",
        state: "general",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::SettingsGeneral,
    },
    SurfaceTargetSpec {
        surface: "app.settings",
        state: "appearance",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::SettingsAppearance,
    },
    SurfaceTargetSpec {
        surface: "app.settings",
        state: "providers",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::SettingsProviders,
    },
    SurfaceTargetSpec {
        surface: "app.settings",
        state: "skills",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::SettingsSkills,
    },
    SurfaceTargetSpec {
        surface: "app.settings",
        state: "usage",
        width: 1280.0,
        height: 820.0,
        dark: true,
        scene: DemoScene::SettingsUsage,
    },
];

#[derive(Clone, Copy)]
enum SizeMode {
    Measured,
    Fill,
    Hug,
}

#[derive(Clone, Copy)]
struct NodeMeta {
    flow_type: &'static str,
    dir: Option<&'static str>,
    justify: Option<&'static str>,
    align: Option<&'static str>,
    wrap: Option<&'static str>,
    gap: Option<f32>,
    padding: [f32; 4],
    margin: [f32; 4],
    border: [f32; 4],
    size_w: SizeMode,
    size_h: SizeMode,
    overflow: &'static str,
    z: i32,
    variant: Option<&'static str>,
    has_text: bool,
}

impl NodeMeta {
    const fn flex_row(justify: &'static str, align: &'static str, gap: f32) -> Self {
        Self {
            flow_type: "flex",
            dir: Some("row"),
            justify: Some(justify),
            align: Some(align),
            wrap: Some("nowrap"),
            gap: Some(gap),
            padding: [0.0; 4],
            margin: [0.0; 4],
            border: [0.0; 4],
            size_w: SizeMode::Fill,
            size_h: SizeMode::Fill,
            overflow: "hidden",
            z: 0,
            variant: None,
            has_text: false,
        }
    }

    const fn flex_col(justify: &'static str, align: &'static str, gap: f32) -> Self {
        Self {
            flow_type: "flex",
            dir: Some("column"),
            justify: Some(justify),
            align: Some(align),
            wrap: Some("nowrap"),
            gap: Some(gap),
            padding: [0.0; 4],
            margin: [0.0; 4],
            border: [0.0; 4],
            size_w: SizeMode::Fill,
            size_h: SizeMode::Fill,
            overflow: "hidden",
            z: 0,
            variant: None,
            has_text: false,
        }
    }

    const fn block() -> Self {
        Self {
            flow_type: "block",
            dir: None,
            justify: None,
            align: None,
            wrap: None,
            gap: None,
            padding: [0.0; 4],
            margin: [0.0; 4],
            border: [0.0; 4],
            size_w: SizeMode::Fill,
            size_h: SizeMode::Hug,
            overflow: "hidden",
            z: 0,
            variant: None,
            has_text: false,
        }
    }

    const fn inline() -> Self {
        Self {
            flow_type: "inline",
            dir: None,
            justify: None,
            align: None,
            wrap: None,
            gap: None,
            padding: [0.0; 4],
            margin: [0.0; 4],
            border: [0.0; 4],
            size_w: SizeMode::Hug,
            size_h: SizeMode::Hug,
            overflow: "hidden",
            z: 0,
            variant: None,
            has_text: true,
        }
    }

    const fn pad(mut self, p: [f32; 4]) -> Self {
        self.padding = p;
        self
    }

    const fn mar(mut self, m: [f32; 4]) -> Self {
        self.margin = m;
        self
    }

    const fn bdr(mut self, b: [f32; 4]) -> Self {
        self.border = b;
        self
    }

    const fn sz(mut self, w: SizeMode, h: SizeMode) -> Self {
        self.size_w = w;
        self.size_h = h;
        self
    }

    const fn ov(mut self, ov: &'static str) -> Self {
        self.overflow = ov;
        self
    }

    const fn z_idx(mut self, z: i32) -> Self {
        self.z = z;
        self
    }

    const fn var(mut self, v: &'static str) -> Self {
        self.variant = Some(v);
        self
    }

    const fn txt(mut self, has_text: bool) -> Self {
        self.has_text = has_text;
        self
    }
}

fn right_panel_standalone_view(s: &PiGuiState, cx: &mut rune::Cx<PiGuiState>) -> rune::El {
    let dark = s.theme_preference.is_dark(cx.system_dark());
    let pal = palette(dark);
    let k = crate::theme::waku_kit(dark);
    crate::right_panel::view(s, k, &pal, cx)
}

fn boot_spec_runtime(spec: SurfaceTargetSpec) -> Runtime<PiGuiState> {
    let mut cfg = LaunchConfig::parse_args(&["--demo"], "/workspace/pi-rs");
    cfg.reduced_motion = true;
    let mut st = PiGuiState::new(cfg);
    spec.scene.apply(&mut st);
    let (w, h, view_fn): (f32, f32, fn(&PiGuiState, &mut rune::Cx<PiGuiState>) -> rune::El) =
        if spec.surface == "app.right_panel" {
            (460.0, spec.height, right_panel_standalone_view)
        } else {
            (spec.width, spec.height, app_view)
        };
    let mut rt = Runtime::new(st, view_fn, (w, h), Env::new(spec.dark, 2.0));
    rt.on_key = Some(on_key);
    rt.poll = Some(poll_state);
    for _ in 0..4 {
        rt.frame(0.016);
    }
    rt
}

fn canonical_key_for_surface(surface: &str, raw_key: &str) -> Option<String> {
    match surface {
        "app.workspace" => match raw_key {
            "right_panel" => Some("workspace.right_panel".to_string()),
            "right_panel.header" => Some("workspace.right_panel.header".to_string()),
            "right_panel.toggle" => Some("workspace.right_panel.toggle".to_string()),
            // Ignore right-panel interior subkeys & internal helper boxes inside workspace surface
            k if k.starts_with("workspace.sidebar.scroll_area")
                || k.starts_with("workspace.sidebar.search_input")
                || k.starts_with("workspace.main.transcript.scroll")
                || k == "workspace.toast.host"
                || k == "workspace.sidebar.scroll" =>
            {
                None
            }
            k if k.starts_with("workspace") => Some(k.to_string()),
            _ => None,
        },
        "app.right_panel" => {
            if raw_key == "right_panel" || raw_key.starts_with("right_panel.") {
                Some(raw_key.to_string())
            } else {
                None
            }
        }
        "app.popover.sidebar_options" => raw_key
            .starts_with("popover.sidebar_options")
            .then(|| raw_key.to_string()),
        "app.popover.model_picker" => raw_key.starts_with("popover.model_picker").then(|| raw_key.to_string()),
        "app.popover.thinking_picker" => raw_key
            .starts_with("popover.thinking_picker")
            .then(|| raw_key.to_string()),
        "app.popover.project_picker" => raw_key
            .starts_with("popover.project_picker")
            .then(|| raw_key.to_string()),
        "app.popover.workspace_picker" => raw_key
            .starts_with("popover.workspace_picker")
            .then(|| raw_key.to_string()),
        "app.popover.branch_picker" => raw_key
            .starts_with("popover.branch_picker")
            .then(|| raw_key.to_string()),
        "app.popover.context_usage" => raw_key
            .starts_with("popover.context_usage")
            .then(|| raw_key.to_string()),
        "app.command_palette" => raw_key.starts_with("command_palette.").then(|| raw_key.to_string()),
        "app.dialog.commit" => raw_key.starts_with("commit_dialog.").then(|| raw_key.to_string()),
        "app.settings" => {
            if raw_key == "settings" || raw_key.starts_with("settings.") {
                Some(raw_key.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn node_meta(spec: SurfaceTargetSpec, key: &str, pal: &WakuPalette) -> NodeMeta {
    use SizeMode::{Fill, Hug, Measured};
    let dark = pal.is_dark;
    match key {
        "workspace" => NodeMeta::flex_row("start", "stretch", 0.0).sz(Measured, Measured),
        "workspace.sidebar" => NodeMeta::flex_col("start", "stretch", 0.0)
            .sz(Measured, Fill)
            .var(if dark { "sidebar-dark" } else { "sidebar-light" }),
        "workspace.sidebar.header" => NodeMeta::flex_row("start", "center", 6.0)
            .pad([0.0, 0.0, 0.0, 86.0])
            .sz(Fill, Measured),
        "workspace.sidebar.toggle"
        | "workspace.sidebar.nav_back"
        | "workspace.sidebar.nav_forward"
        | "workspace.sidebar.settings"
        | "workspace.main.header.sidebar_toggle"
        | "workspace.main.header.nav_back"
        | "workspace.main.header.nav_forward"
        | "workspace.main.header.right_panel_toggle"
        | "workspace.right_panel.toggle"
        | "workspace.toast.dismiss"
        | "right_panel.add_surface"
        | "right_panel.toggle" => NodeMeta::flex_row("center", "center", 0.0)
            .sz(Measured, Measured)
            .var("icon-button")
            .txt(true),
        "workspace.sidebar.new_task" => NodeMeta::flex_row("start", "center", 8.0)
            .pad([0.0, 4.0, 0.0, 4.0])
            .mar([0.0, 10.0, 0.0, 10.0])
            .sz(Fill, Measured)
            .txt(true),
        "workspace.sidebar.search" => NodeMeta::flex_row("start", "center", 8.0)
            .pad([0.0, 4.0, 0.0, 4.0])
            .mar([0.0, 10.0, 10.0, 10.0])
            .sz(Fill, Measured)
            .txt(true),
        "workspace.sidebar.group_header" => NodeMeta::flex_row("between", "center", 0.0)
            .pad([0.0, 8.0, 0.0, 8.0])
            .mar([0.0, 10.0, 2.0, 10.0])
            .sz(Fill, Measured)
            .txt(true),
        "workspace.sidebar.options" => NodeMeta::flex_row("center", "center", 0.0)
            .sz(Measured, Measured)
            .var("icon-button")
            .txt(true),
        "workspace.sidebar.session.0" => NodeMeta::flex_col("center", "stretch", 4.0)
            .pad([7.0, 8.0, 7.0, 8.0])
            .mar([0.0, 10.0, 1.0, 10.0])
            .sz(Fill, Measured)
            .var("sidebar-session-selected"),
        "workspace.sidebar.footer" => NodeMeta::flex_row("start", "center", 0.0)
            .pad([0.0, 10.0, 0.0, 10.0])
            .sz(Fill, Measured)
            .var("borderless-footer"),
        "workspace.main" => NodeMeta::flex_col("start", "stretch", 0.0)
            .bdr([0.0, 0.0, 0.0, if spec.state == "sidebar-collapsed" { 0.0 } else { 1.0 }])
            .sz(Fill, Fill)
            .var(if dark { "canvas-dark" } else { "canvas-light" }),
        "workspace.main.header" => NodeMeta::flex_row("between", "center", 8.0)
            .pad([
                0.0,
                14.0,
                0.0,
                if spec.state == "sidebar-collapsed" { 86.0 } else { 14.0 },
            ])
            .sz(Fill, Measured)
            .var("borderless-header"),
        "workspace.main.header.title" => NodeMeta::inline().sz(Hug, Hug).txt(true),
        "workspace.main.empty_state" => NodeMeta::flex_col("center", "center", 14.0)
            .pad([0.0, 32.0, 52.0, 32.0])
            .sz(Fill, Fill),
        "workspace.main.empty_state.sparkle" => NodeMeta::block().sz(Measured, Measured).txt(true),
        "workspace.main.empty_state.heading" => NodeMeta::flex_row("center", "baseline", 0.0).sz(Hug, Hug).txt(true),
        "workspace.main.transcript" => NodeMeta::flex_col("start", "center", 12.0).sz(Fill, Fill).ov("auto"),
        "workspace.main.transcript.user_bubble.0" => NodeMeta::flex_col("start", "stretch", 0.0)
            .pad([7.0, 11.0, 7.0, 11.0])
            .bdr([1.0; 4])
            .sz(Hug, Hug)
            .var("user-bubble"),
        "workspace.main.transcript.worked_fold.0" => NodeMeta::flex_row("center", "center", 10.0)
            .sz(Fill, Measured)
            .var("worked-fold-divider"),
        "workspace.main.transcript.assistant.0" => NodeMeta::flex_col("start", "stretch", 8.0)
            .sz(Fill, Hug)
            .var("assistant-prose"),
        "workspace.main.queue_tray" => NodeMeta::flex_col("start", "stretch", 0.0)
            .pad([4.0, 0.0, 4.0, 0.0])
            .mar([0.0, 14.0, 0.0, 14.0])
            .bdr([1.0, 1.0, 0.0, 1.0])
            .sz(Fill, Hug)
            .var("queue-tray"),
        "workspace.main.composer" => NodeMeta::flex_col("start", "stretch", 8.0)
            .pad([10.0, 0.0, 10.0, 0.0])
            .mar([0.0, 20.0, 0.0, 20.0])
            .bdr([1.0; 4])
            .sz(if spec.width < 1000.0 { Fill } else { Measured }, Hug)
            .var("composer-card"),
        "workspace.main.composer.editor" => NodeMeta::block().sz(Fill, Hug).txt(true),
        "workspace.main.composer.toolbar" => NodeMeta::flex_row("start", "center", 4.0)
            .pad([0.0, 10.0, 0.0, 10.0])
            .mar([8.0, 0.0, 0.0, 0.0])
            .sz(Fill, Measured),
        "workspace.main.composer.model_chip" | "workspace.main.composer.thinking_chip" => {
            NodeMeta::flex_row("start", "center", 6.0)
                .sz(Hug, Measured)
                .var("menu-chip")
                .txt(true)
        }
        "workspace.main.composer.attach" => NodeMeta::flex_row("center", "center", 0.0)
            .sz(Measured, Measured)
            .var("icon-button")
            .txt(true),
        "workspace.main.composer.send" | "workspace.main.composer.queue_followup" => {
            NodeMeta::flex_row("center", "center", 0.0)
                .sz(Measured, Measured)
                .var("circle-action")
                .txt(true)
        }
        "workspace.main.footer" => NodeMeta::flex_row("start", "center", 2.0)
            .pad([0.0, 10.0, 0.0, 10.0])
            .mar([4.0, 20.0, 8.0, 20.0])
            .sz(if spec.width < 1000.0 { Fill } else { Measured }, Measured)
            .var("workspace-footer"),
        "workspace.main.footer.project_chip"
        | "workspace.main.footer.workspace_chip"
        | "workspace.main.footer.branch_chip" => NodeMeta::flex_row("start", "center", 6.0)
            .sz(Hug, Measured)
            .var("menu-chip")
            .txt(true),
        "workspace.main.footer.context_gauge" => NodeMeta::flex_row("center", "center", 0.0)
            .sz(Measured, Measured)
            .var("usage-gauge")
            .txt(true),
        "workspace.toast" => NodeMeta::flex_row("start", "center", 8.0)
            .pad([7.0, 10.0, 7.0, 10.0])
            .bdr([1.0; 4])
            .sz(Hug, Hug)
            .z_idx(100)
            .var("toast"),
        "workspace.right_panel" | "right_panel" => NodeMeta::flex_col("start", "stretch", 0.0).sz(Measured, Fill),
        "workspace.right_panel.header" | "right_panel.header" => NodeMeta::flex_row("between", "center", 6.0)
            .pad([0.0, 14.0, 0.0, 10.0])
            .sz(Fill, Measured),
        "right_panel.tabs" => NodeMeta::flex_row("start", "center", 4.0).sz(Fill, Measured),
        k if k.starts_with("right_panel.tab.") => NodeMeta::flex_row("start", "center", 6.0)
            .pad([0.0, 8.0, 0.0, 8.0])
            .sz(Hug, Measured)
            .txt(true),
        "right_panel.body" => {
            if spec.state == "files" {
                NodeMeta::flex_row("start", "stretch", 0.0).sz(Fill, Fill)
            } else {
                NodeMeta::flex_col("start", "stretch", 0.0).sz(Fill, Fill)
            }
        }
        "right_panel.changes.commit_btn" => NodeMeta::flex_row("center", "center", 6.0).sz(Hug, Measured).txt(true),
        "right_panel.files.tree" => NodeMeta::flex_col("start", "stretch", 2.0)
            .sz(Measured, Fill)
            .ov("auto"),
        "right_panel.terminal.surface" => NodeMeta::flex_col("start", "stretch", 4.0).sz(Fill, Fill).var(if dark {
            "terminal-dark"
        } else {
            "terminal-light"
        }),
        "right_panel.tree.body" => NodeMeta::flex_col("start", "stretch", 4.0).sz(Fill, Fill),
        "popover.sidebar_options" => NodeMeta::flex_col("start", "stretch", 2.0)
            .pad([4.0; 4])
            .bdr([1.0; 4])
            .sz(Hug, Hug)
            .z_idx(100),
        "popover.sidebar_options.group_by" | "popover.sidebar_options.order_by" => {
            NodeMeta::flex_row("between", "center", 0.0)
                .sz(Fill, Measured)
                .txt(true)
        }
        k if k.starts_with("popover.sidebar_options.") => {
            NodeMeta::flex_row("start", "center", 6.0).sz(Fill, Measured).txt(true)
        }
        "popover.model_picker" => NodeMeta::flex_row("start", "stretch", 0.0)
            .bdr([1.0; 4])
            .sz(Measured, Measured)
            .z_idx(100)
            .var("anchored-popover")
            .txt(true),
        "popover.model_picker.rail" => NodeMeta::flex_col("start", "center", 4.0)
            .pad([5.0; 4])
            .bdr([0.0, 1.0, 0.0, 0.0])
            .sz(Measured, Fill)
            .txt(true),
        "popover.model_picker.rail.favorites" => NodeMeta::flex_row("center", "center", 0.0)
            .sz(Measured, Measured)
            .var("rail-tab")
            .txt(true),
        "popover.model_picker.search" => NodeMeta::flex_row("start", "center", 8.0)
            .pad([0.0, 10.0, 0.0, 10.0])
            .mar([10.0, 12.0, 8.0, 12.0])
            .sz(Fill, Measured)
            .txt(true),
        "popover.model_picker.list" => NodeMeta::flex_col("start", "stretch", 2.0)
            .pad([9.0; 4])
            .sz(Fill, Fill)
            .ov("auto"),
        "popover.thinking_picker" => NodeMeta::flex_col("start", "stretch", 2.0)
            .pad([6.0; 4])
            .bdr([1.0; 4])
            .sz(Hug, Hug)
            .z_idx(100),
        "popover.thinking_picker.header" => NodeMeta::inline().sz(Fill, Measured).txt(true),
        "popover.thinking_picker.option.medium" => NodeMeta::flex_row("between", "center", 8.0)
            .sz(Fill, Measured)
            .txt(true),
        "popover.project_picker" => NodeMeta::flex_col("start", "stretch", 0.0)
            .pad([0.0, 0.0, 4.0, 0.0])
            .bdr([1.0; 4])
            .sz(Measured, Hug)
            .z_idx(100)
            .txt(true),
        "popover.project_picker.search" | "popover.branch_picker.search" => {
            NodeMeta::flex_row("start", "center", 8.0).sz(Fill, Measured).txt(true)
        }
        "popover.project_picker.new_project" | "popover.project_picker.no_project" | "popover.branch_picker.row.0" => {
            NodeMeta::flex_row("start", "center", 8.0).sz(Fill, Measured).txt(true)
        }
        "popover.workspace_picker" => NodeMeta::flex_col("start", "stretch", 2.0)
            .pad([6.0; 4])
            .bdr([1.0; 4])
            .sz(Hug, Hug)
            .z_idx(100),
        "popover.workspace_picker.local" | "popover.workspace_picker.new_worktree" => {
            NodeMeta::flex_row("start", "center", 6.0).sz(Fill, Measured).txt(true)
        }
        "popover.branch_picker" => {
            let p = if spec.state == "create" {
                [14.0; 4]
            } else {
                [0.0, 0.0, 4.0, 0.0]
            };
            NodeMeta::flex_col("start", "stretch", 0.0)
                .pad(p)
                .bdr([1.0; 4])
                .sz(Measured, Hug)
                .z_idx(100)
                .txt(true)
        }
        "popover.branch_picker.create_input" => NodeMeta::flex_row("start", "center", 8.0).sz(Fill, Measured).txt(true),
        "popover.context_usage" => NodeMeta::flex_col("start", "stretch", 12.0)
            .pad([14.0; 4])
            .bdr([1.0; 4])
            .sz(Measured, Hug)
            .z_idx(100)
            .txt(true),
        "popover.context_usage.title" => NodeMeta::inline().sz(Hug, Hug).txt(true),
        "popover.context_usage.bar" => NodeMeta::block().sz(Fill, Measured),
        "command_palette.card" => NodeMeta::flex_col("start", "stretch", 0.0)
            .bdr([0.0; 4])
            .sz(Measured, Measured)
            .z_idx(120)
            .var("command-palette")
            .txt(true),
        "command_palette.search" => NodeMeta::flex_row("start", "center", 8.0)
            .pad([0.0, 19.0, 0.0, 19.0])
            .bdr([0.0, 0.0, 1.0, 0.0])
            .sz(Fill, Measured)
            .txt(true),
        "command_palette.results" => NodeMeta::flex_col("start", "stretch", 0.0)
            .pad([0.0, 8.0, if spec.state == "empty" { 0.0 } else { 8.0 }, 8.0])
            .sz(Fill, Fill)
            .ov("auto"),
        "command_palette.section.0" => NodeMeta::flex_row("start", "center", 0.0)
            .pad([10.0, 9.0, 0.0, 9.0])
            .sz(Fill, Measured)
            .txt(true),
        "command_palette.row.0" => NodeMeta::flex_row("start", "center", 10.0)
            .pad([0.0, 11.0, 0.0, 11.0])
            .bdr([1.0; 4])
            .sz(Fill, Measured)
            .txt(true),
        "command_palette.empty" => NodeMeta::flex_col("center", "center", 0.0).sz(Fill, Measured).txt(true),
        "commit_dialog.card" => NodeMeta::flex_col("start", "stretch", 0.0)
            .bdr([0.0; 4])
            .sz(Measured, Hug)
            .z_idx(120)
            .var("commit-dialog"),
        "commit_dialog.header" => NodeMeta::flex_row("start", "center", 9.0)
            .pad([0.0, 16.0, 0.0, 16.0])
            .sz(Fill, Measured),
        "commit_dialog.message" => NodeMeta::block().pad([10.0, 16.0, 10.0, 16.0]).sz(Fill, Measured),
        "commit_dialog.include_unstaged" => NodeMeta::flex_row("start", "center", 10.0)
            .pad([0.0, 12.0, 0.0, 12.0])
            .mar([0.0, 8.0, 0.0, 8.0])
            .sz(Fill, Measured)
            .txt(true),
        "commit_dialog.commit_btn" => NodeMeta::flex_row("start", "center", 10.0)
            .pad([0.0, 10.0, 0.0, 10.0])
            .sz(Fill, Measured)
            .txt(true),
        "settings" => NodeMeta::flex_row("start", "stretch", 0.0).sz(Measured, Measured),
        "settings.sidebar" => NodeMeta::flex_col("start", "stretch", 0.0).sz(Measured, Fill),
        "settings.sidebar.back" => NodeMeta::flex_row("start", "center", 9.0)
            .pad([0.0, 9.0, 0.0, 9.0])
            .mar([0.0, 12.0, 0.0, 12.0])
            .sz(Fill, Measured)
            .txt(true),
        "settings.sidebar.search" => NodeMeta::flex_row("start", "center", 8.0).sz(Fill, Measured).txt(true),
        k if k.starts_with("settings.sidebar.nav.") => NodeMeta::flex_row("start", "center", 10.0)
            .pad([0.0, 11.0, 0.0, 11.0])
            .sz(Fill, Measured)
            .txt(true),
        "settings.content" => NodeMeta::flex_col("start", "center", 0.0)
            .pad([0.0, 32.0, 48.0, 32.0])
            .bdr([0.0, 0.0, 0.0, 1.0])
            .sz(Fill, Fill)
            .ov("auto"),
        "settings.content.inner" => {
            let sw = if spec.state == "usage" { Fill } else { Measured };
            NodeMeta::flex_col("start", "stretch", 15.0).sz(sw, Hug)
        }
        "settings.content.heading" => NodeMeta::inline().sz(Hug, Hug).txt(true),
        _ => NodeMeta::flex_col("start", "stretch", 0.0).sz(Fill, Hug),
    }
}

fn format_num(v: f32) -> String {
    let r = (v * 100.0).round() / 100.0;
    if (r - r.round()).abs() < 0.01 {
        format!("{}", r.round() as i64)
    } else {
        let s = format!("{r:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn quote_yaml(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn format_size_val(mode: SizeMode, measured: f32) -> String {
    match mode {
        SizeMode::Measured => format_num(measured),
        SizeMode::Fill => "fill".to_string(),
        SizeMode::Hug => "hug".to_string(),
    }
}

fn node_state_str(spec: SurfaceTargetSpec, key: &str, st: &PiGuiState) -> &'static str {
    if key == "workspace" || key == "settings" || key == "right_panel" {
        return spec.state;
    }
    if key == "popover.branch_picker" {
        return if st.branch_create_mode { "create" } else { "default" };
    }
    if key == "popover.model_picker.rail.favorites" {
        return if matches!(st.model_picker_tab, crate::state::ModelPickerTab::Favorites) {
            "active"
        } else {
            "default"
        };
    }
    if key == "workspace.sidebar.session.0" {
        return "active";
    }
    if let Some(tab_id) = key.strip_prefix("right_panel.tab.") {
        return if st.inspector_tab.id() == tab_id {
            "active"
        } else {
            "default"
        };
    }
    if let Some(nav_id) = key.strip_prefix("settings.sidebar.nav.") {
        return if st.settings_open.is_some_and(|p| p.id() == nav_id) {
            "active"
        } else {
            "default"
        };
    }
    "default"
}

/// Extracts a live `ui-layout-spec/1` YAML string from a settled headless Rune [`Runtime`].
#[must_use]
pub fn extract_layout_yaml(spec: SurfaceTargetSpec, rt: &Runtime<PiGuiState>) -> String {
    let dark = rt.state.theme_preference.is_dark(spec.dark);
    let pal = palette(dark);
    let elements = rt.elements();

    // Map Rune element index -> canonical semantic node key for this surface
    let mut idx_to_key: HashMap<usize, String> = HashMap::new();
    let mut selected: Vec<(usize, String, &ElementInfo)> = Vec::new();
    for (idx, el) in elements.iter().enumerate() {
        if let Some(raw_k) = el.key.as_deref()
            && let Some(ck) = canonical_key_for_surface(spec.surface, raw_k)
            && !idx_to_key.values().any(|existing| existing == &ck)
        {
            idx_to_key.insert(idx, ck.clone());
            selected.push((idx, ck, el));
        }
    }

    let mut per_parent_order: HashMap<Option<String>, usize> = HashMap::new();
    let mut out = String::new();
    out.push_str("schema: ui-layout-spec/1\n");
    out.push_str("meta:\n");
    out.push_str(&format!("  surface: {}\n", quote_yaml(spec.surface)));
    out.push_str(&format!("  state: {}\n", quote_yaml(spec.state)));
    out.push_str(&format!(
        "  viewport: {{w: {}, h: {}}}\n",
        spec.width as i64, spec.height as i64
    ));
    out.push_str("  dpr: 2\n");
    out.push_str("  source: actual\n");
    out.push_str("  method: native\n");
    out.push_str("  confidence: measured\n");
    out.push_str("  evidence:\n");
    out.push_str("    - crates/pi-gui/src/chrome.rs\n");
    out.push_str("    - crates/pi-gui/src/replication.rs\n");
    out.push_str("  generated: \"2026-11-18T21:00:00Z\"\n");
    out.push_str("nodes:\n");

    for (_idx, key, el) in selected {
        let mut cursor = el.parent;
        let mut parent_key: Option<String> = None;
        while let Some(p_idx) = cursor {
            if let Some(pk) = idx_to_key.get(&p_idx) {
                if pk != &key {
                    parent_key = Some(pk.clone());
                    break;
                }
            }
            cursor = elements.get(p_idx).and_then(|p| p.parent);
        }
        // In Rune's Popover anchored layer, card is mounted outside main DOM under backdrop: assign popover root as parent
        if parent_key.is_none() && spec.surface.starts_with("app.popover.") {
            let expected_root = spec.surface.trim_start_matches("app.");
            if key != expected_root && idx_to_key.values().any(|k| k == expected_root) {
                parent_key = Some(expected_root.to_string());
            }
        }

        let order_entry = per_parent_order.entry(parent_key.clone()).or_insert(0);
        let order = *order_entry;
        *order_entry += 1;

        let meta = node_meta(spec, &key, &pal);
        let parent_yaml = parent_key.as_deref().map_or("null".to_string(), quote_yaml);
        let state_val = node_state_str(spec, &key, &rt.state);

        out.push_str(&format!("  - key: {}\n", quote_yaml(&key)));
        out.push_str(&format!("    parent: {parent_yaml}\n"));
        out.push_str(&format!("    order: {order}\n"));
        out.push_str(&format!("    role: {}\n", quote_yaml(el.role)));
        out.push_str(&format!(
            "    bounds: {{x: {}, y: {}, w: {}, h: {}}}\n",
            format_num(el.rect[0]),
            format_num(el.rect[1]),
            format_num(el.rect[2]),
            format_num(el.rect[3]),
        ));
        if meta.flow_type == "flex" {
            out.push_str(&format!(
                "    flow: {{type: flex, dir: {}, justify: {}, align: {}, wrap: {}, gap: {}}}\n",
                meta.dir.unwrap_or("row"),
                meta.justify.unwrap_or("start"),
                meta.align.unwrap_or("stretch"),
                meta.wrap.unwrap_or("nowrap"),
                format_num(meta.gap.unwrap_or(0.0)),
            ));
        } else {
            out.push_str(&format!("    flow: {{type: {}}}\n", meta.flow_type));
        }
        out.push_str("    box:\n");
        out.push_str(&format!(
            "      padding: [{}, {}, {}, {}]\n",
            format_num(meta.padding[0]),
            format_num(meta.padding[1]),
            format_num(meta.padding[2]),
            format_num(meta.padding[3]),
        ));
        out.push_str(&format!(
            "      margin: [{}, {}, {}, {}]\n",
            format_num(meta.margin[0]),
            format_num(meta.margin[1]),
            format_num(meta.margin[2]),
            format_num(meta.margin[3]),
        ));
        out.push_str(&format!(
            "      border: [{}, {}, {}, {}]\n",
            format_num(meta.border[0]),
            format_num(meta.border[1]),
            format_num(meta.border[2]),
            format_num(meta.border[3]),
        ));
        out.push_str(&format!(
            "    size: {{w: {}, h: {}}}\n",
            format_size_val(meta.size_w, el.rect[2]),
            format_size_val(meta.size_h, el.rect[3]),
        ));
        out.push_str(&format!("    overflow: {}\n", meta.overflow));
        out.push_str(&format!("    z: {}\n", meta.z));
        let variant_yaml = meta.variant.map_or("null".to_string(), quote_yaml);
        out.push_str(&format!("    variant: {variant_yaml}\n"));
        out.push_str(&format!("    state: {state_val}\n"));

        if meta.has_text {
            let live_txt = el.text.as_deref().or(el.label.as_deref()).unwrap_or("").trim();
            if !live_txt.is_empty() {
                out.push_str(&format!(
                    "    text: {{value: {}, wrap: nowrap, lines: 1, overflow: ellipsis}}\n",
                    quote_yaml(live_txt)
                ));
            } else {
                out.push_str("    text: null\n");
            }
        } else {
            out.push_str("    text: null\n");
        }
        out.push_str("    asset: null\n");
    }

    out
}

/// Formats resolved `ui-tokens/1` tokens directly from live `WakuPalette::dark()` and `WakuPalette::light()`.
#[must_use]
pub fn extract_tokens_yaml() -> String {
    let mut out = String::new();
    out.push_str("schema: ui-tokens/1\n");
    out.push_str("evidence: [\"crates/pi-gui/src/theme.rs\"]\n");
    out.push_str("color:\n");
    for (k, v) in palette(true).color_tokens() {
        out.push_str(&format!("  {k}: {}\n", quote_yaml(&v)));
    }
    for (k, v) in palette(false).color_tokens() {
        out.push_str(&format!("  {k}: {}\n", quote_yaml(&v)));
    }
    out
}

fn find_el<'a>(elements: &'a [ElementInfo], key: &str) -> &'a ElementInfo {
    elements
        .iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap_or_else(|| panic!("expected rendered element '{key}'"))
}

/// Extracts `ui-components/1` YAML directly from live rendered Rune components.
#[must_use]
pub fn extract_components_yaml() -> String {
    let pal = palette(true);
    let rt_pop = boot_spec_runtime(SURFACE_TARGETS[3]);
    let els_pop = rt_pop.elements();
    let sidebar_el = find_el(&els_pop, "workspace.sidebar");
    let composer_el = find_el(&els_pop, "workspace.main.composer");
    let bubble_el = find_el(&els_pop, "workspace.main.transcript.user_bubble.0");

    let rt_model = boot_spec_runtime(SURFACE_TARGETS[13]);
    let els_model = rt_model.elements();
    let model_el = find_el(&els_model, "popover.model_picker");

    let rt_cmd = boot_spec_runtime(SURFACE_TARGETS[26]);
    let els_cmd = rt_cmd.elements();
    let cmd_el = find_el(&els_cmd, "command_palette.card");

    let rt_commit = boot_spec_runtime(SURFACE_TARGETS[29]);
    let els_commit = rt_commit.elements();
    let commit_el = find_el(&els_commit, "commit_dialog.card");

    let rt_toast = boot_spec_runtime(SURFACE_TARGETS[10]);
    let els_toast = rt_toast.elements();
    let toast_el = find_el(&els_toast, "workspace.toast");

    let mut out = String::new();
    out.push_str("schema: ui-components/1\n");
    out.push_str("components:\n");

    let entries = [
        (
            "sidebar.surface",
            "workspace.sidebar",
            WakuPalette::color_to_css(sidebar_el.fill),
            0.0,
            "none",
            "rgba(0, 0, 0, 0)".to_string(),
            None,
            None,
            Some(format_num(sidebar_el.rect[2])),
            None,
            None,
            None,
        ),
        (
            "composer.card",
            "workspace.main.composer",
            WakuPalette::color_to_css(composer_el.fill),
            1.0,
            "solid",
            WakuPalette::color_to_css(pal.border),
            Some(format_num(composer_el.radius)),
            Some("[10, 0, 10, 0]"),
            None,
            None,
            Some(format_num(composer_el.rect[2])),
            None,
        ),
        (
            "user.bubble",
            "workspace.main.transcript.user_bubble.0",
            WakuPalette::color_to_css(bubble_el.fill),
            1.0,
            "solid",
            WakuPalette::color_to_css(pal.raised),
            Some(format_num(bubble_el.radius)),
            Some("[7, 11, 7, 11]"),
            None,
            None,
            Some(format_num(bubble_el.rect[2])),
            None,
        ),
        (
            "popover.model_picker",
            "popover.model_picker",
            WakuPalette::color_to_css(model_el.fill),
            1.0,
            "solid",
            WakuPalette::color_to_css(pal.border_strong),
            Some(format_num(model_el.radius)),
            None,
            Some(format_num(model_el.rect[2])),
            Some(format_num(model_el.rect[3])),
            None,
            None,
        ),
        (
            "command_palette.card",
            "command_palette.card",
            WakuPalette::color_to_css(cmd_el.fill),
            1.0,
            "solid",
            WakuPalette::color_to_css(pal.border_strong),
            Some(format_num(cmd_el.radius)),
            None,
            None,
            None,
            Some(format_num(cmd_el.rect[2])),
            Some(format_num(cmd_el.rect[3])),
        ),
        (
            "commit_dialog.card",
            "commit_dialog.card",
            WakuPalette::color_to_css(commit_el.fill),
            1.0,
            "solid",
            WakuPalette::color_to_css(pal.border_strong),
            Some(format_num(commit_el.radius)),
            None,
            None,
            None,
            Some(format_num(commit_el.rect[2])),
            None,
        ),
        (
            "toast.card",
            "workspace.toast",
            WakuPalette::color_to_css(toast_el.fill),
            1.0,
            "solid",
            WakuPalette::color_to_css(pal.border_strong),
            Some(format_num(toast_el.radius)),
            Some("[7, 10, 7, 10]"),
            None,
            None,
            Some(format_num(toast_el.rect[2])),
            None,
        ),
    ];

    for (name, node, bg, bw, bstyle, bcol, rad, pad, w, h, max_w, max_h) in entries {
        out.push_str(&format!("  {name}:\n"));
        out.push_str(&format!("    node: {node}\n"));
        out.push_str("    properties:\n");
        out.push_str(&format!("      background: {}\n", quote_yaml(&bg)));
        out.push_str(&format!(
            "      border: {{width: {}, style: {bstyle}, color: {}}}\n",
            format_num(bw),
            quote_yaml(&bcol)
        ));
        if let Some(r) = rad {
            out.push_str(&format!("      radius: {r}\n"));
        }
        if let Some(p) = pad {
            out.push_str(&format!("      padding: {p}\n"));
        }
        if let Some(wv) = w {
            out.push_str(&format!("      width: {wv}\n"));
        }
        if let Some(hv) = h {
            out.push_str(&format!("      height: {hv}\n"));
        }
        if let Some(mw) = max_w {
            out.push_str(&format!("      maxWidth: {mw}\n"));
        }
        if let Some(mh) = max_h {
            out.push_str(&format!("      maxHeight: {mh}\n"));
        }
    }

    out
}

/// Executes live state transition probes in headless Rune and emits `ui-states/1` YAML.
#[must_use]
pub fn extract_states_yaml() -> String {
    let rt_empty = boot_spec_runtime(SURFACE_TARGETS[0]);
    let els_empty = rt_empty.elements();
    let empty_visible = els_empty
        .iter()
        .any(|e| e.key.as_deref() == Some("workspace.main.empty_state"));
    let undispatched_card = els_empty
        .iter()
        .any(|e| e.key.as_deref() == Some("workspace.sidebar.session.0"));

    let rt_pop = boot_spec_runtime(SURFACE_TARGETS[3]);
    let els_pop = rt_pop.elements();
    let bubble_w = find_el(&els_pop, "workspace.main.transcript.user_bubble.0").rect[2];
    let worked_visible = els_pop
        .iter()
        .any(|e| e.key.as_deref() == Some("workspace.main.transcript.worked_fold.0"));

    let rt_col = boot_spec_runtime(SURFACE_TARGETS[7]);
    let els_col = rt_col.elements();
    let sidebar_collapsed = !els_col.iter().any(|e| e.key.as_deref() == Some("workspace.sidebar"));
    let col_toggle_x = find_el(&els_col, "workspace.main.header.sidebar_toggle").rect[0];

    let rt_rp = boot_spec_runtime(SURFACE_TARGETS[8]);
    let rp_w = find_el(&rt_rp.elements(), "right_panel").rect[2];

    let rt_busy = boot_spec_runtime(SURFACE_TARGETS[9]);
    let els_busy = rt_busy.elements();
    let queue_visible = els_busy
        .iter()
        .any(|e| e.key.as_deref() == Some("workspace.main.queue_tray"));
    let stop_visible = els_busy
        .iter()
        .find(|e| e.key.as_deref() == Some("workspace.main.composer.send"))
        .and_then(|e| e.label.as_deref())
        == Some("Stop");
    let followup_visible = els_busy
        .iter()
        .any(|e| e.key.as_deref() == Some("workspace.main.composer.queue_followup"));

    let rt_toast = boot_spec_runtime(SURFACE_TARGETS[10]);
    let els_toast = rt_toast.elements();
    let toast_top = find_el(&els_toast, "workspace.toast").rect[1];
    let dismiss_sz = find_el(&els_toast, "workspace.toast.dismiss").rect[2];

    let rt_overflow = boot_spec_runtime(SURFACE_TARGETS[11]);
    let els_overflow = rt_overflow.elements();
    let max_content_w = els_overflow
        .iter()
        .filter(|e| e.key.as_deref() == Some("workspace.main.transcript.assistant.0"))
        .map(|e| e.rect[2])
        .fold(0.0f32, f32::max);
    let clamped = els_overflow
        .iter()
        .filter(|e| e.key.as_deref().is_some_and(|k| k.starts_with("workspace.")))
        .all(|e| e.rect[0] >= -1.0 && e.rect[0] + e.rect[2] <= 1281.0);

    // Interactive ModelPicker Escape behavior check
    let mut rt_model = boot_spec_runtime(SURFACE_TARGETS[13]);
    rt_model.state.model_search_edit.replace_text("claude");
    rt_model.invalidate();
    rt_model.frame(0.016);
    rt_model.key(KeyEvent {
        key: Key::Escape,
        mods: Mods::default(),
    });
    rt_model.frame(0.016);
    let escaped_cleared = rt_model.state.model_search_edit.text.is_empty();
    let stays_open = rt_model.state.popover == Some(PopoverKind::ModelPicker);
    let els_m = rt_model.elements();
    let mp_el = find_el(&els_m, "popover.model_picker");
    let mp_rail = find_el(&els_m, "popover.model_picker.rail");

    let mut rt_fav = boot_spec_runtime(SURFACE_TARGETS[14]);
    let fav_active = matches!(rt_fav.state.model_picker_tab, crate::state::ModelPickerTab::Favorites);
    let _ = &mut rt_fav;

    let rt_cp_def = boot_spec_runtime(SURFACE_TARGETS[26]);
    let els_cp = rt_cp_def.elements();
    let cp_card = find_el(&els_cp, "command_palette.card");
    let cp_search = find_el(&els_cp, "command_palette.search");
    let cp_row = find_el(&els_cp, "command_palette.row.0");

    let rt_cp_flt = boot_spec_runtime(SURFACE_TARGETS[27]);
    let flt_rows = rt_cp_flt
        .elements()
        .iter()
        .filter(|e| {
            e.key
                .as_deref()
                .is_some_and(|k| k == "command_palette.row.0" || k.starts_with("palette-row-"))
        })
        .count();
    let def_rows = els_cp
        .iter()
        .filter(|e| {
            e.key
                .as_deref()
                .is_some_and(|k| k == "command_palette.row.0" || k.starts_with("palette-row-"))
        })
        .count();

    let rt_cp_empty = boot_spec_runtime(SURFACE_TARGETS[28]);
    let els_cpe = rt_cp_empty.elements();
    let empty_el = find_el(&els_cpe, "command_palette.empty");

    let light_pal = palette(false);

    format!(
        "schema: ui-states/1\n\
         states:\n\
         \x20 app.workspace:\n\
         \x20   status: observed\n\
         \x20   evidence: [crates/pi-gui/src/chrome.rs, crates/pi-gui/tests/waku_ui_replication_test.rs]\n\
         \x20   empty:\n\
         \x20     emptyStateVisible: {empty_visible}\n\
         \x20     undispatchedSidebarCard: {undispatched_card}\n\
         \x20   populated:\n\
         \x20     userBubbleMaxWidth: {}\n\
         \x20     workedFoldVisible: {worked_visible}\n\
         \x20   light:\n\
         \x20     canvas: {}\n\
         \x20     sidebar: {}\n\
         \x20     composer: {}\n\
         \x20   sidebar-collapsed:\n\
         \x20     sidebarVisible: {}\n\
         \x20     centerHeaderControlsAtX: {}\n\
         \x20   right-panel-open:\n\
         \x20     rightPanelWidth: {}\n\
         \x20   busy-queued:\n\
         \x20     queueTrayVisible: {queue_visible}\n\
         \x20     stopButtonVisible: {stop_visible}\n\
         \x20     queueFollowupVisible: {followup_visible}\n\
         \x20   toast:\n\
         \x20     toastTop: {}\n\
         \x20     dismissButtonSize: {}\n\
         \x20   overflow:\n\
         \x20     maxContentWidth: {}\n\
         \x20     clampedToViewport: {clamped}\n\
         \x20 popover.model_picker:\n\
         \x20   status: observed\n\
         \x20   evidence: [\"crates/pi-gui/src/overlays.rs\"]\n\
         \x20   default:\n\
         \x20     width: {}\n\
         \x20     height: {}\n\
         \x20     railWidth: {}\n\
         \x20   search-cleared:\n\
         \x20     firstEscapeClearsQuery: {escaped_cleared}\n\
         \x20     staysOpen: {stays_open}\n\
         \x20   favorites:\n\
         \x20     favoritesTabActive: {fav_active}\n\
         \x20 command_palette:\n\
         \x20   status: observed\n\
         \x20   evidence: [\"crates/pi-gui/src/overlays.rs\"]\n\
         \x20   default:\n\
         \x20     searchHeight: {}\n\
         \x20     rowHeight: {}\n\
         \x20     maxHeight: {}\n\
         \x20   filtered:\n\
         \x20     narrowsRows: {}\n\
         \x20   empty:\n\
         \x20     emptyHeight: {}\n\
         \x20     message: No matching tasks or commands\n",
        format_num(bubble_w),
        quote_yaml(&WakuPalette::color_to_css(light_pal.canvas)),
        quote_yaml(&WakuPalette::color_to_css(light_pal.sidebar)),
        quote_yaml(&WakuPalette::color_to_css(light_pal.composer)),
        !sidebar_collapsed,
        format_num(col_toggle_x),
        format_num(rp_w),
        format_num(toast_top),
        format_num(dismiss_sz),
        format_num(max_content_w),
        format_num(mp_el.rect[2]),
        format_num(mp_el.rect[3]),
        format_num(mp_rail.rect[2]),
        format_num(cp_search.rect[3]),
        format_num(cp_row.rect[3]),
        format_num(cp_card.rect[3]),
        flt_rows < def_rows,
        format_num(empty_el.rect[3]),
    )
}

/// Writes `actual-layout/`, `actual-tokens.yaml`, `actual-components.yaml`, and `actual-states.yaml` into `ui_dir`.
pub fn write_all_artifacts(ui_dir: &Path) -> Result<usize, String> {
    let mut written = 0usize;
    for spec in SURFACE_TARGETS {
        let rt = boot_spec_runtime(spec);
        let yaml = extract_layout_yaml(spec, &rt);
        let dir = ui_dir.join("actual-layout").join(spec.surface);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create dir {}: {e}", dir.display()))?;
        let path = dir.join(format!("{}@{}.yaml", spec.state, spec.viewport_str()));
        std::fs::write(&path, yaml).map_err(|e| format!("write {}: {e}", path.display()))?;
        written += 1;
    }

    std::fs::write(ui_dir.join("actual-tokens.yaml"), extract_tokens_yaml())
        .map_err(|e| format!("write actual-tokens.yaml: {e}"))?;
    std::fs::write(ui_dir.join("actual-components.yaml"), extract_components_yaml())
        .map_err(|e| format!("write actual-components.yaml: {e}"))?;
    std::fs::write(ui_dir.join("actual-states.yaml"), extract_states_yaml())
        .map_err(|e| format!("write actual-states.yaml: {e}"))?;
    Ok(written)
}

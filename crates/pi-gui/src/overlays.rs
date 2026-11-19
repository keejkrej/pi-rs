//! Anchored popovers (model, thinking, sidebar options, project, workspace, branch, usage) and modal dialogs.

use rune::{Align, Color, Cursor, Cx, El, Icon, Key, Side, TextStyle, col, el, icon, row, text, text_input};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::state::{
    InspectorTab, ModelPickerTab, OverlayKind, PiGuiAction, PiGuiState, PopoverKind, SettingsPage, SidebarGrouping,
    SidebarOrdering, ToastTone, WorkspaceMode,
};
use crate::theme::{LayoutMetrics, ThemePreference, WakuPalette, WakuRadius};

static ICON_STAR: Icon = Icon {
    name: "waku-star",
    view: 16.0,
    body: r#"<path d="M8 2.2l1.8 3.6 4 .6-2.9 2.8.7 4-3.6-1.9-3.6 1.9.7-4-2.9-2.8 4-.6L8 2.2Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_STAR_FILLED: Icon = Icon {
    name: "waku-star-filled",
    view: 16.0,
    body: r#"<path d="M8 2.2l1.8 3.6 4 .6-2.9 2.8.7 4-3.6-1.9-3.6 1.9.7-4-2.9-2.8 4-.6L8 2.2Z" fill="currentColor"/>"#,
};

static ICON_CHECK: Icon = Icon {
    name: "waku-check",
    view: 16.0,
    body: r#"<path d="M3.8 8.2l2.8 2.8 5.6-6" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_SEARCH: Icon = Icon {
    name: "waku-modal-search",
    view: 16.0,
    body: r#"<circle cx="7.2" cy="7.2" r="4.3" stroke="currentColor" stroke-width="1.25" fill="none"/><path d="M10.5 10.5l3 3" stroke="currentColor" stroke-width="1.25" stroke-linecap="round"/>"#,
};

static ICON_LAYERS: Icon = Icon {
    name: "waku-layers",
    view: 16.0,
    body: r#"<path d="M8 2.5L13.5 5.5 8 8.5 2.5 5.5 8 2.5Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/><path d="M2.5 8.2L8 11.2l5.5-3M2.5 10.8L8 13.8l5.5-3" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static ICON_BRANCH: Icon = Icon {
    name: "waku-commit-branch",
    view: 16.0,
    body: r#"<circle cx="5" cy="4" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="5" cy="12" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="11" cy="6" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M5 5.4v5.2M11 7.4c0 2-1.5 3-6 3" stroke="currentColor" stroke-width="1.2" fill="none"/>"#,
};

static ICON_GIT_COMMIT: Icon = Icon {
    name: "waku-git-commit",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="3" stroke="currentColor" stroke-width="2" fill="none"/><path d="M3 12h6m6 0h6" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
};

static ICON_CLOUD_UPLOAD: Icon = Icon {
    name: "waku-cloud-upload",
    view: 24.0,
    body: r#"<path d="M12 13v8M8 17l4-4 4 4" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M20 16.58A5 5 0 0 0 18 7.26 7 7 0 0 0 4.33 8.3 4 4 0 0 0 6 16h1" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_PROVIDER_ANTHROPIC: Icon = Icon {
    name: "waku-prov-anthropic",
    view: 256.0,
    body: r#"<path fill="currentColor" d="m50.228 170.321 50.357-28.257.843-2.463-.843-1.361h-2.462l-8.426-.518-28.775-.778-24.952-1.037-24.175-1.296-6.092-1.297L0 125.796l.583-3.759 5.12-3.434 7.324.648 16.202 1.101 24.304 1.685 17.629 1.037 26.118 2.722h4.148l.583-1.685-1.426-1.037-1.101-1.037-25.147-17.045-27.22-18.017-14.258-10.37-7.713-5.25-3.888-4.925-1.685-10.758 7-7.713 9.397.649 2.398.648 9.527 7.323 20.35 15.75L94.817 91.9l3.889 3.24 1.555-1.102.195-.777-1.75-2.917-14.453-26.118-15.425-26.572-6.87-11.018-1.814-6.61c-.648-2.723-1.102-4.991-1.102-7.778l7.972-10.823L71.42 0 82.05 1.426l4.472 3.888 6.61 15.101 10.694 23.786 16.591 32.34 4.861 9.592 2.592 8.879.973 2.722h1.685v-1.556l1.36-18.211 2.528-22.36 2.463-28.776.843-8.1 4.018-9.722 7.971-5.25 6.222 2.981 5.12 7.324-.713 4.73-3.046 19.768-5.962 30.98-3.889 20.739h2.268l2.593-2.593 10.499-13.934 17.628-22.036 7.778-8.749 9.073-9.657 5.833-4.601h11.018l8.1 12.055-3.628 12.443-11.342 14.388-9.398 12.184-13.48 18.147-8.426 14.518.778 1.166 2.01-.194 30.46-6.481 16.462-2.982 19.637-3.37 8.88 4.148.971 4.213-3.5 8.62-20.998 5.184-24.628 4.926-36.682 8.685-.454.324.519.648 16.526 1.555 7.065.389h17.304l32.21 2.398 8.426 5.574 5.055 6.805-.843 5.184-12.962 6.611-17.498-4.148-40.83-9.721-14-3.5h-1.944v1.167l11.666 11.406 21.387 19.314 26.767 24.887 1.36 6.157-3.434 4.86-3.63-.518-23.526-17.693-9.073-7.972-20.545-17.304h-1.36v1.814l4.73 6.935 25.017 37.59 1.296 11.536-1.814 3.76-6.481 2.268-7.13-1.297-14.647-20.544-15.1-23.138-12.185-20.739-1.49.843-7.194 77.448-3.37 3.953-7.778 2.981-6.48-4.925-3.436-7.972 3.435-15.749 4.148-20.544 3.37-16.333 3.046-20.285 1.815-6.74-.13-.454-1.49.194-15.295 20.999-23.267 31.433-18.406 19.702-4.407 1.75-7.648-3.954.713-7.064 4.277-6.286 25.47-32.405 15.36-20.092 9.917-11.6-.065-1.686h-.583L44.07 198.125l-12.055 1.555-5.185-4.86.648-7.972 2.463-2.593 20.35-13.999-.064.065Z"/>"#,
};

static ICON_PROVIDER_OPENAI: Icon = Icon {
    name: "waku-prov-openai",
    view: 256.0,
    body: r#"<path fill="currentColor" d="M239.184 106.203a64.716 64.716 0 0 0-5.576-53.103C219.452 28.459 191 15.784 163.213 21.74A65.586 65.586 0 0 0 52.096 45.22a64.716 64.716 0 0 0-43.23 31.36c-14.31 24.602-11.061 55.634 8.033 76.74a64.665 64.665 0 0 0 5.525 53.102c14.174 24.65 42.644 37.324 70.446 31.36a64.72 64.72 0 0 0 48.754 21.744c28.481.025 53.714-18.361 62.414-45.481a64.767 64.767 0 0 0 43.229-31.36c14.137-24.558 10.875-55.423-8.083-76.483Zm-97.56 136.338a48.397 48.397 0 0 1-31.105-11.255l1.535-.87 51.67-29.825a8.595 8.595 0 0 0 4.247-7.367v-72.85l21.845 12.636c.218.111.37.32.409.563v60.367c-.056 26.818-21.783 48.545-48.601 48.601Zm-104.466-44.61a48.345 48.345 0 0 1-5.781-32.589l1.534.921 51.722 29.826a8.339 8.339 0 0 0 8.441 0l63.181-36.425v25.221a.87.87 0 0 1-.358.665l-52.335 30.184c-23.257 13.398-52.97 5.431-66.404-17.803ZM23.549 85.38a48.499 48.499 0 0 1 25.58-21.333v61.39a8.288 8.288 0 0 0 4.195 7.316l62.874 36.272-21.845 12.636a.819.819 0 0 1-.767 0L41.353 151.53c-23.211-13.454-31.171-43.144-17.804-66.405v.256Zm179.466 41.695-63.08-36.63L161.73 77.86a.819.819 0 0 1 .768 0l52.233 30.184a48.6 48.6 0 0 1-7.316 87.635v-61.391a8.544 8.544 0 0 0-4.4-7.213Zm21.742-32.69-1.535-.922-51.619-30.081a8.39 8.39 0 0 0-8.492 0L99.98 99.808V74.587a.716.716 0 0 1 .307-.665l52.233-30.133a48.652 48.652 0 0 1 72.236 50.391v.205ZM88.061 139.097l-21.845-12.585a.87.87 0 0 1-.41-.614V65.685a48.652 48.652 0 0 1 79.757-37.346l-1.535.87-51.67 29.825a8.595 8.595 0 0 0-4.246 7.367l-.051 72.697Zm11.868-25.58 28.138-16.217 28.188 16.218v32.434l-28.086 16.218-28.188-16.218-.052-32.434Z"/>"#,
};

static ICON_PROVIDER_GOOGLE: Icon = Icon {
    name: "waku-prov-google",
    view: 24.0,
    body: r#"<path fill="currentColor" d="M12 2.2c.5 4.4 5.4 9.3 9.8 9.8-4.4.5-9.3 5.4-9.8 9.8-.5-4.4-5.4-9.3-9.8-9.8 4.4-.5 9.3-5.4 9.8-9.8Z"/>"#,
};

static ICON_PROVIDER_PI: Icon = Icon {
    name: "waku-prov-pi",
    view: 16.0,
    body: r#"<path d="M3 4h10M6 4v7.2a1.2 1.2 0 0 1-1.2 1.2M10.5 4v6.8a1.4 1.4 0 0 0 2 .9" stroke="currentColor" stroke-width="1.35" stroke-linecap="round" fill="none"/>"#,
};

static ICON_PLUS: Icon = Icon {
    name: "waku-modal-plus",
    view: 16.0,
    body: r#"<path d="M8 3.2v9.6M3.2 8h9.6" stroke="currentColor" stroke-width="1.45" stroke-linecap="round"/>"#,
};

static ICON_FOLDER_NEW: Icon = Icon {
    name: "waku-folder-new",
    view: 56.0,
    body: r#"<path fill="currentColor" d="M45.556 30.91C51.21 30.91 56 26.18 56 20.466s-4.728-10.443-10.444-10.443c-5.735 0-10.443 4.728-10.443 10.443c0 5.756 4.708 10.444 10.443 10.444M0 42.196c0 4.255 2.18 6.373 6.455 6.373h34.928c3.68 0 5.818-2.138 5.818-6.373v-8.573a11 11 0 0 1-1.645.123c-.575 0-1.11-.04-1.665-.102v8.408c0 2.117-1.15 3.207-3.186 3.207H6.517c-2.056 0-3.207-1.09-3.207-3.228v-16.96h29.788a11.4 11.4 0 0 1-.699-3.104H3.31v-5.18c0-2.036 1.09-3.064 3.043-3.064h4.605c1.357 0 2.18.329 3.228 1.233l1.254 1.028c1.336 1.11 2.426 1.522 4.461 1.522h12.746a12 12 0 0 1 1.234-3.31H20.764c-1.398 0-2.241-.35-3.27-1.213l-1.253-1.049c-1.357-1.13-2.405-1.52-4.44-1.52H5.673C2.076 10.413 0 12.468 0 16.621ZM45.556 27.25c-.72 0-1.336-.493-1.336-1.254V21.72h-3.947a1.273 1.273 0 0 1-1.254-1.254c0-.678.576-1.254 1.254-1.254h3.947v-4.255c0-.782.617-1.275 1.336-1.275s1.337.493 1.337 1.274v4.256h3.947c.678 0 1.254.576 1.254 1.254s-.576 1.254-1.254 1.254h-3.947v4.276c0 .76-.617 1.254-1.337 1.254"/>"#,
};

static ICON_CLOSE_X: Icon = Icon {
    name: "waku-close-x",
    view: 24.0,
    body: r#"<path d="m6 6 12 12M18 6 6 18" stroke="currentColor" stroke-width="2" stroke-linecap="round" fill="none"/>"#,
};

static ICON_WORKSPACE_LAPTOP: Icon = Icon {
    name: "waku-workspace-laptop",
    view: 24.0,
    body: r#"<rect width="18" height="12" x="3" y="4" rx="2" ry="2" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><line x1="2" x2="22" y1="20" y2="20" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static ICON_WORKSPACE_FORK: Icon = Icon {
    name: "waku-workspace-fork",
    view: 24.0,
    body: r#"<path d="M2 12h9L21 2" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M13 2h8v8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="m13 14 8 8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M13 22h8v-8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_CMD_PENCIL: Icon = Icon {
    name: "waku-cmd-pencil",
    view: 24.0,
    body: r#"<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="m15 5 4 4" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_CMD_PANEL_LEFT: Icon = Icon {
    name: "waku-cmd-panel-left",
    view: 24.0,
    body: r#"<rect width="18" height="18" x="3" y="3" rx="2" stroke="currentColor" stroke-width="2" fill="none"/><path d="M9 3v18" stroke="currentColor" stroke-width="2"/>"#,
};

static ICON_CMD_PANEL_RIGHT: Icon = Icon {
    name: "waku-cmd-panel-right",
    view: 24.0,
    body: r#"<rect width="18" height="18" x="3" y="3" rx="2" stroke="currentColor" stroke-width="2" fill="none"/><path d="M15 3v18" stroke="currentColor" stroke-width="2"/>"#,
};

static ICON_CMD_TERMINAL: Icon = Icon {
    name: "waku-cmd-terminal",
    view: 24.0,
    body: r#"<polyline points="4 17 10 11 4 5" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><line x1="12" x2="20" y1="19" y2="19" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
};

static ICON_CMD_FOLDER: Icon = Icon {
    name: "waku-cmd-folder",
    view: 16.0,
    body: r#"<path d="M2.2 4.2a1.2 1.2 0 0 1 1.2-1.2h3.1l1.2 1.3h4.9a1.2 1.2 0 0 1 1.2 1.2v6.3a1.2 1.2 0 0 1-1.2 1.2H3.4a1.2 1.2 0 0 1-1.2-1.2V4.2Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/>"#,
};

static ICON_CMD_SETTINGS: Icon = Icon {
    name: "waku-cmd-settings",
    view: 24.0,
    body: r#"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.38a2 2 0 0 0-.73-2.73l-.15-.09a2 2 0 0 1-1-1.74v-.51a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><circle cx="12" cy="12" r="3" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_CMD_APPEARANCE: Icon = Icon {
    name: "waku-cmd-appearance",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="4" stroke="currentColor" stroke-width="2" fill="none"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
};

pub(crate) fn provider_mark_view(provider: &str, size: f32, pal: &WakuPalette, active: bool) -> El {
    let lower = provider.to_ascii_lowercase();
    let (ic, base_color) = if lower.contains("anthropic") || lower.contains("claude") {
        (
            &ICON_PROVIDER_ANTHROPIC,
            Color {
                r: 0.85,
                g: 0.47,
                b: 0.34,
                a: 1.0,
            },
        )
    } else if lower.contains("openai") || lower.contains("codex") {
        (&ICON_PROVIDER_OPENAI, pal.text)
    } else if lower.contains("google") || lower.contains("gemini") {
        (
            &ICON_PROVIDER_GOOGLE,
            Color {
                r: 0.30,
                g: 0.42,
                b: 1.0,
                a: 1.0,
            },
        )
    } else {
        (&ICON_PROVIDER_PI, pal.text_secondary)
    };
    let color = Color {
        a: if active { 1.0 } else { 0.82 },
        ..base_color
    };
    icon(ic, size).color(color)
}

#[derive(Clone)]
struct PaletteCommand {
    title: &'static str,
    icon: &'static Icon,
    shortcut: &'static str,
    action: PiGuiAction,
}

fn palette_commands(is_dark: bool) -> Vec<PaletteCommand> {
    vec![
        PaletteCommand {
            title: "New Task",
            icon: &ICON_CMD_PENCIL,
            shortcut: if cfg!(target_os = "macos") { "⌘N" } else { "Ctrl+N" },
            action: PiGuiAction::NewThread,
        },
        PaletteCommand {
            title: "Toggle Sidebar",
            icon: &ICON_CMD_PANEL_LEFT,
            shortcut: if cfg!(target_os = "macos") { "⌘B" } else { "Ctrl+B" },
            action: PiGuiAction::ToggleSidebar,
        },
        PaletteCommand {
            title: "Toggle Right Panel",
            icon: &ICON_CMD_PANEL_RIGHT,
            shortcut: if cfg!(target_os = "macos") {
                "⌘⇧I"
            } else {
                "Ctrl+Shift+I"
            },
            action: PiGuiAction::ToggleInspector,
        },
        PaletteCommand {
            title: "Changes",
            icon: &ICON_GIT_COMMIT,
            shortcut: "",
            action: PiGuiAction::SelectInspectorTab(InspectorTab::Changes),
        },
        PaletteCommand {
            title: "Files",
            icon: &ICON_CMD_FOLDER,
            shortcut: "",
            action: PiGuiAction::SelectInspectorTab(InspectorTab::Files),
        },
        PaletteCommand {
            title: "Terminal",
            icon: &ICON_CMD_TERMINAL,
            shortcut: "",
            action: PiGuiAction::SelectInspectorTab(InspectorTab::Terminal),
        },
        PaletteCommand {
            title: "Settings",
            icon: &ICON_CMD_SETTINGS,
            shortcut: if cfg!(target_os = "macos") { "⌘," } else { "Ctrl+," },
            action: PiGuiAction::OpenSettings(SettingsPage::General),
        },
        PaletteCommand {
            title: if is_dark {
                "Switch to Light Theme"
            } else {
                "Switch to Dark Theme"
            },
            icon: &ICON_CMD_APPEARANCE,
            shortcut: "",
            action: PiGuiAction::SetThemePreference(if is_dark {
                ThemePreference::Light
            } else {
                ThemePreference::Dark
            }),
        },
        PaletteCommand {
            title: "Compact Context",
            icon: &ICON_LAYERS,
            shortcut: "",
            action: PiGuiAction::CompactSession,
        },
    ]
}

fn render_popover_menu_row_with_icon(
    key: String,
    role: &'static str,
    label: &str,
    leading: Option<(&'static Icon, Color)>,
    selected: bool,
    trailing: Option<&str>,
    pal: &WakuPalette,
    on_press: impl Fn(&mut PiGuiState) + 'static,
) -> El {
    let mut r = row()
        .key(key)
        .role(role)
        .label(label)
        .w_full()
        .h(26.0)
        .px(8.0)
        .gap(8.0)
        .justify_start()
        .center_y()
        .radius(WakuRadius::SM)
        .hover(pal.overlay_strong)
        .cursor(Cursor::Pointer)
        .on_press(on_press);
    if let Some((ic, ic_color)) = leading {
        r = r.child(icon(ic, 12.0).color(ic_color).shrink());
    }
    r = r.child(
        text(label)
            .font(TextStyle::new(13.0, if selected { 500.0 } else { 400.0 }))
            .color(if selected || role == "button" {
                pal.text
            } else {
                pal.text_secondary
            })
            .truncate()
            .grow(1.0)
            .min_w(0.0),
    );
    if selected {
        r = r.child(icon(&ICON_CHECK, 12.0).color(pal.text_secondary).shrink());
    } else if let Some(t) = trailing {
        r = r.child(
            text(t)
                .font(TextStyle::new(11.5, 400.0))
                .color(pal.text_tertiary)
                .shrink(),
        );
    }
    r
}

fn render_popover_menu_row(
    key: String,
    role: &'static str,
    label: &str,
    selected: bool,
    trailing: Option<&str>,
    pal: &WakuPalette,
    on_press: impl Fn(&mut PiGuiState) + 'static,
) -> El {
    render_popover_menu_row_with_icon(key, role, label, None, selected, trailing, pal, on_press)
}

fn render_sidebar_options_popover(s: &PiGuiState, pal: &WakuPalette) -> El {
    col()
        .key("popover.sidebar_options")
        .role("menu")
        .w(200.0)
        .padding(4.0)
        .gap(2.0)
        .radius(WakuRadius::LG)
        .fill(pal.raised)
        .border_layout()
        .border(1.0, pal.border_strong)
        .clip_rounded(WakuRadius::LG)
        .on_press(|_: &mut PiGuiState| {})
        .child(
            row()
                .key("popover.sidebar_options.group_by")
                .role("menuitem")
                .label_static("Group by")
                .w_full()
                .h(26.0)
                .px(8.0)
                .justify_between()
                .center_y()
                .child(
                    text("Group by")
                        .font(TextStyle::new(11.5, 500.0))
                        .color(pal.text_tertiary),
                ),
        )
        .child(render_popover_menu_row(
            "popover.sidebar_options.updated".to_string(),
            "menuitem",
            "Updated",
            s.sidebar_grouping == SidebarGrouping::Updated,
            None,
            pal,
            |st| st.dispatch(PiGuiAction::SetSidebarGrouping(SidebarGrouping::Updated), None),
        ))
        .child(render_popover_menu_row(
            "popover.sidebar_options.project".to_string(),
            "menuitem",
            "Project",
            s.sidebar_grouping == SidebarGrouping::Project,
            None,
            pal,
            |st| st.dispatch(PiGuiAction::SetSidebarGrouping(SidebarGrouping::Project), None),
        ))
        .child(
            row()
                .key("popover.sidebar_options.order_by")
                .role("menuitem")
                .label_static("Order by")
                .w_full()
                .h(26.0)
                .px(8.0)
                .justify_between()
                .center_y()
                .child(
                    text("Order by")
                        .font(TextStyle::new(11.5, 500.0))
                        .color(pal.text_tertiary),
                ),
        )
        .child(render_popover_menu_row(
            "popover.sidebar_options.newest".to_string(),
            "menuitem",
            "Newest",
            s.sidebar_ordering == SidebarOrdering::Newest,
            None,
            pal,
            |st| st.dispatch(PiGuiAction::SetSidebarOrdering(SidebarOrdering::Newest), None),
        ))
        .child(render_popover_menu_row(
            "popover.sidebar_options.oldest".to_string(),
            "menuitem",
            "Oldest",
            s.sidebar_ordering == SidebarOrdering::Oldest,
            None,
            pal,
            |st| st.dispatch(PiGuiAction::SetSidebarOrdering(SidebarOrdering::Oldest), None),
        ))
        .anchored("workspace.sidebar.options", Side::Bottom, Align::Start, 6.0)
        .anchored_round()
}

fn render_model_picker_popover(s: &PiGuiState, pal: &WakuPalette) -> El {
    let fav_active = matches!(s.model_picker_tab, ModelPickerTab::Favorites);
    let active_prov = s.active_model_picker_provider();

    let mut providers: Vec<String> = Vec::new();
    for m in &s.models {
        if !providers.contains(&m.provider) {
            providers.push(m.provider.clone());
        }
    }

    let mut rail = col()
        .key("popover.model_picker.rail")
        .role("toolbar")
        .label_static("Model Provider Rail")
        .w(50.0)
        .h_full()
        .shrink()
        .padding(5.0)
        .gap(4.0)
        .items_center()
        .border_layout()
        .border_sides([0.0, 1.0, 0.0, 0.0], pal.border)
        .child(
            el().key("popover.model_picker.rail.favorites")
                .role("button")
                .label_static("Filter Favorite Models")
                .size(38.0, 38.0)
                .center()
                .radius(8.0)
                .fill(if fav_active {
                    pal.overlay_strong
                } else {
                    Color::TRANSPARENT
                })
                .hover(pal.overlay)
                .cursor(Cursor::Pointer)
                .on_press(|st: &mut PiGuiState| {
                    let next = if matches!(st.model_picker_tab, ModelPickerTab::Favorites) {
                        ModelPickerTab::Provider(st.active_model_picker_provider())
                    } else {
                        ModelPickerTab::Favorites
                    };
                    st.dispatch(PiGuiAction::SelectModelTab(next), None);
                })
                .child(
                    icon(if fav_active { &ICON_STAR_FILLED } else { &ICON_STAR }, 16.0).color(if fav_active {
                        pal.text
                    } else {
                        pal.text_secondary
                    }),
                ),
        )
        .child(el().size(34.0, 1.0).my(2.0).fill(pal.border));

    for prov in &providers {
        let active = matches!(&s.model_picker_tab, ModelPickerTab::Provider(p) if p == prov);
        let owned_prov = prov.clone();
        rail = rail.child(
            el().key(format!("model-rail-{prov}"))
                .role("button")
                .label(prov)
                .size(38.0, 38.0)
                .center()
                .radius(8.0)
                .fill(if active { pal.overlay_strong } else { Color::TRANSPARENT })
                .hover(pal.overlay)
                .cursor(Cursor::Pointer)
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(
                        PiGuiAction::SelectModelTab(ModelPickerTab::Provider(owned_prov.clone())),
                        None,
                    );
                })
                .child(provider_mark_view(prov, 17.0, pal, active)),
        );
    }

    let search_bar = row()
        .key("popover.model_picker.search")
        .role("textbox")
        .label_static("Search Models")
        .mt(10.0)
        .mx(12.0)
        .mb(8.0)
        .h(34.0)
        .px(10.0)
        .gap(8.0)
        .center_y()
        .radius(8.0)
        .fill(pal.inset)
        .border_layout()
        .border(1.0, pal.border)
        .child(icon(&ICON_SEARCH, 14.0).color(pal.text_tertiary))
        .child(
            text_input(&s.model_search_edit, TextStyle::new(13.0, 400.0))
                .label("Search models")
                .single_line()
                .grow(1.0)
                .min_w(0.0)
                .color(pal.text)
                .placeholder("Search models…", pal.text_ghost)
                .caret(pal.accent, pal.selection)
                .on_edit(|st: &mut PiGuiState| &mut st.model_search_edit)
                .on_input_key(|st: &mut PiGuiState, ev, _tx| {
                    if ev.key == Key::Escape && !st.model_search_edit.text.is_empty() {
                        st.model_search_edit.clear();
                        return true;
                    }
                    false
                }),
        );

    let q = s.model_search_edit.text.trim().to_ascii_lowercase();
    let mut list = col()
        .key("popover.model_picker.list")
        .role("list")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .padding(9.0)
        .gap(2.0)
        .scroll(rune::ScrollSpec { stick_bottom: false });

    for model in &s.models {
        let fav_key = format!("{}/{}", model.provider, model.id);
        let is_fav = s.favorite_models.contains(&fav_key);
        let matches_tab = match &s.model_picker_tab {
            ModelPickerTab::Favorites => is_fav,
            ModelPickerTab::Provider(p) => !q.is_empty() || &model.provider == p,
            ModelPickerTab::All => !q.is_empty() || model.provider == active_prov,
        };
        let matches_q = q.is_empty()
            || model.name.to_ascii_lowercase().contains(&q)
            || model.id.to_ascii_lowercase().contains(&q)
            || model.provider.to_ascii_lowercase().contains(&q);
        if !matches_tab || !matches_q {
            continue;
        }
        let is_active = s
            .active_model
            .as_ref()
            .is_some_and(|m| m.provider == model.provider && m.id == model.id);
        let prov = model.provider.clone();
        let mid = model.id.clone();
        let fk_btn = fav_key.clone();

        list = list.child(
            row()
                .key(format!("model-row-{fav_key}"))
                .role("button")
                .w_full()
                .h(34.0)
                .px(8.0)
                .gap(8.0)
                .justify_between()
                .center_y()
                .radius(WakuRadius::SM)
                .fill(if is_active { pal.overlay } else { Color::TRANSPARENT })
                .hover(pal.overlay_strong)
                .cursor(Cursor::Pointer)
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(
                        PiGuiAction::SelectModel {
                            provider: prov.clone(),
                            model_id: mid.clone(),
                        },
                        None,
                    );
                })
                .child(
                    row()
                        .gap(7.0)
                        .center_y()
                        .min_w(0.0)
                        .grow(1.0)
                        .child(provider_mark_view(&model.provider, 12.0, pal, true).shrink())
                        .child(
                            text(&model.name)
                                .font(TextStyle::new(13.0, if is_active { 500.0 } else { 400.0 }))
                                .color(pal.text)
                                .truncate(),
                        )
                        .child(
                            text(&model.provider)
                                .font(TextStyle::new(11.5, 400.0))
                                .color(pal.text_tertiary),
                        ),
                )
                .child(
                    el().size(22.0, 22.0)
                        .center()
                        .radius(4.0)
                        .hover(pal.overlay_strong)
                        .on_press(move |st: &mut PiGuiState| {
                            st.dispatch(PiGuiAction::ToggleFavoriteModel(fk_btn.clone()), None);
                        })
                        .child(
                            icon(if is_fav { &ICON_STAR_FILLED } else { &ICON_STAR }, 13.0).color(if is_fav {
                                pal.favorite
                            } else {
                                pal.text_ghost
                            }),
                        ),
                ),
        );
    }

    row()
        .key("popover.model_picker")
        .role("dialog")
        .label_static("Model Picker Popover")
        .size(460.0, 390.0)
        .radius(WakuRadius::LG)
        .fill(pal.canvas)
        .border_layout()
        .border(1.0, pal.border_strong)
        .clip_rounded(WakuRadius::LG)
        .on_press(|_: &mut PiGuiState| {})
        .child(rail)
        .child(col().grow(1.0).min_w(0.0).h_full().child(search_bar).child(list))
        .anchored("workspace.main.composer.model_chip", Side::Top, Align::Start, 5.0)
        .anchored_round()
}

fn render_thinking_picker_popover(s: &PiGuiState, pal: &WakuPalette) -> El {
    let levels = [
        ("off", "Off"),
        ("low", "Low"),
        ("medium", "Medium"),
        ("high", "High"),
        ("xhigh", "Max"),
    ];
    let mut menu = col()
        .key("popover.thinking_picker")
        .role("menu")
        .w(200.0)
        .padding(6.0)
        .gap(2.0)
        .radius(WakuRadius::LG)
        .fill(pal.raised)
        .border_layout()
        .border(1.0, pal.border_strong)
        .on_press(|_: &mut PiGuiState| {})
        .child(
            row()
                .key("popover.thinking_picker.header")
                .role("heading")
                .label_static("Reasoning")
                .w_full()
                .h(24.0)
                .px(8.0)
                .center_y()
                .child(
                    text("Reasoning")
                        .font(TextStyle::new(11.5, 500.0))
                        .color(pal.text_tertiary),
                ),
        );

    for (id, label) in levels {
        let selected = s.context_usage.thinking_level == id;
        let owned = id.to_string();
        let key = if id == "medium" {
            "popover.thinking_picker.option.medium".to_string()
        } else {
            format!("thinking-opt-{id}")
        };
        menu = menu.child(render_popover_menu_row(
            key,
            "menuitem",
            label,
            selected,
            None,
            pal,
            move |st| st.dispatch(PiGuiAction::SetThinkingLevel(owned.clone()), None),
        ));
    }

    menu.anchored("workspace.main.composer.thinking_chip", Side::Top, Align::Start, 6.0)
        .anchored_round()
}

fn render_project_picker_popover(s: &PiGuiState, pal: &WakuPalette) -> El {
    let dlg = col()
        .key("popover.project_picker")
        .role("dialog")
        .label_static("Project Picker Popover")
        .w(300.0)
        .pb(4.0)
        .radius(WakuRadius::LG)
        .fill(pal.raised)
        .border_layout()
        .border(1.0, pal.border_strong)
        .on_press(|_: &mut PiGuiState| {})
        .child(
            row()
                .key("popover.project_picker.search")
                .role("textbox")
                .label_static("Search projects")
                .mt(10.0)
                .mx(12.0)
                .mb(8.0)
                .h(28.0)
                .px(10.0)
                .gap(8.0)
                .center_y()
                .radius(9.0)
                .fill(pal.surface)
                .child(icon(&ICON_SEARCH, 12.0).color(pal.text_secondary))
                .child(
                    text_input(&s.project_search_edit, TextStyle::new(13.0, 400.0))
                        .label("Search projects")
                        .single_line()
                        .grow(1.0)
                        .min_w(0.0)
                        .color(pal.text)
                        .placeholder("Search projects…", pal.text_ghost)
                        .on_edit(|st: &mut PiGuiState| &mut st.project_search_edit),
                ),
        );

    let q = s.project_search_edit.text.trim().to_ascii_lowercase();
    let filtered: Vec<(usize, &String)> = s
        .projects
        .iter()
        .enumerate()
        .filter(|(_, proj)| q.is_empty() || proj.to_ascii_lowercase().contains(&q))
        .collect();

    let rows_section = if filtered.is_empty() {
        row().w_full().h(64.0).center().child(
            text("No projects found")
                .font(TextStyle::new(12.5, 400.0))
                .color(pal.text_ghost),
        )
    } else {
        let mut rows_col = col().w_full().px(4.0).gap(2.0);
        if filtered.len() > 11 {
            rows_col = rows_col
                .max_h(11.0 * 26.0 + 10.0 * 2.0)
                .scroll(rune::ScrollSpec { stick_bottom: false });
        }
        for (idx, proj) in filtered {
            let owned = proj.clone();
            let active = s.selected_project == *proj;
            rows_col = rows_col.child(render_popover_menu_row(
                format!("project-picker-item-{idx}"),
                "button",
                proj,
                active,
                None,
                pal,
                move |st| st.dispatch(PiGuiAction::SelectProject(owned.clone()), None),
            ));
        }
        rows_col
    };

    dlg.child(rows_section)
        .child(el().mx(6.0).my(4.0).h(1.0).fill(pal.border))
        .child(
            col()
                .w_full()
                .px(4.0)
                .child(render_popover_menu_row_with_icon(
                    "popover.project_picker.new_project".to_string(),
                    "button",
                    "New Project…",
                    Some((&ICON_FOLDER_NEW, pal.text_secondary)),
                    false,
                    None,
                    pal,
                    |st| {
                        st.push_toast("Choose a workspace folder via --cwd", ToastTone::Info);
                        st.dispatch(PiGuiAction::ClosePopover, None);
                    },
                ))
                .child(render_popover_menu_row_with_icon(
                    "popover.project_picker.no_project".to_string(),
                    "button",
                    "No Project",
                    Some((&ICON_CLOSE_X, pal.text_secondary)),
                    false,
                    None,
                    pal,
                    |st| st.dispatch(PiGuiAction::SelectProject("pi".to_string()), None),
                )),
        )
        .anchored("workspace.main.footer.project_chip", Side::Top, Align::Start, 6.0)
        .anchored_round()
}

fn render_workspace_picker_popover(s: &PiGuiState, pal: &WakuPalette) -> El {
    col()
        .key("popover.workspace_picker")
        .role("menu")
        .w(190.0)
        .padding(6.0)
        .gap(2.0)
        .radius(WakuRadius::LG)
        .fill(pal.raised)
        .border_layout()
        .border(1.0, pal.border_strong)
        .on_press(|_: &mut PiGuiState| {})
        .child(
            row().w_full().h(21.0).px(8.0).center_y().child(
                text("Work in")
                    .font(TextStyle::new(11.5, 500.0))
                    .color(pal.text_tertiary),
            ),
        )
        .child(render_popover_menu_row_with_icon(
            "popover.workspace_picker.local".to_string(),
            "menuitem",
            "Local",
            Some((
                &ICON_WORKSPACE_LAPTOP,
                if s.workspace_mode == WorkspaceMode::Local {
                    pal.text
                } else {
                    pal.text_secondary
                },
            )),
            s.workspace_mode == WorkspaceMode::Local,
            None,
            pal,
            |st| st.dispatch(PiGuiAction::SelectWorkspaceMode(WorkspaceMode::Local), None),
        ))
        .child(render_popover_menu_row_with_icon(
            "popover.workspace_picker.new_worktree".to_string(),
            "menuitem",
            "New worktree",
            Some((
                &ICON_WORKSPACE_FORK,
                if s.workspace_mode == WorkspaceMode::NewWorktree {
                    pal.text
                } else {
                    pal.text_secondary
                },
            )),
            s.workspace_mode == WorkspaceMode::NewWorktree,
            None,
            pal,
            |st| st.dispatch(PiGuiAction::SelectWorkspaceMode(WorkspaceMode::NewWorktree), None),
        ))
        .anchored("workspace.main.footer.workspace_chip", Side::Top, Align::Start, 6.0)
        .anchored_round()
}

fn render_branch_picker_popover(s: &PiGuiState, pal: &WakuPalette) -> El {
    if s.branch_create_mode {
        return col()
            .key("popover.branch_picker")
            .role("dialog")
            .label_static("Branch Picker Popover")
            .w(300.0)
            .padding(14.0)
            .radius(WakuRadius::LG)
            .fill(pal.raised)
            .border_layout()
            .border(1.0, pal.border_strong)
            .on_press(|_: &mut PiGuiState| {})
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .center_y()
                    .child(icon(&ICON_PLUS, 14.0).color(pal.text_secondary))
                    .child(
                        text("Create and checkout new branch")
                            .font(TextStyle::new(13.0, 500.0))
                            .color(pal.text),
                    ),
            )
            .child(
                row()
                    .key("popover.branch_picker.create_input")
                    .role("textbox")
                    .label_static("New branch name")
                    .mt(12.0)
                    .w_full()
                    .h(36.0)
                    .px(10.0)
                    .gap(8.0)
                    .center_y()
                    .radius(9.0)
                    .fill(pal.surface)
                    .border_layout()
                    .border(1.0, pal.border_strong)
                    .child(
                        text_input(&s.branch_create_edit, TextStyle::new(13.0, 400.0))
                            .label("New branch name")
                            .single_line()
                            .grow(1.0)
                            .min_w(0.0)
                            .color(pal.text)
                            .placeholder("feat/new-branch", pal.text_ghost)
                            .caret(pal.accent, pal.selection)
                            .on_edit(|st: &mut PiGuiState| &mut st.branch_create_edit)
                            .on_submit(|st: &mut PiGuiState, _tx| {
                                let name = st.branch_create_edit.text.clone();
                                st.dispatch(PiGuiAction::CreateBranch(name), None);
                            })
                            .on_input_key(|st: &mut PiGuiState, ev, _tx| {
                                if ev.key == Key::Escape {
                                    st.dispatch(PiGuiAction::SetBranchCreateMode(false), None);
                                    return true;
                                }
                                false
                            }),
                    ),
            )
            .child(
                text("Press Return to create · Esc to cancel")
                    .mt(9.0)
                    .font(TextStyle::new(12.5, 400.0))
                    .color(pal.text_tertiary),
            )
            .anchored("workspace.main.footer.branch_chip", Side::Top, Align::Start, 6.0)
            .anchored_round();
    }

    let mut dlg = col()
        .key("popover.branch_picker")
        .role("dialog")
        .label_static("Branch Picker Popover")
        .w(300.0)
        .pb(4.0)
        .radius(WakuRadius::LG)
        .fill(pal.raised)
        .border_layout()
        .border(1.0, pal.border_strong)
        .on_press(|_: &mut PiGuiState| {})
        .child(
            row()
                .key("popover.branch_picker.search")
                .role("textbox")
                .label_static("Search branches")
                .mt(6.0)
                .mx(11.0)
                .mb(2.0)
                .h(28.0)
                .px(1.0)
                .gap(8.0)
                .center_y()
                .child(icon(&ICON_SEARCH, 13.0).color(pal.text_tertiary))
                .child(
                    text_input(&s.branch_search_edit, TextStyle::new(13.0, 400.0))
                        .label("Search branches")
                        .single_line()
                        .grow(1.0)
                        .min_w(0.0)
                        .color(pal.text)
                        .placeholder("Search branches…", pal.text_ghost)
                        .on_edit(|st: &mut PiGuiState| &mut st.branch_search_edit),
                ),
        )
        .child(el().w_full().h(1.0).fill(pal.border))
        .child(
            row().w_full().h(20.0).px(12.0).pt(2.0).center_y().child(
                text("Branches")
                    .font(TextStyle::new(12.0, 500.0))
                    .color(pal.text_tertiary),
            ),
        );

    let q = s.branch_search_edit.text.trim().to_ascii_lowercase();
    for (idx, branch) in s.branches.iter().enumerate() {
        if !q.is_empty() && !branch.to_ascii_lowercase().contains(&q) {
            continue;
        }
        let owned = branch.clone();
        let active = s.selected_branch == *branch;
        let key = if idx == 0 {
            "popover.branch_picker.row.0".to_string()
        } else {
            format!("branch-item-{idx}")
        };
        dlg = dlg.child(
            render_popover_menu_row_with_icon(
                key,
                "button",
                branch,
                Some((&ICON_BRANCH, if active { pal.text } else { pal.text_secondary })),
                active,
                None,
                pal,
                move |st| st.dispatch(PiGuiAction::SelectBranch(owned.clone()), None),
            )
            .mx(4.0),
        );
    }

    dlg.child(el().w_full().h(1.0).my(3.0).fill(pal.border))
        .child(
            render_popover_menu_row_with_icon(
                "branch-create-btn".to_string(),
                "button",
                "Create and checkout new branch…",
                Some((&ICON_PLUS, pal.text_secondary)),
                false,
                None,
                pal,
                |st| st.dispatch(PiGuiAction::SetBranchCreateMode(true), None),
            )
            .mx(4.0),
        )
        .anchored("workspace.main.footer.branch_chip", Side::Top, Align::Start, 6.0)
        .anchored_round()
}

fn render_context_usage_popover(s: &PiGuiState, pal: &WakuPalette) -> El {
    let pct = s.context_usage.percent.clamp(0.0, 100.0);
    let track_w = 292.0;
    let fill_w = (track_w * (pct as f32 / 100.0)).max(if pct > 0.0 { 4.0 } else { 0.0 });
    col()
        .key("popover.context_usage")
        .role("dialog")
        .label_static("Context Window Popover")
        .w(320.0)
        .padding(14.0)
        .gap(12.0)
        .radius(WakuRadius::LG)
        .fill(pal.raised)
        .border_layout()
        .border(1.0, pal.border_strong)
        .on_press(|_: &mut PiGuiState| {})
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(
                    text("Context Window")
                        .key("popover.context_usage.title")
                        .role("heading")
                        .font(TextStyle::new(13.5, 500.0))
                        .color(pal.text),
                )
                .child(
                    text(format!("{pct:.1}%"))
                        .font(TextStyle::new(12.5, 500.0).mono())
                        .color(pal.gauge),
                ),
        )
        .child(
            el().key("popover.context_usage.bar")
                .role("region")
                .w_full()
                .h(3.0)
                .radius(2.0)
                .fill(pal.inset)
                .child(el().w(fill_w).h(3.0).radius(2.0).fill(pal.gauge)),
        )
        .child(
            text(format!(
                "{} / {} tokens · ${:.4}",
                s.context_usage.used_tokens, s.context_usage.window_tokens, s.context_usage.session_usage.total_cost
            ))
            .font(TextStyle::new(12.0, 400.0).mono())
            .color(pal.text_secondary),
        )
        .anchored("workspace.main.footer.context_gauge", Side::Top, Align::End, 6.0)
        .anchored_round()
}

/// Renders the active anchored popover (with transparent click-away scrim) when `s.popover.is_some()`.
pub fn view_popover(s: &PiGuiState, _k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> Option<El> {
    let pop = s.popover?;
    let card = match pop {
        PopoverKind::SidebarOptions => render_sidebar_options_popover(s, pal),
        PopoverKind::ModelPicker => render_model_picker_popover(s, pal),
        PopoverKind::ThinkingPicker => render_thinking_picker_popover(s, pal),
        PopoverKind::ProjectPicker => render_project_picker_popover(s, pal),
        PopoverKind::WorkspacePicker => render_workspace_picker_popover(s, pal),
        PopoverKind::BranchPicker => render_branch_picker_popover(s, pal),
        PopoverKind::ContextUsage => render_context_usage_popover(s, pal),
    };

    Some(
        el().key("popover-backdrop")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ClosePopover, None))
            .child(card),
    )
}

fn render_command_palette(s: &PiGuiState, pal: &WakuPalette) -> El {
    let q = s.palette_edit.text.trim().to_ascii_lowercase();
    let cmds = palette_commands(pal.is_dark);
    let filtered_cmds: Vec<PaletteCommand> = cmds
        .into_iter()
        .filter(|c| q.is_empty() || c.title.to_ascii_lowercase().contains(&q))
        .collect();
    let filtered_threads: Vec<_> = s
        .threads
        .iter()
        .filter(|t| q.is_empty() || t.title.to_ascii_lowercase().contains(&q))
        .take(5)
        .collect();

    let is_empty = filtered_cmds.is_empty() && filtered_threads.is_empty();

    let mut results = col()
        .key("command_palette.results")
        .role("list")
        .w_full()
        .px(8.0)
        .pb(if is_empty { 0.0 } else { 8.0 })
        .gap(0.0)
        .scroll(rune::ScrollSpec { stick_bottom: false });

    if is_empty {
        results = results.child(
            col()
                .key("command_palette.empty")
                .role("region")
                .label_static("No matching tasks or commands")
                .w_full()
                .h(180.0)
                .gap(12.0)
                .center()
                .child(icon(&ICON_SEARCH, 18.0).color(pal.text_ghost))
                .child(
                    text("No matching tasks or commands")
                        .font(TextStyle::new(13.0, 500.0))
                        .color(pal.text_secondary),
                ),
        );
    } else {
        results = results.grow(1.0).min_h(0.0);
        if !filtered_cmds.is_empty() {
            results = results.child(
                row()
                    .key("command_palette.section.0")
                    .role("heading")
                    .label_static("Commands")
                    .w_full()
                    .h(30.0)
                    .pt(10.0)
                    .px(9.0)
                    .center_y()
                    .child(
                        text("Commands")
                            .font(TextStyle::new(11.5, 500.0))
                            .color(pal.text_tertiary),
                    ),
            );
            for (idx, cmd) in filtered_cmds.into_iter().enumerate() {
                let highlighted = idx == 0;
                let act = cmd.action.clone();
                let row_key = if idx == 0 {
                    "command_palette.row.0".to_string()
                } else {
                    format!("palette-row-{idx}")
                };
                let mut cmd_row = row()
                    .key(row_key)
                    .role("button")
                    .label(cmd.title)
                    .w_full()
                    .h(44.0)
                    .px(11.0)
                    .gap(10.0)
                    .justify_start()
                    .center_y()
                    .radius(9.0)
                    .fill(if highlighted {
                        pal.overlay_strong
                    } else {
                        Color::TRANSPARENT
                    })
                    .border_layout()
                    .border(
                        1.0,
                        if highlighted {
                            pal.border_strong
                        } else {
                            Color::TRANSPARENT
                        },
                    )
                    .hover(pal.overlay)
                    .cursor(Cursor::Pointer)
                    .on_press(move |st: &mut PiGuiState| {
                        st.dispatch(PiGuiAction::CloseOverlay, None);
                        st.dispatch(act.clone(), None);
                    })
                    .child(
                        el().size(20.0, 20.0)
                            .shrink()
                            .center()
                            .child(icon(cmd.icon, 16.0).color(pal.text_secondary)),
                    )
                    .child(
                        text(cmd.title)
                            .font(TextStyle::new(14.0, if highlighted { 500.0 } else { 400.0 }))
                            .color(if highlighted { pal.text } else { pal.text_secondary }),
                    )
                    .child(el().grow(1.0));
                if !cmd.shortcut.is_empty() {
                    cmd_row = cmd_row.child(
                        row()
                            .h(22.0)
                            .min_w(34.0)
                            .px(7.0)
                            .radius(11.0)
                            .shrink()
                            .center()
                            .fill(pal.overlay_strong)
                            .child(
                                text(cmd.shortcut)
                                    .font(TextStyle::new(12.5, 400.0))
                                    .color(pal.text_secondary),
                            ),
                    );
                }
                results = results.child(cmd_row);
            }
        }
        if !filtered_threads.is_empty() {
            for (tidx, th) in filtered_threads.into_iter().enumerate() {
                let id = th.id.clone();
                results = results.child(
                    row()
                        .key(format!("palette-thread-{tidx}"))
                        .role("button")
                        .label(&th.title)
                        .w_full()
                        .h(44.0)
                        .px(11.0)
                        .justify_between()
                        .center_y()
                        .radius(8.0)
                        .hover(pal.overlay)
                        .cursor(Cursor::Pointer)
                        .on_press(move |st: &mut PiGuiState| {
                            st.dispatch(PiGuiAction::CloseOverlay, None);
                            st.dispatch(PiGuiAction::SelectThread(id.clone()), None);
                        })
                        .child(
                            text(&th.title)
                                .font(TextStyle::new(13.5, 400.0))
                                .color(pal.text)
                                .truncate(),
                        )
                        .child(text("Task").font(TextStyle::new(11.5, 400.0)).color(pal.text_tertiary)),
                );
            }
        }
    }

    let mut card = col()
        .key("command_palette.card")
        .role("dialog")
        .label_static("Command Palette")
        .w_full()
        .max_w(LayoutMetrics::COMMAND_PALETTE_MAX_WIDTH)
        .max_h(LayoutMetrics::COMMAND_PALETTE_MAX_HEIGHT)
        .radius(WakuRadius::PALETTE)
        .fill(pal.raised)
        .outline(1.0, 0.0, pal.border_strong)
        .clip_rounded(WakuRadius::PALETTE)
        .on_press(|_: &mut PiGuiState| {});

    if !is_empty {
        card = card.h(LayoutMetrics::COMMAND_PALETTE_MAX_HEIGHT);
    }

    card.child(
        row()
            .key("command_palette.search")
            .role("textbox")
            .label_static("Search tasks or run a command…")
            .w_full()
            .h(60.0)
            .px(19.0)
            .gap(8.0)
            .center_y()
            .border_layout()
            .border_sides([0.0, 0.0, 1.0, 0.0], pal.border)
            .shrink()
            .child(icon(&ICON_SEARCH, 16.0).color(pal.text_tertiary))
            .child(
                text_input(&s.palette_edit, TextStyle::new(15.0, 400.0))
                    .label("Search tasks or run a command")
                    .single_line()
                    .grow(1.0)
                    .min_w(0.0)
                    .color(pal.text)
                    .placeholder("Search tasks or run a command…", pal.text_ghost)
                    .caret(pal.accent, pal.selection)
                    .on_edit(|st: &mut PiGuiState| &mut st.palette_edit)
                    .on_input_key(|st: &mut PiGuiState, ev, tx| {
                        if ev.key == Key::Escape {
                            st.dispatch(PiGuiAction::CloseOverlay, Some(tx));
                            return true;
                        }
                        false
                    }),
            ),
    )
    .child(results)
}

fn render_commit_action_row(
    key: &'static str,
    label: &'static str,
    action_icon: &'static Icon,
    enabled: bool,
    shortcut: Option<&'static str>,
    pal: &WakuPalette,
    action: PiGuiAction,
) -> El {
    let fg = if enabled { pal.text } else { pal.text_ghost };
    let mut r = row()
        .key(key)
        .role("button")
        .label_static(label)
        .w_full()
        .h(38.0)
        .px(10.0)
        .gap(10.0)
        .justify_start()
        .center_y()
        .radius(9.0)
        .fill(Color::TRANSPARENT);
    if enabled {
        r = r
            .hover(pal.overlay_strong)
            .cursor(Cursor::Pointer)
            .on_press(move |st: &mut PiGuiState| st.dispatch(action.clone(), None));
    }
    r = r
        .child(icon(action_icon, 15.0).color(fg).shrink())
        .child(text(label).font(TextStyle::new(14.0, 400.0)).color(fg))
        .child(el().grow(1.0));
    if let Some(sc) = shortcut {
        r = r.child(
            row()
                .h(22.0)
                .min_w(34.0)
                .px(7.0)
                .radius(11.0)
                .center()
                .fill(pal.overlay_strong)
                .child(text(sc).font(TextStyle::new(12.5, 400.0)).color(if enabled {
                    pal.text_secondary
                } else {
                    pal.text_ghost
                })),
        );
    }
    r
}

fn render_commit_dialog(s: &PiGuiState, pal: &WakuPalette) -> El {
    let check_bg = if s.commit_include_unstaged {
        pal.composer
    } else {
        Color::TRANSPARENT
    };
    let check_border = if s.commit_include_unstaged {
        pal.border_strong
    } else {
        pal.text_ghost
    };
    let branch_title = s.diff.branch.as_deref().unwrap_or(s.selected_branch.as_str());
    let has_changes = !s.diff.files.is_empty();
    let shadow_color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: if pal.is_dark { 0.50 } else { 0.16 },
    };

    col()
        .key("commit_dialog.card")
        .role("dialog")
        .w(LayoutMetrics::COMMIT_DIALOG_MAX_WIDTH)
        .max_w(LayoutMetrics::COMMIT_DIALOG_MAX_WIDTH)
        .min_h(350.0)
        .radius(WakuRadius::DIALOG)
        .fill(pal.composer)
        .outline(1.0, 0.0, pal.border_strong)
        .shadow(12.0, 32.0, shadow_color)
        .clip_rounded(WakuRadius::DIALOG)
        .on_press(|_: &mut PiGuiState| {})
        .child(
            row()
                .key("commit_dialog.header")
                .role("region")
                .w_full()
                .h(48.0)
                .px(16.0)
                .gap(9.0)
                .justify_start()
                .center_y()
                .child(icon(&ICON_BRANCH, 15.0).color(pal.text).shrink())
                .child(
                    text(branch_title)
                        .font(TextStyle::new(14.0, 500.0))
                        .color(pal.text)
                        .truncate()
                        .min_w(0.0),
                ),
        )
        .child(
            col()
                .key("commit_dialog.message")
                .role("textbox")
                .w_full()
                .h(112.0)
                .py(10.0)
                .px(16.0)
                .child(
                    text_input(&s.commit_message_edit, TextStyle::new(14.0, 400.0).lh(21.0))
                        .label("Commit message")
                        .w_full()
                        .color(pal.text)
                        .placeholder("Commit message", pal.text_ghost)
                        .caret(pal.accent, pal.selection)
                        .on_edit(|st: &mut PiGuiState| &mut st.commit_message_edit),
                ),
        )
        .child(
            row()
                .key("commit_dialog.include_unstaged")
                .role("checkbox")
                .label_static("Include unstaged")
                .h(44.0)
                .mx(8.0)
                .px(12.0)
                .gap(10.0)
                .center_y()
                .radius(9.0)
                .hover(pal.overlay)
                .cursor(Cursor::Pointer)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ToggleCommitIncludeUnstaged, None))
                .child(
                    el().size(15.0, 15.0)
                        .radius(4.0)
                        .center()
                        .fill(check_bg)
                        .border(1.0, check_border)
                        .child(if s.commit_include_unstaged {
                            icon(&ICON_CHECK, 12.0).color(pal.text)
                        } else {
                            el()
                        }),
                )
                .child(
                    text("Include unstaged")
                        .font(TextStyle::new(14.0, 400.0))
                        .color(pal.text)
                        .grow(1.0),
                )
                .child(
                    row()
                        .gap(6.0)
                        .center_y()
                        .shrink()
                        .child(
                            text(format!("+{}", s.diff.total_additions))
                                .font(TextStyle::new(13.5, 500.0))
                                .color(pal.success),
                        )
                        .child(
                            text(format!("-{}", s.diff.total_deletions))
                                .font(TextStyle::new(13.5, 500.0))
                                .color(pal.danger),
                        ),
                ),
        )
        .child(el().mx(8.0).h(1.0).fill(pal.border))
        .child(
            col()
                .w_full()
                .padding(8.0)
                .gap(2.0)
                .child(render_commit_action_row(
                    "commit_dialog.commit_btn",
                    "Commit",
                    &ICON_GIT_COMMIT,
                    has_changes,
                    Some("⌘↩"),
                    pal,
                    PiGuiAction::SubmitCommit,
                ))
                .child(render_commit_action_row(
                    "commit-action-commit-and-push",
                    "Commit & Push",
                    &ICON_CLOUD_UPLOAD,
                    has_changes,
                    None,
                    pal,
                    PiGuiAction::SubmitCommitAndPush,
                ))
                .child(render_commit_action_row(
                    "commit-action-push",
                    "Push",
                    &ICON_CLOUD_UPLOAD,
                    true,
                    None,
                    pal,
                    PiGuiAction::SubmitPush,
                )),
        )
}

/// Renders the centered modal backdrop and dialog when `s.overlay.is_some()`.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> Option<El> {
    let overlay = s.overlay.as_ref()?;
    match overlay {
        OverlayKind::CommandPalette => {
            let card = render_command_palette(s, pal);
            Some(
                col()
                    .key("modal-backdrop")
                    .absolute(0.0, 0.0)
                    .w_full()
                    .h_full()
                    .pt(72.0)
                    .px(16.0)
                    .items_center()
                    .fill(Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.45,
                    })
                    .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
                    .child(card),
            )
        }
        OverlayKind::CommitDialog => {
            let card = render_commit_dialog(s, pal);
            Some(
                col()
                    .key("modal-backdrop")
                    .absolute(0.0, 0.0)
                    .w_full()
                    .h_full()
                    .center()
                    .padding(16.0)
                    .fill(Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.45,
                    })
                    .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
                    .child(card),
            )
        }
        OverlayKind::ExtensionDialog(dlg) => {
            let mut body = col()
                .key("overlay-extension-dialog")
                .role("dialog")
                .w(420.0)
                .padding(16.0)
                .gap(10.0)
                .radius(WakuRadius::DIALOG)
                .fill(pal.composer)
                .border_layout()
                .border(1.0, pal.border_strong)
                .on_press(|_: &mut PiGuiState| {})
                .child(text(&dlg.title).font(TextStyle::new(14.5, 500.0)).color(pal.text));
            if let Some(msg) = &dlg.message {
                body = body.child(text(msg).font(k.ty.body).color(pal.text_secondary).wrap());
            }
            for opt in &dlg.options {
                let req_id = dlg.request_id.clone();
                let val = opt.clone();
                body = body.child(
                    Button::new(opt)
                        .variant(ButtonVariant::Outline)
                        .on_press(move |st: &mut PiGuiState| {
                            st.dispatch(
                                PiGuiAction::RespondExtensionDialog {
                                    request_id: req_id.clone(),
                                    value: Some(val.clone()),
                                },
                                None,
                            );
                        })
                        .build(k),
                );
            }
            let req_id = dlg.request_id.clone();
            body = body.child(
                Button::new("Dismiss")
                    .variant(ButtonVariant::Ghost)
                    .control_size(ControlSize::Small)
                    .on_press(move |st: &mut PiGuiState| {
                        st.dispatch(
                            PiGuiAction::RespondExtensionDialog {
                                request_id: req_id.clone(),
                                value: None,
                            },
                            None,
                        );
                    })
                    .build(k),
            );
            Some(
                col()
                    .absolute(0.0, 0.0)
                    .w_full()
                    .h_full()
                    .center()
                    .fill(Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.45,
                    })
                    .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
                    .child(body),
            )
        }
        OverlayKind::Confirm {
            title,
            message,
            action_id,
        } => {
            let act_id = action_id.clone();
            let card = col()
                .key("overlay-confirm")
                .role("dialog")
                .w(400.0)
                .padding(16.0)
                .gap(12.0)
                .radius(WakuRadius::DIALOG)
                .fill(pal.composer)
                .border_layout()
                .border(1.0, pal.border_strong)
                .on_press(|_: &mut PiGuiState| {})
                .child(text(title).font(TextStyle::new(14.5, 500.0)).color(pal.text))
                .child(text(message).font(k.ty.body).color(pal.text_secondary).wrap())
                .child(
                    row()
                        .w_full()
                        .justify_end()
                        .gap(8.0)
                        .child(
                            Button::new("Cancel")
                                .variant(ButtonVariant::Ghost)
                                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
                                .build(k),
                        )
                        .child(
                            Button::new("Confirm")
                                .variant(ButtonVariant::Submit)
                                .on_press(move |st: &mut PiGuiState| {
                                    if act_id == "clear-terminal" {
                                        st.dispatch(PiGuiAction::ClearTerminal, None);
                                    }
                                    st.dispatch(PiGuiAction::CloseOverlay, None);
                                })
                                .build(k),
                        ),
                );
            Some(
                col()
                    .absolute(0.0, 0.0)
                    .w_full()
                    .h_full()
                    .center()
                    .fill(Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.45,
                    })
                    .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
                    .child(card),
            )
        }
        OverlayKind::ImagePreview { path, title } => {
            let card = col()
                .key("overlay-image-preview")
                .role("dialog")
                .w(480.0)
                .padding(16.0)
                .gap(10.0)
                .radius(WakuRadius::LG)
                .fill(pal.composer)
                .border_layout()
                .border(1.0, pal.border_strong)
                .on_press(|_: &mut PiGuiState| {})
                .child(text(title).font(TextStyle::new(14.0, 500.0)).color(pal.text))
                .child(
                    text(path)
                        .font(TextStyle::new(12.0, 400.0).mono())
                        .color(pal.text_tertiary),
                )
                .child(
                    Button::new("Close")
                        .variant(ButtonVariant::Ghost)
                        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
                        .build(k),
                );
            Some(
                col()
                    .absolute(0.0, 0.0)
                    .w_full()
                    .h_full()
                    .center()
                    .fill(Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.45,
                    })
                    .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
                    .child(card),
            )
        }
    }
}

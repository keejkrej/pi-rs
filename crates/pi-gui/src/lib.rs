//! Native desktop GUI for pi-rs built with Rune, rune-kit, and Waku's graphite palette.

pub mod bridge;
pub mod chrome;
pub mod composer;
pub mod diff_view;
pub mod file_view;
pub mod models;
pub mod overlays;
pub mod right_panel;
pub mod settings;
pub mod sidebar;
pub mod state;
pub mod terminal_view;
pub mod theme;
pub mod transcript;

pub use bridge::{BridgeCommand, BridgeEvent, GuiBridge};
pub use chrome::{app_spec, app_view, init_state, on_key, poll_state, run, setup_state, view as chrome_view};
pub use state::{
    ActiveStreamState, AuthProviderViewModel, ContentBlockView, ContextUsageView, DemoScene, DiffFileViewModel,
    DiffHunkViewModel, DiffLineKind, DiffLineViewModel, DiffSummary, ExtensionDialogView, FileTreeEntryView,
    InspectorTab, LaunchConfig, MessageRoleView, MessageViewModel, ModelUsageSummary, ModelViewModel, NavRailTab,
    OpenFileView, OverlayKind, PiGuiAction, PiGuiError, PiGuiState, PluginKind, PluginOrigin, PluginViewModel,
    SteeringQueueItem, TerminalOutputLine, TerminalSessionView, ThreadGroup, ThreadSummary, ToastMessage, ToastTone,
    ToolCallStatus, ToolCallViewModel,
};
pub use theme::{LayoutMetrics, ThemePreference, WakuPalette, palette, sp, waku_kit};

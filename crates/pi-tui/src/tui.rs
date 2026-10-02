//! Port of packages/tui/src/tui.ts
//!
//! Minimal TUI implementation with differential rendering.

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

use async_trait::async_trait;
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};

use crate::layout_node::LayoutComponent;
use crate::terminal::Terminal;
use crate::terminal_colors::{RgbColor, TerminalColorScheme, TerminalColors};

pub use crate::utils::visible_width;

const TERMINAL_PALETTE_SIZE: i64 = 16;
/// OSC 10 and 11 plus OSC 4 for every palette color.
const TERMINAL_COLOR_REPLY_COUNT: i64 = 2 + TERMINAL_PALETTE_SIZE;
/// Default colors, palette colors 0-15, and a trailing primary device attributes (DA1) request.
/// Every terminal answers DA1 and terminals answer in order, so the DA1 reply marks the end of
/// the color replies, including for terminals that ignore the color queries.
const TERMINAL_COLOR_QUERY: &str = "\x1b]10;?\x07\x1b]11;?\x07\x1b]4;0;?\x07\x1b]4;1;?\x07\x1b]4;2;?\x07\x1b]4;3;?\x07\x1b]4;4;?\x07\x1b]4;5;?\x07\x1b]4;6;?\x07\x1b]4;7;?\x07\x1b]4;8;?\x07\x1b]4;9;?\x07\x1b]4;10;?\x07\x1b]4;11;?\x07\x1b]4;12;?\x07\x1b]4;13;?\x07\x1b]4;14;?\x07\x1b]4;15;?\x07\x1b[c";
/// PORT: JS `\d` is `[0-9]`.
static DEVICE_ATTRIBUTES_RESPONSE_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new("^\x1b\\[\\?[0-9;]*c$").expect("DEVICE_ATTRIBUTES_RESPONSE_PATTERN"));
const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TuiMouseEventType {
    #[serde(rename = "press")]
    Press,
    #[serde(rename = "release")]
    Release,
    #[serde(rename = "move")]
    Move,
    #[serde(rename = "drag")]
    Drag,
    #[serde(rename = "click")]
    Click,
    #[serde(rename = "wheel")]
    Wheel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TuiMouseButton {
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "middle")]
    Middle,
    #[serde(rename = "right")]
    Right,
    #[serde(rename = "none")]
    None,
}

/// Normalized cell-based mouse event. Coordinates are zero-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TuiMouseEvent {
    pub r#type: TuiMouseEventType,
    pub button: TuiMouseButton,
    /// Coordinates local to the receiving component.
    pub x: i64,
    pub y: i64,
    /// Absolute terminal coordinates.
    pub screen_x: i64,
    pub screen_y: i64,
    /// Current component bounds.
    pub width: i64,
    pub height: i64,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    /// Logical lines. Negative values scroll up.
    pub wheel_delta: Option<i64>,
    /// Consecutive click count when type is click.
    pub click_count: Option<i64>,
}

/// PORT: `target` / `focus_target` are set only for a forwarded `TuiMouseDispatchResult`
/// (`"target" in result`). [`Component::handle_mouse`] stays `Option<TuiMouseEventResult>`.
#[derive(Clone, Default)]
pub struct TuiMouseEventResult {
    /// Stop propagation and suppress renderer-level fallback behavior.
    pub handled: Option<bool>,
    /// Route subsequent drag/release events to this component. Implies handled.
    pub capture: Option<bool>,
    /// Give keyboard focus to this component. Implies handled.
    pub focus: Option<bool>,
    /// Explicitly request or suppress a render. Move and release default to false;
    /// press, click, drag, and wheel default to true.
    pub render: Option<bool>,
    pub target: Option<TuiMouseDispatchTarget>,
    pub focus_target: Option<Arc<dyn Component>>,
}

/// Internal target metadata used by containers and alternate-screen dispatch.
#[derive(Clone)]
pub struct TuiMouseDispatchTarget {
    pub component: Arc<dyn Component>,
    pub origin_x: i64,
    pub origin_y: i64,
    pub width: i64,
    pub height: i64,
}

/// Result of dispatching to a concrete component.
#[derive(Clone)]
pub struct TuiMouseDispatchResult {
    pub handled: bool,
    pub capture: Option<bool>,
    pub focus: Option<bool>,
    pub render: Option<bool>,
    pub target: TuiMouseDispatchTarget,
    /// Keyboard focus target, which may be a delegating parent container.
    pub focus_target: Option<Arc<dyn Component>>,
}

/// Dispatch an event to a component and retain the exact target and coordinate
/// transform. Containers use this when forwarding events to nested children.
pub fn dispatch_mouse_event(component: &dyn Component, event: &TuiMouseEvent) -> Option<TuiMouseDispatchResult> {
    todo!("port: dispatch_mouse_event")
}

/// Recreate local coordinates for a previously dispatched mouse target.
pub fn retarget_mouse_event(event: &TuiMouseEvent, target: &TuiMouseDispatchTarget) -> TuiMouseEvent {
    todo!("port: retarget_mouse_event")
}

/// Interface for components that can receive focus and display a hardware cursor.
/// When focused, the component should emit CURSOR_MARKER at the cursor position
/// in its render output. TUI will find this marker and position the hardware
/// cursor there for proper IME candidate window positioning.
pub trait Focusable: Send + Sync {
    /// Set by TUI when focus changes. Component should emit CURSOR_MARKER when true.
    fn focused(&self) -> bool;
    fn set_focused(&self, focused: bool);
}

/// Component interface - all components must implement this.
///
/// PORT: `handle_input` / `handle_mouse` / `wants_key_release` are optional in TS.
/// `has_handle_input` is false unless the component defines `handleInput`.
/// `is_container_handle_mouse` is TS `handleMouse === Container.prototype.handleMouse`.
/// `container_children` is TS `instanceof Container` (subclasses included, `Box` excluded).
/// `as_focusable` / `as_layout_component` stand in for `"focused" in component` and `LAYOUT_NODE`.
pub trait Component: Send + Sync {
    /// Render the component to lines for the given viewport width.
    fn render(&self, width: usize) -> Vec<String>;

    /// Optional handler for keyboard input when component has focus.
    fn handle_input(&self, data: &str) {}

    /// Optional normalized mouse handler.
    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    /// If true, component receives key release events (Kitty protocol).
    /// Default is false - release events are filtered out.
    fn wants_key_release(&self) -> bool {
        false
    }

    /// Invalidate any cached rendering state.
    /// Called when theme changes or when component needs to re-render from scratch.
    fn invalidate(&self);

    /// PORT: TS `handleInput` method presence. Default false.
    fn has_handle_input(&self) -> bool {
        false
    }

    /// PORT: TS `component.handleMouse === Container.prototype.handleMouse`.
    fn is_container_handle_mouse(&self) -> bool {
        false
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        None
    }

    /// PORT: `Some` iff TS `instanceof Container`. The vec is that container's children.
    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        None
    }

    fn as_layout_component(&self) -> Option<&dyn LayoutComponent> {
        None
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TuiInputListenerResult {
    pub consume: Option<bool>,
    pub data: Option<String>,
}

/// PORT: TS `TuiInputListenerResult` includes `| undefined`, so the callback returns `Option`.
pub type TuiInputListener = Arc<dyn Fn(&str) -> Option<TuiInputListenerResult> + Send + Sync>;

/// Type guard to check if a component implements Focusable.
pub fn is_focusable(component: Option<&dyn Component>) -> bool {
    component.is_some_and(|component| component.as_focusable().is_some())
}

/// Cursor position marker - APC (Application Program Command) sequence.
/// This is a zero-width escape sequence that terminals ignore.
/// Components emit this at the cursor position when focused.
/// TUI finds and strips this marker, then positions the hardware cursor there.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

/// Anchor position for overlays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverlayAnchor {
    #[serde(rename = "center")]
    Center,
    #[serde(rename = "top-left")]
    TopLeft,
    #[serde(rename = "top-right")]
    TopRight,
    #[serde(rename = "bottom-left")]
    BottomLeft,
    #[serde(rename = "bottom-right")]
    BottomRight,
    #[serde(rename = "top-center")]
    TopCenter,
    #[serde(rename = "bottom-center")]
    BottomCenter,
    #[serde(rename = "left-center")]
    LeftCenter,
    #[serde(rename = "right-center")]
    RightCenter,
}

/// Margin configuration for overlays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverlayMargin {
    pub top: Option<i64>,
    pub right: Option<i64>,
    pub bottom: Option<i64>,
    pub left: Option<i64>,
}

/// `OverlayMargin | number`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayMarginValue {
    All(i64),
    Sides(OverlayMargin),
}

/// Value that can be absolute (number) or percentage (string like "50%").
#[derive(Clone, Debug, PartialEq)]
pub enum SizeValue {
    Absolute(f64),
    /// Raw percentage text, including `%`, so an invalid value still round-trips into the parser.
    Percent(String),
}

/// Parse a SizeValue into absolute value given a reference size.
fn parse_size_value(value: Option<&SizeValue>, reference_size: i64) -> Option<f64> {
    todo!("port: parse_size_value")
}

/// Options for overlay positioning and sizing.
/// Values can be absolute numbers or percentage strings (e.g., "50%").
#[derive(Clone, Default)]
pub struct OverlayOptions {
    /// Width in columns, or percentage of terminal width (e.g., "50%").
    pub width: Option<SizeValue>,
    /// Minimum width in columns.
    pub min_width: Option<i64>,
    /// Maximum height in rows, or percentage of terminal height (e.g., "50%").
    pub max_height: Option<SizeValue>,
    /// Anchor point for positioning (default: 'center').
    pub anchor: Option<OverlayAnchor>,
    /// Horizontal offset from anchor position (positive = right).
    pub offset_x: Option<i64>,
    /// Vertical offset from anchor position (positive = down).
    pub offset_y: Option<i64>,
    /// Row position: absolute number, or percentage (e.g., "25%" = 25% from top).
    pub row: Option<SizeValue>,
    /// Column position: absolute number, or percentage (e.g., "50%" = centered horizontally).
    pub col: Option<SizeValue>,
    /// Margin from terminal edges. Number applies to all sides.
    pub margin: Option<OverlayMarginValue>,
    /// Control overlay visibility based on terminal dimensions.
    /// If provided, overlay is only rendered when this returns true.
    /// Called each render cycle with current terminal dimensions.
    pub visible: Option<Arc<dyn Fn(i64, i64) -> bool + Send + Sync>>,
    /// If true, don't capture keyboard focus when shown.
    pub non_capturing: Option<bool>,
}

/// Options for [`OverlayHandle::unfocus`].
#[derive(Clone)]
pub struct OverlayUnfocusOptions {
    /// Explicit target to focus after releasing this overlay.
    pub target: Option<Arc<dyn Component>>,
}

/// Last rendered terminal-relative overlay rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayBounds {
    pub row: i64,
    pub col: i64,
    pub width: i64,
    pub height: i64,
}

/// Handle returned by showOverlay for controlling the overlay.
///
/// PORT: TS returns a closure object. This handle shares the stack entry by `Arc` identity.
#[derive(Clone)]
pub struct OverlayHandle {
    inner: Arc<OverlayHandleInner>,
}

struct OverlayHandleInner {
    base: TuiBase,
    entry: OverlayStackEntry,
}

impl OverlayHandle {
    /// Permanently remove the overlay (cannot be shown again).
    pub fn hide(&self) {
        todo!("port: OverlayHandle::hide")
    }

    /// Temporarily hide or show the overlay.
    pub fn set_hidden(&self, hidden: bool) {
        todo!("port: OverlayHandle::set_hidden")
    }

    /// Check if overlay is temporarily hidden.
    pub fn is_hidden(&self) -> bool {
        todo!("port: OverlayHandle::is_hidden")
    }

    /// Focus this overlay and bring it to the visual front.
    pub fn focus(&self) {
        todo!("port: OverlayHandle::focus")
    }

    /// Release focus to the next visible capturing overlay or previous target, or to an explicit target when provided.
    pub fn unfocus(&self, options: Option<OverlayUnfocusOptions>) {
        todo!("port: OverlayHandle::unfocus")
    }

    /// Check if this overlay currently has focus.
    pub fn is_focused(&self) -> bool {
        todo!("port: OverlayHandle::is_focused")
    }

    /// Get the most recent rendered bounds for a visible overlay.
    pub fn get_bounds(&self) -> Option<OverlayBounds> {
        todo!("port: OverlayHandle::get_bounds")
    }
}

struct ContainerMouseChild {
    component: Arc<dyn Component>,
    height: i64,
}

struct ContainerMouseLayout {
    width: i64,
    children: Vec<ContainerMouseChild>,
}

struct ContainerState {
    children: Vec<Arc<dyn Component>>,
    mouse_layout: Option<ContainerMouseLayout>,
}

struct ContainerInner {
    state: Mutex<ContainerState>,
}

/// Container - a component that contains other components.
///
/// PORT: TS class with identity. Subclasses compose this and delegate (§9.4).
#[derive(Clone)]
pub struct Container {
    inner: Arc<ContainerInner>,
}

impl Container {
    pub fn new() -> Self {
        Container {
            inner: Arc::new(ContainerInner {
                state: Mutex::new(ContainerState {
                    children: Vec::new(),
                    mouse_layout: None,
                }),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ContainerState> {
        self.inner.state.lock().unwrap()
    }

    /// Public `children` array (Arc clones).
    pub fn children(&self) -> Vec<Arc<dyn Component>> {
        self.lock().children.clone()
    }

    /// `this.children = children`.
    pub fn set_children(&self, children: Vec<Arc<dyn Component>>) {
        self.lock().children = children;
    }

    pub fn add_child(&self, component: Arc<dyn Component>) {
        todo!("port: Container::add_child")
    }

    pub fn remove_child(&self, component: &Arc<dyn Component>) {
        todo!("port: Container::remove_child")
    }

    pub fn clear(&self) {
        todo!("port: Container::clear")
    }

    pub fn invalidate(&self) {
        todo!("port: Container::invalidate")
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        todo!("port: Container::handle_mouse")
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: Container::render")
    }
}

impl Component for Container {
    fn render(&self, width: usize) -> Vec<String> {
        Container::render(self, width)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        Container::handle_mouse(self, event)
    }

    fn invalidate(&self) {
        Container::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.children())
    }
}

/// Composite overlay content into a terminal line at a fixed column.
pub fn composite_tui_line(
    base_line: &str,
    overlay_line: &str,
    start_col: i64,
    overlay_width: i64,
    total_width: i64,
) -> String {
    todo!("port: composite_tui_line")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TuiMode {
    #[serde(rename = "regular")]
    Regular,
    #[serde(rename = "fullscreen")]
    Fullscreen,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TuiStopOptions {
    /// Leave renderer output in place for another TUI taking over the same terminal.
    pub preserve_screen: Option<bool>,
}

pub struct QueryTerminalColorsOptions {
    /// Query timeout in milliseconds, for terminals that do not answer DA1 either.
    pub timeout_ms: i64,
    /// Receives the replies if the query completes after the timeout, e.g. over slow links.
    pub on_late_reply: Option<Arc<dyn Fn(TerminalColors) + Send + Sync>>,
}

/// PORT: JS `Symbol.for("@earendil-works/pi-tui/viewport")`. Capability is [`ViewportTUI`].
pub const VIEWPORT_TUI: &str = "@earendil-works/pi-tui/viewport";

#[async_trait]
pub trait ViewportTUI: TUI {
    fn set_layout_root(&self, component: Option<Arc<dyn Component>>);
}

#[async_trait]
pub trait TUI: Component {
    fn mode(&self) -> TuiMode;
    fn children(&self) -> Vec<Arc<dyn Component>>;
    fn set_children(&self, children: Vec<Arc<dyn Component>>);
    fn terminal(&self) -> Arc<dyn Terminal>;
    fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>>;
    fn set_on_debug(&self, on_debug: Option<Arc<dyn Fn() + Send + Sync>>);
    fn full_redraws(&self) -> i64;
    fn add_child(&self, component: Arc<dyn Component>);
    fn remove_child(&self, component: &Arc<dyn Component>);
    fn clear(&self);
    fn get_show_hardware_cursor(&self) -> bool;
    fn set_show_hardware_cursor(&self, enabled: bool);
    fn get_clear_on_shrink(&self) -> bool;
    fn set_clear_on_shrink(&self, enabled: bool);
    fn set_focus(&self, component: Option<Arc<dyn Component>>);
    fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle;
    fn hide_overlay(&self);
    fn has_overlay(&self) -> bool;
    fn start(&self);
    fn stop(&self, options: Option<TuiStopOptions>);
    fn render_now(&self, force: Option<bool>);
    fn request_render(&self, force: Option<bool>);
    fn add_input_listener(&self, listener: TuiInputListener) -> pi_js::Unsubscribe;
    fn remove_input_listener(&self, listener: &TuiInputListener);
    fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe;
    fn set_terminal_color_scheme_notifications(&self, enabled: bool);

    /// Query the terminal's theme colors: the default foreground (OSC 10), the default background
    /// (OSC 11), and ANSI colors 0-15 (OSC 4), followed by a DA1 request that marks the end of the
    /// replies. Resolves when the DA1 reply or all color replies arrive, or when the timeout expires.
    /// Colors the terminal did not report are undefined; the palette is only set when all 16 arrived.
    async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors;

    /// PORT: TS `[VIEWPORT_TUI] === true`.
    fn as_viewport(&self) -> Option<&dyn ViewportTUI> {
        None
    }
}

pub fn is_viewport_tui(tui: &dyn TUI) -> bool {
    tui.as_viewport().is_some()
}

#[derive(Clone)]
struct OverlayStackEntry {
    inner: Arc<OverlayStackEntryInner>,
}

struct OverlayStackEntryInner {
    state: Mutex<OverlayStackEntryState>,
}

struct OverlayStackEntryState {
    component: Arc<dyn Component>,
    options: Option<OverlayOptions>,
    pre_focus: Option<Arc<dyn Component>>,
    hidden: bool,
    focus_order: i64,
    bounds: Option<OverlayBounds>,
}

struct RenderedOverlayLayout {
    entry: OverlayStackEntry,
    row: i64,
    col: i64,
    width: i64,
    height: i64,
}

#[derive(Clone)]
enum OverlayBlockedFocusResume {
    RestoreOverlay,
    FocusTarget { target: Option<Arc<dyn Component>> },
}

#[derive(Clone)]
enum OverlayFocusRestoreState {
    Inactive,
    Eligible {
        overlay: OverlayStackEntry,
    },
    Blocked {
        overlay: OverlayStackEntry,
        blocked_by: Arc<dyn Component>,
        resume: OverlayBlockedFocusResume,
    },
}

#[derive(Clone, Copy)]
enum OverlayFocusRestorePolicy {
    Clear,
    Preserve,
}

struct PendingTerminalColorQuery {
    foreground: Option<RgbColor>,
    background: Option<RgbColor>,
    palette: Vec<Option<RgbColor>>,
    /// Targets that already replied, so duplicates do not count twice.
    replied: IndexSet<String>,
    /// Receives the result: the promise's resolve until the timeout, then `onLateReply`. Unset once the
    /// query completed (on the DA1 reply or once every color replied); later replies are ignored.
    deliver: Option<Arc<dyn Fn(TerminalColors) + Send + Sync>>,
    timer: Option<pi_js::time::Timeout>,
}

pub(crate) struct CursorPosition {
    pub row: i64,
    pub col: i64,
}

pub(crate) struct OverlayMouseDispatch {
    pub hit: bool,
    pub result: Option<TuiMouseDispatchResult>,
}

struct ResolvedOverlayLayout {
    width: i64,
    row: i64,
    col: i64,
    max_height: Option<i64>,
}

/// Overridable `protected` / `abstract` hooks of [`TuiBase`].
///
/// PORT: TS subclassing is composition. `TuiMainScreen` / `TuiAltScreen` implement this and
/// [`TuiBase::bind_overrides`]. Defaults match the empty base methods. `do_render` and `mode` are abstract.
pub(crate) trait TuiBaseOverrides: Send + Sync {
    fn mode(&self) -> TuiMode;
    fn do_render(&self);
    fn reset_render_state(&self) {}
    fn before_terminal_start(&self) {}
    fn after_terminal_start(&self) {}
    fn before_terminal_stop(&self, options: &TuiStopOptions) {}
    fn after_terminal_stop(&self, options: &TuiStopOptions) {}
    fn get_mounted_roots(&self, base: &TuiBase) -> Vec<Arc<dyn Component>> {
        base.children()
    }
}

struct TuiBaseState {
    focused_component: Option<Arc<dyn Component>>,
    /// PORT: JS `Set` of callbacks. Insertion order; removal uses `Arc::ptr_eq`.
    input_listeners: Vec<TuiInputListener>,
    on_debug: Option<Arc<dyn Fn() + Send + Sync>>,
    render_requested: bool,
    immediate_render_scheduled: bool,
    render_timer: Option<pi_js::time::Timeout>,
    last_render_at: f64,
    show_hardware_cursor: bool,
    clear_on_shrink: bool,
    full_redraw_count: i64,
    stopped: bool,
    pending_terminal_color_queries: Vec<PendingTerminalColorQuery>,
    terminal_color_scheme_listeners: Vec<Arc<dyn Fn(TerminalColorScheme) + Send + Sync>>,
    terminal_color_scheme_notifications_enabled: bool,
    focus_order_counter: i64,
    overlay_stack: Vec<OverlayStackEntry>,
    rendered_overlay_layouts: Vec<RenderedOverlayLayout>,
    overlay_focus_restore: OverlayFocusRestoreState,
}

struct TuiBaseInner {
    terminal: Arc<dyn Terminal>,
    /// Directory for debug/crash logs. When absent, debug logging is disabled and crash dumps fall back to the OS temp directory.
    log_directory: Option<String>,
    container: Container,
    /// PORT: `Weak` breaks the screen → base → overrides → screen cycle.
    overrides: Mutex<Option<Weak<dyn TuiBaseOverrides>>>,
    state: Mutex<TuiBaseState>,
}

/// Abstract TUI base. Concrete screens are [`crate::tui_main_screen::TuiMainScreen`] and `TuiAltScreen`.
///
/// PORT: TS class with identity. `extends Container` is a composed [`Container`].
#[derive(Clone)]
pub struct TuiBase {
    inner: Arc<TuiBaseInner>,
}

impl TuiBase {
    const MIN_RENDER_INTERVAL_MS: i64 = 16;

    pub fn new(terminal: Arc<dyn Terminal>, show_hardware_cursor: Option<bool>, log_directory: Option<&str>) -> Self {
        todo!("port: TuiBase::new")
    }

    pub(crate) fn bind_overrides(&self, overrides: Arc<dyn TuiBaseOverrides>) {
        todo!("port: TuiBase::bind_overrides")
    }

    fn lock(&self) -> MutexGuard<'_, TuiBaseState> {
        self.inner.state.lock().unwrap()
    }

    pub fn mode(&self) -> TuiMode {
        todo!("port: TuiBase::mode")
    }

    pub fn children(&self) -> Vec<Arc<dyn Component>> {
        self.inner.container.children()
    }

    pub fn set_children(&self, children: Vec<Arc<dyn Component>>) {
        self.inner.container.set_children(children);
    }

    pub fn terminal(&self) -> Arc<dyn Terminal> {
        Arc::clone(&self.inner.terminal)
    }

    pub fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.lock().on_debug.clone()
    }

    pub fn set_on_debug(&self, on_debug: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.lock().on_debug = on_debug;
    }

    pub fn full_redraws(&self) -> i64 {
        self.lock().full_redraw_count
    }

    pub(crate) fn full_redraw_count(&self) -> i64 {
        self.lock().full_redraw_count
    }

    pub(crate) fn set_full_redraw_count(&self, count: i64) {
        self.lock().full_redraw_count = count;
    }

    pub(crate) fn stopped(&self) -> bool {
        self.lock().stopped
    }

    pub(crate) fn log_directory(&self) -> Option<&str> {
        self.inner.log_directory.as_deref()
    }

    pub fn add_child(&self, component: Arc<dyn Component>) {
        self.inner.container.add_child(component);
    }

    pub fn remove_child(&self, component: &Arc<dyn Component>) {
        self.inner.container.remove_child(component);
    }

    pub fn clear(&self) {
        self.inner.container.clear();
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    pub fn get_show_hardware_cursor(&self) -> bool {
        self.lock().show_hardware_cursor
    }

    pub fn set_show_hardware_cursor(&self, enabled: bool) {
        todo!("port: TuiBase::set_show_hardware_cursor")
    }

    pub fn get_clear_on_shrink(&self) -> bool {
        self.lock().clear_on_shrink
    }

    /// Set whether to trigger full re-render when content shrinks.
    /// When true, empty rows are cleared when content shrinks.
    /// When false (default), empty rows remain (reduces redraws on slower terminals).
    pub fn set_clear_on_shrink(&self, enabled: bool) {
        self.lock().clear_on_shrink = enabled;
    }

    pub fn get_focused_component(&self) -> Option<Arc<dyn Component>> {
        self.lock().focused_component.clone()
    }

    pub fn set_focus(&self, component: Option<Arc<dyn Component>>) {
        todo!("port: TuiBase::set_focus")
    }

    fn set_focus_internal(
        &self,
        component: Option<Arc<dyn Component>>,
        overlay_focus_restore: OverlayFocusRestorePolicy,
    ) {
        todo!("port: TuiBase::set_focus_internal")
    }

    fn clear_overlay_focus_restore(&self) {
        todo!("port: TuiBase::clear_overlay_focus_restore")
    }

    fn clear_overlay_focus_restore_for(&self, overlay: &OverlayStackEntry) {
        todo!("port: TuiBase::clear_overlay_focus_restore_for")
    }

    fn resolve_blocked_overlay_focus_resume(
        &self,
        restore_state: &OverlayFocusRestoreState,
    ) -> Option<Arc<dyn Component>> {
        todo!("port: TuiBase::resolve_blocked_overlay_focus_resume")
    }

    fn get_visible_overlay_focus_restore(&self) -> OverlayFocusRestoreState {
        todo!("port: TuiBase::get_visible_overlay_focus_restore")
    }

    fn is_overlay_focus_ancestor(&self, entry: &OverlayStackEntry, component: &Arc<dyn Component>) -> bool {
        todo!("port: TuiBase::is_overlay_focus_ancestor")
    }

    fn retarget_overlay_pre_focus(&self, removed: &OverlayStackEntry) {
        todo!("port: TuiBase::retarget_overlay_pre_focus")
    }

    pub(crate) fn get_mounted_roots(&self) -> Vec<Arc<dyn Component>> {
        todo!("port: TuiBase::get_mounted_roots")
    }

    fn is_component_mounted(&self, component: &Arc<dyn Component>) -> bool {
        todo!("port: TuiBase::is_component_mounted")
    }

    fn contains_component(&self, root: &Arc<dyn Component>, target: &Arc<dyn Component>) -> bool {
        todo!("port: TuiBase::contains_component")
    }

    /// Show an overlay component with configurable positioning and sizing.
    /// Returns a handle to control the overlay's visibility.
    pub fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
        todo!("port: TuiBase::show_overlay")
    }

    /// Hide the topmost overlay and restore previous focus.
    pub fn hide_overlay(&self) {
        todo!("port: TuiBase::hide_overlay")
    }

    /// Hide the cursor while running. After stop(), the shell owns the cursor and it must stay visible.
    fn hide_terminal_cursor(&self) {
        todo!("port: TuiBase::hide_terminal_cursor")
    }

    /// Check if there are any visible overlays.
    pub fn has_overlay(&self) -> bool {
        todo!("port: TuiBase::has_overlay")
    }

    pub fn has_overlay_entries(&self) -> bool {
        !self.lock().overlay_stack.is_empty()
    }

    /// Check if the focused component is a visible overlay.
    pub(crate) fn is_overlay_focused(&self) -> bool {
        todo!("port: TuiBase::is_overlay_focused")
    }

    /// Keep overlay containers as keyboard focus owners when a nested control is clicked.
    pub(crate) fn resolve_mouse_focus_target(&self, component: Arc<dyn Component>) -> Arc<dyn Component> {
        todo!("port: TuiBase::resolve_mouse_focus_target")
    }

    /// Dispatch to the visually topmost overlay under the pointer.
    pub(crate) fn dispatch_mouse_to_overlay(&self, event: &TuiMouseEvent) -> OverlayMouseDispatch {
        todo!("port: TuiBase::dispatch_mouse_to_overlay")
    }

    fn is_overlay_visible(&self, entry: &OverlayStackEntry) -> bool {
        todo!("port: TuiBase::is_overlay_visible")
    }

    fn get_topmost_visible_overlay(&self) -> Option<OverlayStackEntry> {
        todo!("port: TuiBase::get_topmost_visible_overlay")
    }

    pub fn invalidate(&self) {
        todo!("port: TuiBase::invalidate")
    }

    pub fn start(&self) {
        todo!("port: TuiBase::start")
    }

    pub fn add_input_listener(&self, listener: TuiInputListener) -> pi_js::Unsubscribe {
        todo!("port: TuiBase::add_input_listener")
    }

    pub fn remove_input_listener(&self, listener: &TuiInputListener) {
        todo!("port: TuiBase::remove_input_listener")
    }

    pub fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        todo!("port: TuiBase::on_terminal_color_scheme_change")
    }

    pub fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        todo!("port: TuiBase::set_terminal_color_scheme_notifications")
    }

    fn query_cell_size(&self) {
        todo!("port: TuiBase::query_cell_size")
    }

    pub fn stop(&self, options: Option<TuiStopOptions>) {
        todo!("port: TuiBase::stop")
    }

    pub fn render_now(&self, force: Option<bool>) {
        todo!("port: TuiBase::render_now")
    }

    pub fn request_render(&self, force: Option<bool>) {
        todo!("port: TuiBase::request_render")
    }

    fn request_immediate_render(&self) {
        todo!("port: TuiBase::request_immediate_render")
    }

    fn cancel_render_timer(&self) {
        todo!("port: TuiBase::cancel_render_timer")
    }

    fn schedule_render(&self) {
        todo!("port: TuiBase::schedule_render")
    }

    pub(crate) fn do_render(&self) {
        todo!("port: TuiBase::do_render")
    }

    pub(crate) fn reset_render_state(&self) {
        todo!("port: TuiBase::reset_render_state")
    }

    pub(crate) fn before_terminal_start(&self) {
        todo!("port: TuiBase::before_terminal_start")
    }

    pub(crate) fn after_terminal_start(&self) {
        todo!("port: TuiBase::after_terminal_start")
    }

    pub(crate) fn before_terminal_stop(&self, options: &TuiStopOptions) {
        todo!("port: TuiBase::before_terminal_stop")
    }

    pub(crate) fn after_terminal_stop(&self, options: &TuiStopOptions) {
        todo!("port: TuiBase::after_terminal_stop")
    }

    fn handle_terminal_input(&self, data: &str) {
        todo!("port: TuiBase::handle_terminal_input")
    }

    fn consume_terminal_color_response(&self, data: &str) -> bool {
        todo!("port: TuiBase::consume_terminal_color_response")
    }

    fn terminal_color_query_result(&self, query: &PendingTerminalColorQuery) -> TerminalColors {
        todo!("port: TuiBase::terminal_color_query_result")
    }

    fn complete_terminal_color_query(&self, query: &mut PendingTerminalColorQuery) {
        todo!("port: TuiBase::complete_terminal_color_query")
    }

    fn consume_terminal_color_scheme_report(&self, data: &str) -> bool {
        todo!("port: TuiBase::consume_terminal_color_scheme_report")
    }

    fn consume_cell_size_response(&self, data: &str) -> bool {
        todo!("port: TuiBase::consume_cell_size_response")
    }

    fn resolve_overlay_layout(
        &self,
        options: Option<&OverlayOptions>,
        overlay_height: i64,
        term_width: i64,
        term_height: i64,
    ) -> ResolvedOverlayLayout {
        todo!("port: TuiBase::resolve_overlay_layout")
    }

    fn resolve_anchor_row(&self, anchor: OverlayAnchor, height: i64, avail_height: i64, margin_top: i64) -> i64 {
        todo!("port: TuiBase::resolve_anchor_row")
    }

    fn resolve_anchor_col(&self, anchor: OverlayAnchor, width: i64, avail_width: i64, margin_left: i64) -> i64 {
        todo!("port: TuiBase::resolve_anchor_col")
    }

    /// Composite all overlays into content lines (sorted by focusOrder, higher = on top).
    pub(crate) fn composite_overlays(&self, lines: Vec<String>, term_width: i64, term_height: i64) -> Vec<String> {
        todo!("port: TuiBase::composite_overlays")
    }

    pub(crate) fn apply_line_resets(&self, lines: Vec<String>) -> Vec<String> {
        todo!("port: TuiBase::apply_line_resets")
    }

    fn composite_line_at(
        &self,
        base_line: &str,
        overlay_line: &str,
        start_col: i64,
        overlay_width: i64,
        total_width: i64,
    ) -> String {
        todo!("port: TuiBase::composite_line_at")
    }

    /// Find and extract cursor position from rendered lines.
    /// Searches for CURSOR_MARKER, calculates its position, and strips it from the output.
    /// Only scans the bottom terminal height lines (visible viewport).
    pub(crate) fn extract_cursor_position(&self, lines: &mut [String], height: i64) -> Option<CursorPosition> {
        todo!("port: TuiBase::extract_cursor_position")
    }

    pub async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        todo!("port: TuiBase::query_terminal_colors")
    }
}

impl Component for TuiBase {
    fn render(&self, width: usize) -> Vec<String> {
        TuiBase::render(self, width)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        TuiBase::handle_mouse(self, event)
    }

    fn invalidate(&self) {
        TuiBase::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.children())
    }
}

#[async_trait]
impl TUI for TuiBase {
    fn mode(&self) -> TuiMode {
        TuiBase::mode(self)
    }

    fn children(&self) -> Vec<Arc<dyn Component>> {
        TuiBase::children(self)
    }

    fn set_children(&self, children: Vec<Arc<dyn Component>>) {
        TuiBase::set_children(self, children);
    }

    fn terminal(&self) -> Arc<dyn Terminal> {
        TuiBase::terminal(self)
    }

    fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        TuiBase::on_debug(self)
    }

    fn set_on_debug(&self, on_debug: Option<Arc<dyn Fn() + Send + Sync>>) {
        TuiBase::set_on_debug(self, on_debug);
    }

    fn full_redraws(&self) -> i64 {
        TuiBase::full_redraws(self)
    }

    fn add_child(&self, component: Arc<dyn Component>) {
        TuiBase::add_child(self, component);
    }

    fn remove_child(&self, component: &Arc<dyn Component>) {
        TuiBase::remove_child(self, component);
    }

    fn clear(&self) {
        TuiBase::clear(self);
    }

    fn get_show_hardware_cursor(&self) -> bool {
        TuiBase::get_show_hardware_cursor(self)
    }

    fn set_show_hardware_cursor(&self, enabled: bool) {
        TuiBase::set_show_hardware_cursor(self, enabled);
    }

    fn get_clear_on_shrink(&self) -> bool {
        TuiBase::get_clear_on_shrink(self)
    }

    fn set_clear_on_shrink(&self, enabled: bool) {
        TuiBase::set_clear_on_shrink(self, enabled);
    }

    fn set_focus(&self, component: Option<Arc<dyn Component>>) {
        TuiBase::set_focus(self, component);
    }

    fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
        TuiBase::show_overlay(self, component, options)
    }

    fn hide_overlay(&self) {
        TuiBase::hide_overlay(self);
    }

    fn has_overlay(&self) -> bool {
        TuiBase::has_overlay(self)
    }

    fn start(&self) {
        TuiBase::start(self);
    }

    fn stop(&self, options: Option<TuiStopOptions>) {
        TuiBase::stop(self, options);
    }

    fn render_now(&self, force: Option<bool>) {
        TuiBase::render_now(self, force);
    }

    fn request_render(&self, force: Option<bool>) {
        TuiBase::request_render(self, force);
    }

    fn add_input_listener(&self, listener: TuiInputListener) -> pi_js::Unsubscribe {
        TuiBase::add_input_listener(self, listener)
    }

    fn remove_input_listener(&self, listener: &TuiInputListener) {
        TuiBase::remove_input_listener(self, listener);
    }

    fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        TuiBase::on_terminal_color_scheme_change(self, listener)
    }

    fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        TuiBase::set_terminal_color_scheme_notifications(self, enabled);
    }

    async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        TuiBase::query_terminal_colors(self, options).await
    }
}

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
// CURSOR_MARKER + FAKE_CURSOR_START + text + FAKE_CURSOR_END. Truncating the line right after the cursor cell drops the end.
static FOCUSED_FAKE_CURSOR: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new("(?s)(\x1b_pi:c\x07)\x1b_pi:fc\x07(.*?)(?:\x1b_pi:/fc\x07|\\z)").expect("FOCUSED_FAKE_CURSOR")
});
const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

static EXIT_RESTORE_ONCE: std::sync::Once = std::sync::Once::new();
static EXIT_RESTORE_REGISTRY: LazyLock<Mutex<Vec<(usize, Weak<TuiBaseInner>)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

fn run_exit_restore_handlers() {
    let snapshot = {
        let mut reg = EXIT_RESTORE_REGISTRY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut upgraded = Vec::with_capacity(reg.len());
        reg.retain(|(_, weak)| {
            if let Some(inner) = weak.upgrade() {
                upgraded.push(inner);
                true
            } else {
                false
            }
        });
        upgraded
    };
    for inner in snapshot {
        let base = TuiBase { inner };
        if base
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stopped
        {
            continue;
        }
        let prefix = base.exit_restore_prefix();
        let suffix = base.exit_restore_suffix();
        base.inner.terminal.restore_on_exit(&prefix, &suffix);
    }
}

#[cfg(unix)]
extern "C" fn exit_restore_atexit_handler() {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run_exit_restore_handlers));
}

fn ensure_exit_restore_registered() {
    #[cfg(unix)]
    EXIT_RESTORE_ONCE.call_once(|| {
        // SAFETY: `exit_restore_atexit_handler` is a `C`-ABI function with no captures or shared state.
        unsafe {
            libc::atexit(exit_restore_atexit_handler);
        }
    });
}

fn register_tui_exit_restore(inner: &Arc<TuiBaseInner>) {
    ensure_exit_restore_registered();
    let key = Arc::as_ptr(inner) as usize;
    let mut reg = EXIT_RESTORE_REGISTRY.lock().unwrap();
    reg.retain(|(k, w)| *k != key && w.strong_count() > 0);
    reg.push((key, Arc::downgrade(inner)));
}

fn unregister_tui_exit_restore(inner: &Arc<TuiBaseInner>) {
    let key = Arc::as_ptr(inner) as usize;
    let mut reg = EXIT_RESTORE_REGISTRY.lock().unwrap();
    reg.retain(|(k, w)| *k != key && w.strong_count() > 0);
}

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

static PERCENT_SIZE_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^([0-9]+(?:\.[0-9]+)?)%$").expect("PERCENT_SIZE_REGEX"));
static CELL_SIZE_RESPONSE_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new("^\x1b\\[6;([0-9]+);([0-9]+)t$").expect("CELL_SIZE_RESPONSE_REGEX"));

struct NoopDispatchComponent;

impl Component for NoopDispatchComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        Vec::new()
    }

    fn invalidate(&self) {}
}

pub(crate) fn dispatch_mouse_event_arc(
    component: &Arc<dyn Component>,
    event: &TuiMouseEvent,
) -> Option<TuiMouseDispatchResult> {
    let result = component.handle_mouse(event)?;
    if let Some(forwarded_target) = result.target {
        let focus = result.focus;
        let focus_target = if focus.unwrap_or(false) && component.has_handle_input() {
            Some(Arc::clone(component))
        } else {
            result.focus_target
        };
        return Some(TuiMouseDispatchResult {
            handled: true,
            capture: result.capture,
            focus,
            render: result.render,
            target: forwarded_target,
            focus_target,
        });
    }
    if !result.handled.unwrap_or(false) && !result.capture.unwrap_or(false) && !result.focus.unwrap_or(false) {
        return None;
    }
    let focus = result.focus;
    Some(TuiMouseDispatchResult {
        handled: true,
        capture: result.capture,
        focus,
        render: result.render,
        target: TuiMouseDispatchTarget {
            component: Arc::clone(component),
            origin_x: event.screen_x - event.x,
            origin_y: event.screen_y - event.y,
            width: event.width,
            height: event.height,
        },
        focus_target: if focus.unwrap_or(false) {
            Some(Arc::clone(component))
        } else {
            None
        },
    })
}

/// Dispatch an event to a component and retain the exact target and coordinate
/// transform. Containers use this when forwarding events to nested children.
pub fn dispatch_mouse_event(component: &dyn Component, event: &TuiMouseEvent) -> Option<TuiMouseDispatchResult> {
    let result = component.handle_mouse(event)?;
    if let Some(forwarded_target) = result.target {
        return Some(TuiMouseDispatchResult {
            handled: true,
            capture: result.capture,
            focus: result.focus,
            render: result.render,
            target: forwarded_target,
            focus_target: result.focus_target,
        });
    }
    if !result.handled.unwrap_or(false) && !result.capture.unwrap_or(false) && !result.focus.unwrap_or(false) {
        return None;
    }
    Some(TuiMouseDispatchResult {
        handled: true,
        capture: result.capture,
        focus: result.focus,
        render: result.render,
        target: TuiMouseDispatchTarget {
            component: Arc::new(NoopDispatchComponent),
            origin_x: event.screen_x - event.x,
            origin_y: event.screen_y - event.y,
            width: event.width,
            height: event.height,
        },
        focus_target: result.focus_target,
    })
}

/// Recreate local coordinates for a previously dispatched mouse target.
pub fn retarget_mouse_event(event: &TuiMouseEvent, target: &TuiMouseDispatchTarget) -> TuiMouseEvent {
    TuiMouseEvent {
        x: event.screen_x - target.origin_x,
        y: event.screen_y - target.origin_y,
        width: target.width,
        height: target.height,
        ..*event
    }
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

    /// If true while focused, fullscreen viewport keys (`tui.altScreen.*` scrolling and prompt jumps) go to this
    /// component instead of scrolling the transcript, as they do for a focused overlay. Default is false.
    fn captures_viewport_input(&self) -> bool {
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

/// Fake cursor markers - zero-width APC sequences around the text drawn as the cursor. See `resolve_fake_cursors`.
pub const FAKE_CURSOR_START: &str = "\x1b_pi:fc\x07";
pub const FAKE_CURSOR_END: &str = "\x1b_pi:/fc\x07";

/// Wrap text in fake cursor markers.
pub fn render_fake_cursor(text: &str) -> String {
    format!("{FAKE_CURSOR_START}{text}{FAKE_CURSOR_END}")
}

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
    let value = value?;
    match value {
        SizeValue::Absolute(n) => Some(*n),
        SizeValue::Percent(raw) => {
            let caps = PERCENT_SIZE_REGEX.captures(raw)?;
            let pct: f64 = caps.get(1)?.as_str().parse().ok()?;
            Some(((reference_size as f64) * pct / 100.0).floor())
        }
    }
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
        let base = &self.inner.base;
        let entry = &self.inner.entry;
        let (removed, component, pre_focus, was_focused, empty_stack) = {
            let mut state = base.lock();
            let Some(index) = state
                .overlay_stack
                .iter()
                .position(|o| Arc::ptr_eq(&o.inner, &entry.inner))
            else {
                return;
            };
            if let OverlayFocusRestoreState::Eligible { overlay } | OverlayFocusRestoreState::Blocked { overlay, .. } =
                &state.overlay_focus_restore
            {
                if Arc::ptr_eq(&overlay.inner, &entry.inner) {
                    state.overlay_focus_restore = OverlayFocusRestoreState::Inactive;
                }
            }
            let (component, pre_focus) = {
                let es = entry.inner.state.lock().unwrap();
                (Arc::clone(&es.component), es.pre_focus.clone())
            };
            for other in &state.overlay_stack {
                if !Arc::ptr_eq(&other.inner, &entry.inner) {
                    let mut os = other.inner.state.lock().unwrap();
                    if os.pre_focus.as_ref().is_some_and(|pf| Arc::ptr_eq(pf, &component)) {
                        os.pre_focus = pre_focus.clone();
                    }
                }
            }
            state.overlay_stack.remove(index);
            let was_focused = state
                .focused_component
                .as_ref()
                .is_some_and(|fc| Arc::ptr_eq(fc, &component));
            let empty_stack = state.overlay_stack.is_empty();
            (true, component, pre_focus, was_focused, empty_stack)
        };
        if !removed {
            return;
        }
        let _ = component;
        if was_focused {
            let top_visible = base.get_topmost_visible_overlay();
            let target = top_visible
                .map(|ov| ov.inner.state.lock().unwrap().component.clone())
                .or(pre_focus);
            base.set_focus(target);
        }
        if empty_stack {
            base.hide_terminal_cursor();
        }
        base.request_render(None);
    }

    /// Temporarily hide or show the overlay.
    pub fn set_hidden(&self, hidden: bool) {
        let base = &self.inner.base;
        let entry = &self.inner.entry;
        let (changed, component, pre_focus, options, was_focused) = {
            let mut state = base.lock();
            let mut es = entry.inner.state.lock().unwrap();
            if es.hidden == hidden {
                return;
            }
            es.hidden = hidden;
            let component = Arc::clone(&es.component);
            let pre_focus = es.pre_focus.clone();
            let options = es.options.clone();
            drop(es);
            if hidden {
                if let OverlayFocusRestoreState::Eligible { overlay }
                | OverlayFocusRestoreState::Blocked { overlay, .. } = &state.overlay_focus_restore
                {
                    if Arc::ptr_eq(&overlay.inner, &entry.inner) {
                        state.overlay_focus_restore = OverlayFocusRestoreState::Inactive;
                    }
                }
            }
            let was_focused = state
                .focused_component
                .as_ref()
                .is_some_and(|fc| Arc::ptr_eq(fc, &component));
            (true, component, pre_focus, options, was_focused)
        };
        if !changed {
            return;
        }
        if hidden {
            if was_focused {
                let top_visible = base.get_topmost_visible_overlay();
                let target = top_visible
                    .map(|ov| ov.inner.state.lock().unwrap().component.clone())
                    .or(pre_focus);
                base.set_focus(target);
            }
        } else if !options.and_then(|o| o.non_capturing).unwrap_or(false) && base.is_overlay_visible(entry) {
            let next_order = {
                let mut state = base.lock();
                state.focus_order_counter += 1;
                state.focus_order_counter
            };
            entry.inner.state.lock().unwrap().focus_order = next_order;
            base.set_focus(Some(component));
        }
        base.request_render(None);
    }

    /// Check if overlay is temporarily hidden.
    pub fn is_hidden(&self) -> bool {
        self.inner.entry.inner.state.lock().unwrap().hidden
    }

    /// Focus this overlay and bring it to the visual front.
    pub fn focus(&self) {
        let base = &self.inner.base;
        let entry = &self.inner.entry;
        let in_stack = base
            .lock()
            .overlay_stack
            .iter()
            .any(|o| Arc::ptr_eq(&o.inner, &entry.inner));
        if !in_stack || !base.is_overlay_visible(entry) {
            return;
        }
        let next_order = {
            let mut state = base.lock();
            state.focus_order_counter += 1;
            state.focus_order_counter
        };
        let component = {
            let mut es = entry.inner.state.lock().unwrap();
            es.focus_order = next_order;
            Arc::clone(&es.component)
        };
        base.set_focus(Some(component));
        base.request_render(None);
    }

    /// Release focus to the next visible capturing overlay or previous target, or to an explicit target when provided.
    pub fn unfocus(&self, options: Option<OverlayUnfocusOptions>) {
        let base = &self.inner.base;
        let entry = &self.inner.entry;
        let (component, pre_focus) = {
            let es = entry.inner.state.lock().unwrap();
            (Arc::clone(&es.component), es.pre_focus.clone())
        };
        let (is_focused, action) = {
            let mut state = base.lock();
            let is_focused = state
                .focused_component
                .as_ref()
                .is_some_and(|fc| Arc::ptr_eq(fc, &component));
            let has_pending_restore = match &state.overlay_focus_restore {
                OverlayFocusRestoreState::Eligible { overlay } | OverlayFocusRestoreState::Blocked { overlay, .. } => {
                    Arc::ptr_eq(&overlay.inner, &entry.inner)
                }
                OverlayFocusRestoreState::Inactive => false,
            };
            if !is_focused && !has_pending_restore {
                return;
            }
            if let OverlayFocusRestoreState::Blocked {
                overlay, blocked_by, ..
            } = &state.overlay_focus_restore
            {
                if Arc::ptr_eq(&overlay.inner, &entry.inner)
                    && state
                        .focused_component
                        .as_ref()
                        .is_some_and(|fc| Arc::ptr_eq(fc, blocked_by))
                {
                    let blocked_by = Arc::clone(blocked_by);
                    if let Some(ref opts) = options {
                        state.overlay_focus_restore = OverlayFocusRestoreState::Blocked {
                            overlay: overlay.clone(),
                            blocked_by,
                            resume: OverlayBlockedFocusResume::FocusTarget {
                                target: opts.target.clone(),
                            },
                        };
                    } else {
                        state.overlay_focus_restore = OverlayFocusRestoreState::Inactive;
                    }
                    (is_focused, true)
                } else {
                    (is_focused, false)
                }
            } else {
                (is_focused, false)
            }
        };
        if action {
            base.request_render(None);
            return;
        }
        base.clear_overlay_focus_restore_for(entry);
        if is_focused || options.is_some() {
            let target = match options {
                Some(opts) => opts.target,
                None => {
                    let top_visible = base.get_topmost_visible_overlay();
                    match top_visible {
                        Some(ov) if !Arc::ptr_eq(&ov.inner, &entry.inner) => {
                            Some(ov.inner.state.lock().unwrap().component.clone())
                        }
                        _ => pre_focus,
                    }
                }
            };
            base.set_focus(target);
        }
        base.request_render(None);
    }

    /// Check if this overlay currently has focus.
    pub fn is_focused(&self) -> bool {
        let base = &self.inner.base;
        let component = self.inner.entry.inner.state.lock().unwrap().component.clone();
        base.lock()
            .focused_component
            .as_ref()
            .is_some_and(|fc| Arc::ptr_eq(fc, &component))
    }

    /// Get the most recent rendered bounds for a visible overlay.
    pub fn get_bounds(&self) -> Option<OverlayBounds> {
        let base = &self.inner.base;
        let entry = &self.inner.entry;
        let in_stack = base
            .lock()
            .overlay_stack
            .iter()
            .any(|o| Arc::ptr_eq(&o.inner, &entry.inner));
        if !in_stack || !base.is_overlay_visible(entry) {
            return None;
        }
        entry.inner.state.lock().unwrap().bounds
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
        self.lock().children.push(component);
    }

    pub fn remove_child(&self, component: &Arc<dyn Component>) {
        let mut state = self.lock();
        if let Some(index) = state.children.iter().position(|c| Arc::ptr_eq(c, component)) {
            state.children.remove(index);
        }
    }

    pub fn clear(&self) {
        self.lock().children.clear();
    }

    pub fn invalidate(&self) {
        let children = self.children();
        for child in children {
            child.invalidate();
        }
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.y < 0 || event.y >= event.height {
            return None;
        }
        let mouse_children: Vec<(Arc<dyn Component>, i64)> = {
            let state = self.lock();
            if let Some(layout) = &state.mouse_layout {
                if layout.width == event.width {
                    layout
                        .children
                        .iter()
                        .map(|c| (Arc::clone(&c.component), c.height))
                        .collect()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        };
        let mouse_children = if mouse_children.is_empty() {
            let children = self.children();
            let render_width = usize::try_from(event.width.max(0)).unwrap_or(0);
            children
                .into_iter()
                .map(|c| {
                    let h = c.render(render_width).len() as i64;
                    (c, h)
                })
                .collect()
        } else {
            mouse_children
        };

        let mut child_y = 0i64;
        for (child, child_height) in mouse_children {
            if event.y >= child_y && event.y < child_y + child_height {
                let sub_event = TuiMouseEvent {
                    y: event.y - child_y,
                    height: child_height,
                    ..*event
                };
                return dispatch_mouse_event_arc(&child, &sub_event).map(|d| TuiMouseEventResult {
                    handled: Some(true),
                    capture: d.capture,
                    focus: d.focus,
                    render: d.render,
                    target: Some(d.target),
                    focus_target: d.focus_target,
                });
            }
            child_y += child_height;
        }
        None
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let children = self.children();
        let mut lines = Vec::new();
        let mut mouse_children = Vec::with_capacity(children.len());
        for child in children {
            let child_lines = child.render(width);
            mouse_children.push(ContainerMouseChild {
                component: Arc::clone(&child),
                height: child_lines.len() as i64,
            });
            lines.extend(child_lines);
        }
        self.lock().mouse_layout = Some(ContainerMouseLayout {
            width: width as i64,
            children: mouse_children,
        });
        lines
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
    if crate::terminal_image::is_image_line(base_line) {
        return base_line.to_string();
    }

    let after_start = start_col + overlay_width;
    let base = crate::utils::extract_segments(base_line, start_col, after_start, total_width - after_start, Some(true));
    let overlay = crate::utils::slice_with_width(overlay_line, 0, overlay_width, Some(true));
    let before_pad = usize::try_from(0.max(start_col - base.before_width)).unwrap_or(0);
    let overlay_pad = usize::try_from(0.max(overlay_width - overlay.width)).unwrap_or(0);
    let actual_before_width = start_col.max(base.before_width);
    let actual_overlay_width = overlay_width.max(overlay.width);
    let after_target = 0.max(total_width - actual_before_width - actual_overlay_width);
    let after_pad = usize::try_from(0.max(after_target - base.after_width)).unwrap_or(0);
    let result = format!(
        "{}{}{}{}{}{}{}{}",
        base.before,
        " ".repeat(before_pad),
        SEGMENT_RESET,
        overlay.text,
        " ".repeat(overlay_pad),
        SEGMENT_RESET,
        base.after,
        " ".repeat(after_pad)
    );

    if crate::utils::visible_width(&result) <= total_width {
        result
    } else {
        crate::utils::slice_by_column(&result, 0, total_width, Some(true))
    }
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

    /// Drop the text selection and multi-click history, e.g. before the host replaces the transcript.
    fn reset_text_selection(&self) {}
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

#[derive(Clone, Copy, serde::Serialize)]
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
    fn get_exit_restore_prefix(&self, base: &TuiBase) -> String {
        base.default_exit_restore_prefix()
    }
    fn get_exit_restore_suffix(&self) -> String {
        String::new()
    }
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
        TuiBase {
            inner: Arc::new(TuiBaseInner {
                terminal,
                log_directory: log_directory.map(ToString::to_string),
                container: Container::new(),
                overrides: Mutex::new(None),
                state: Mutex::new(TuiBaseState {
                    focused_component: None,
                    input_listeners: Vec::new(),
                    on_debug: None,
                    render_requested: false,
                    immediate_render_scheduled: false,
                    render_timer: None,
                    last_render_at: 0.0,
                    show_hardware_cursor: show_hardware_cursor.unwrap_or(false),
                    clear_on_shrink: false,
                    full_redraw_count: 0,
                    stopped: false,
                    pending_terminal_color_queries: Vec::new(),
                    terminal_color_scheme_listeners: Vec::new(),
                    terminal_color_scheme_notifications_enabled: false,
                    focus_order_counter: 0,
                    overlay_stack: Vec::new(),
                    rendered_overlay_layouts: Vec::new(),
                    overlay_focus_restore: OverlayFocusRestoreState::Inactive,
                }),
            }),
        }
    }

    pub(crate) fn bind_overrides(&self, overrides: Arc<dyn TuiBaseOverrides>) {
        *self.inner.overrides.lock().unwrap() = Some(Arc::downgrade(&overrides));
    }

    fn overrides(&self) -> Option<Arc<dyn TuiBaseOverrides>> {
        self.inner.overrides.lock().unwrap().as_ref().and_then(Weak::upgrade)
    }

    fn lock(&self) -> MutexGuard<'_, TuiBaseState> {
        self.inner.state.lock().unwrap()
    }

    pub fn mode(&self) -> TuiMode {
        self.overrides().map(|o| o.mode()).unwrap_or(TuiMode::Regular)
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
        {
            let mut state = self.lock();
            if state.show_hardware_cursor == enabled {
                return;
            }
            state.show_hardware_cursor = enabled;
        }
        if !enabled {
            self.hide_terminal_cursor();
        }
        self.request_render(None);
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
        self.set_focus_internal(component, OverlayFocusRestorePolicy::Clear);
    }

    fn set_focus_internal(
        &self,
        component: Option<Arc<dyn Component>>,
        overlay_focus_restore: OverlayFocusRestorePolicy,
    ) {
        let previous_focus = self.lock().focused_component.clone();
        let mut next_focus = component;

        let stack = self.lock().overlay_stack.clone();
        let previous_focused_overlay = previous_focus
            .as_ref()
            .and_then(|prev| {
                stack.iter().find(|entry| {
                    let entry_comp = entry.inner.state.lock().unwrap().component.clone();
                    Arc::ptr_eq(&entry_comp, prev) && self.is_overlay_visible(entry)
                })
            })
            .cloned();

        let next_focus_is_overlay = next_focus.as_ref().is_some_and(|nf| {
            stack.iter().any(|entry| {
                let entry_comp = entry.inner.state.lock().unwrap().component.clone();
                Arc::ptr_eq(&entry_comp, nf)
            })
        });

        let restore_state = self.get_visible_overlay_focus_restore();
        if let Some(ref nf) = next_focus {
            if !next_focus_is_overlay {
                if let OverlayFocusRestoreState::Blocked {
                    overlay,
                    blocked_by,
                    resume,
                } = &restore_state
                {
                    if previous_focus.as_ref().is_some_and(|pf| Arc::ptr_eq(pf, blocked_by)) {
                        if matches!(resume, OverlayBlockedFocusResume::FocusTarget { .. })
                            || !self.is_component_mounted(blocked_by)
                        {
                            next_focus = self.resolve_blocked_overlay_focus_resume(&restore_state);
                        } else {
                            self.lock().overlay_focus_restore = OverlayFocusRestoreState::Blocked {
                                overlay: overlay.clone(),
                                blocked_by: Arc::clone(nf),
                                resume: resume.clone(),
                            };
                        }
                    }
                } else if let Some(ref prev_ov) = previous_focused_overlay {
                    let matches_restore = match &restore_state {
                        OverlayFocusRestoreState::Eligible { overlay }
                        | OverlayFocusRestoreState::Blocked { overlay, .. } => {
                            Arc::ptr_eq(&overlay.inner, &prev_ov.inner)
                        }
                        OverlayFocusRestoreState::Inactive => false,
                    };
                    if matches_restore && !self.is_overlay_focus_ancestor(prev_ov, nf) {
                        self.lock().overlay_focus_restore = OverlayFocusRestoreState::Blocked {
                            overlay: prev_ov.clone(),
                            blocked_by: Arc::clone(nf),
                            resume: OverlayBlockedFocusResume::RestoreOverlay,
                        };
                    }
                }
            }
        } else {
            if let OverlayFocusRestoreState::Blocked { blocked_by, .. } = &restore_state {
                if previous_focus.as_ref().is_some_and(|pf| Arc::ptr_eq(pf, blocked_by)) {
                    next_focus = self.resolve_blocked_overlay_focus_resume(&restore_state);
                }
            } else if matches!(overlay_focus_restore, OverlayFocusRestorePolicy::Clear) {
                self.clear_overlay_focus_restore();
            }
        }

        let old_focused = self.lock().focused_component.clone();
        if let Some(ref prev) = old_focused {
            if let Some(focusable) = prev.as_focusable() {
                focusable.set_focused(false);
            }
        }

        self.lock().focused_component = next_focus.clone();

        if let Some(ref nf) = next_focus {
            if let Some(focusable) = nf.as_focusable() {
                focusable.set_focused(true);
            }
        }

        let focused_overlay = next_focus.as_ref().and_then(|nf| {
            let stack_now = self.lock().overlay_stack.clone();
            stack_now.into_iter().find(|entry| {
                let entry_comp = entry.inner.state.lock().unwrap().component.clone();
                Arc::ptr_eq(&entry_comp, nf) && self.is_overlay_visible(entry)
            })
        });
        if let Some(ov) = focused_overlay {
            self.lock().overlay_focus_restore = OverlayFocusRestoreState::Eligible { overlay: ov };
        }
    }

    fn clear_overlay_focus_restore(&self) {
        self.lock().overlay_focus_restore = OverlayFocusRestoreState::Inactive;
    }

    fn clear_overlay_focus_restore_for(&self, overlay: &OverlayStackEntry) {
        let mut state = self.lock();
        let should_clear = match &state.overlay_focus_restore {
            OverlayFocusRestoreState::Eligible { overlay: ov }
            | OverlayFocusRestoreState::Blocked { overlay: ov, .. } => Arc::ptr_eq(&ov.inner, &overlay.inner),
            OverlayFocusRestoreState::Inactive => false,
        };
        if should_clear {
            state.overlay_focus_restore = OverlayFocusRestoreState::Inactive;
        }
    }

    fn resolve_blocked_overlay_focus_resume(
        &self,
        restore_state: &OverlayFocusRestoreState,
    ) -> Option<Arc<dyn Component>> {
        match restore_state {
            OverlayFocusRestoreState::Blocked { overlay, resume, .. } => match resume {
                OverlayBlockedFocusResume::RestoreOverlay => {
                    Some(overlay.inner.state.lock().unwrap().component.clone())
                }
                OverlayBlockedFocusResume::FocusTarget { target } => {
                    self.clear_overlay_focus_restore();
                    target.clone()
                }
            },
            _ => None,
        }
    }

    fn get_visible_overlay_focus_restore(&self) -> OverlayFocusRestoreState {
        let restore_state = self.lock().overlay_focus_restore.clone();
        let overlay = match &restore_state {
            OverlayFocusRestoreState::Inactive => return restore_state,
            OverlayFocusRestoreState::Eligible { overlay } | OverlayFocusRestoreState::Blocked { overlay, .. } => {
                overlay.clone()
            }
        };
        let in_stack = self
            .lock()
            .overlay_stack
            .iter()
            .any(|o| Arc::ptr_eq(&o.inner, &overlay.inner));
        if !in_stack || !self.is_overlay_visible(&overlay) {
            return OverlayFocusRestoreState::Inactive;
        }
        restore_state
    }

    fn is_overlay_focus_ancestor(&self, entry: &OverlayStackEntry, component: &Arc<dyn Component>) -> bool {
        let stack = self.lock().overlay_stack.clone();
        let mut visited: Vec<Arc<dyn Component>> = Vec::new();
        let mut current = entry.inner.state.lock().unwrap().pre_focus.clone();
        while let Some(curr) = current {
            if visited.iter().any(|v| Arc::ptr_eq(v, &curr)) {
                break;
            }
            if Arc::ptr_eq(&curr, component) {
                return true;
            }
            visited.push(Arc::clone(&curr));
            current = stack.iter().find_map(|ov| {
                let es = ov.inner.state.lock().unwrap();
                if Arc::ptr_eq(&es.component, &curr) {
                    es.pre_focus.clone()
                } else {
                    None
                }
            });
        }
        false
    }

    fn retarget_overlay_pre_focus(&self, removed: &OverlayStackEntry) {
        let (removed_comp, removed_pre_focus) = {
            let es = removed.inner.state.lock().unwrap();
            (Arc::clone(&es.component), es.pre_focus.clone())
        };
        let stack = self.lock().overlay_stack.clone();
        for overlay in stack {
            if !Arc::ptr_eq(&overlay.inner, &removed.inner) {
                let mut es = overlay.inner.state.lock().unwrap();
                if es.pre_focus.as_ref().is_some_and(|pf| Arc::ptr_eq(pf, &removed_comp)) {
                    es.pre_focus = removed_pre_focus.clone();
                }
            }
        }
    }

    pub(crate) fn get_mounted_roots(&self) -> Vec<Arc<dyn Component>> {
        if let Some(overrides) = self.overrides() {
            overrides.get_mounted_roots(self)
        } else {
            self.children()
        }
    }

    fn is_component_mounted(&self, component: &Arc<dyn Component>) -> bool {
        self.get_mounted_roots()
            .iter()
            .any(|child| self.contains_component(child, component))
    }

    fn contains_component(&self, root: &Arc<dyn Component>, target: &Arc<dyn Component>) -> bool {
        if Arc::ptr_eq(root, target) {
            return true;
        }
        let Some(children) = root.container_children() else {
            return false;
        };
        children.iter().any(|child| self.contains_component(child, target))
    }

    /// Show an overlay component with configurable positioning and sizing.
    /// Returns a handle to control the overlay's visibility.
    pub fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
        let entry = {
            let mut state = self.lock();
            state.focus_order_counter += 1;
            let focus_order = state.focus_order_counter;
            let pre_focus = state.focused_component.clone();
            let entry = OverlayStackEntry {
                inner: Arc::new(OverlayStackEntryInner {
                    state: Mutex::new(OverlayStackEntryState {
                        component: Arc::clone(&component),
                        options: options.clone(),
                        pre_focus,
                        hidden: false,
                        focus_order,
                        bounds: None,
                    }),
                }),
            };
            state.overlay_stack.push(entry.clone());
            entry
        };
        let non_capturing = options.and_then(|o| o.non_capturing).unwrap_or(false);
        if !non_capturing && self.is_overlay_visible(&entry) {
            self.set_focus(Some(component));
        }
        self.hide_terminal_cursor();
        self.request_render(None);
        OverlayHandle {
            inner: Arc::new(OverlayHandleInner {
                base: self.clone(),
                entry,
            }),
        }
    }

    /// Hide the topmost overlay and restore previous focus.
    pub fn hide_overlay(&self) {
        let (overlay_comp, overlay_pre_focus, was_focused, empty_stack) = {
            let state = self.lock();
            let Some(overlay) = state.overlay_stack.last().cloned() else {
                return;
            };
            drop(state);
            self.clear_overlay_focus_restore_for(&overlay);
            self.retarget_overlay_pre_focus(&overlay);
            let (overlay_comp, overlay_pre_focus) = {
                let es = overlay.inner.state.lock().unwrap();
                (Arc::clone(&es.component), es.pre_focus.clone())
            };
            let mut state = self.lock();
            state.overlay_stack.pop();
            let was_focused = state
                .focused_component
                .as_ref()
                .is_some_and(|fc| Arc::ptr_eq(fc, &overlay_comp));
            let empty_stack = state.overlay_stack.is_empty();
            (overlay_comp, overlay_pre_focus, was_focused, empty_stack)
        };
        let _ = overlay_comp;
        if was_focused {
            let top_visible = self.get_topmost_visible_overlay();
            let target = top_visible
                .map(|ov| ov.inner.state.lock().unwrap().component.clone())
                .or(overlay_pre_focus);
            self.set_focus(target);
        }
        if empty_stack {
            self.hide_terminal_cursor();
        }
        self.request_render(None);
    }

    /// Hide the cursor while running. After stop(), the shell owns the cursor and it must stay visible.
    fn hide_terminal_cursor(&self) {
        if !self.stopped() {
            self.inner.terminal.hide_cursor();
        }
    }

    /// Check if there are any visible overlays.
    pub fn has_overlay(&self) -> bool {
        let stack = self.lock().overlay_stack.clone();
        stack.iter().any(|o| self.is_overlay_visible(o))
    }

    pub fn has_overlay_entries(&self) -> bool {
        !self.lock().overlay_stack.is_empty()
    }

    /// Check if the focused component is a visible overlay.
    pub(crate) fn is_overlay_focused(&self) -> bool {
        let (focused, stack) = {
            let state = self.lock();
            (state.focused_component.clone(), state.overlay_stack.clone())
        };
        let Some(focused) = focused else {
            return false;
        };
        stack.iter().any(|entry| {
            let comp = entry.inner.state.lock().unwrap().component.clone();
            Arc::ptr_eq(&comp, &focused) && self.is_overlay_visible(entry)
        })
    }

    /// Keep overlay containers as keyboard focus owners when a nested control is clicked.
    pub(crate) fn resolve_mouse_focus_target(&self, component: Arc<dyn Component>) -> Arc<dyn Component> {
        let stack = self.lock().overlay_stack.clone();
        for overlay in stack.iter().rev() {
            if self.is_overlay_visible(overlay) {
                let ov_comp = overlay.inner.state.lock().unwrap().component.clone();
                if self.contains_component(&ov_comp, &component) {
                    return ov_comp;
                }
            }
        }
        component
    }

    /// Dispatch to the visually topmost overlay under the pointer.
    pub(crate) fn dispatch_mouse_to_overlay(&self, event: &TuiMouseEvent) -> OverlayMouseDispatch {
        let layouts: Vec<(OverlayStackEntry, i64, i64, i64, i64)> = {
            let state = self.lock();
            state
                .rendered_overlay_layouts
                .iter()
                .map(|l| (l.entry.clone(), l.row, l.col, l.width, l.height))
                .collect()
        };
        for (entry, row, col, width, height) in layouts.into_iter().rev() {
            if event.screen_x < col
                || event.screen_x >= col + width
                || event.screen_y < row
                || event.screen_y >= row + height
            {
                continue;
            }
            let ov_comp = entry.inner.state.lock().unwrap().component.clone();
            let local_event = TuiMouseEvent {
                x: event.screen_x - col,
                y: event.screen_y - row,
                width,
                height,
                ..*event
            };
            let result = dispatch_mouse_event_arc(&ov_comp, &local_event).map(|mut r| {
                if r.focus.unwrap_or(false) {
                    r.focus_target = Some(Arc::clone(&ov_comp));
                }
                r
            });
            return OverlayMouseDispatch { hit: true, result };
        }
        OverlayMouseDispatch {
            hit: false,
            result: None,
        }
    }

    fn is_overlay_visible(&self, entry: &OverlayStackEntry) -> bool {
        let (hidden, visible_cb) = {
            let es = entry.inner.state.lock().unwrap();
            (es.hidden, es.options.as_ref().and_then(|o| o.visible.clone()))
        };
        if hidden {
            return false;
        }
        if let Some(cb) = visible_cb {
            return cb(self.inner.terminal.columns(), self.inner.terminal.rows());
        }
        true
    }

    fn get_topmost_visible_overlay(&self) -> Option<OverlayStackEntry> {
        let stack = self.lock().overlay_stack.clone();
        let mut topmost: Option<(OverlayStackEntry, i64)> = None;
        for overlay in stack {
            let (non_capturing, focus_order) = {
                let es = overlay.inner.state.lock().unwrap();
                (
                    es.options.as_ref().and_then(|o| o.non_capturing).unwrap_or(false),
                    es.focus_order,
                )
            };
            if non_capturing || !self.is_overlay_visible(&overlay) {
                continue;
            }
            if topmost.as_ref().is_none_or(|(_, top_order)| focus_order > *top_order) {
                topmost = Some((overlay, focus_order));
            }
        }
        topmost.map(|(entry, _)| entry)
    }

    pub fn invalidate(&self) {
        for root in self.get_mounted_roots() {
            root.invalidate();
        }
        let stack = self.lock().overlay_stack.clone();
        for overlay in stack {
            let comp = overlay.inner.state.lock().unwrap().component.clone();
            comp.invalidate();
        }
    }

    pub fn start(&self) {
        self.lock().stopped = false;
        register_tui_exit_restore(&self.inner);
        self.before_terminal_start();
        let this_input = self.clone();
        let this_resize = self.clone();
        self.inner.terminal.start(
            Arc::new(move |data| this_input.handle_terminal_input(data)),
            Arc::new(move || this_resize.request_render(None)),
        );
        self.after_terminal_start();
        self.inner.terminal.hide_cursor();
        if self.lock().terminal_color_scheme_notifications_enabled {
            self.inner.terminal.write("\x1b[?2031h");
        }
        self.query_cell_size();
        self.request_render(None);
    }

    pub fn add_input_listener(&self, listener: TuiInputListener) -> pi_js::Unsubscribe {
        {
            let mut state = self.lock();
            if !state.input_listeners.iter().any(|l| Arc::ptr_eq(l, &listener)) {
                state.input_listeners.push(Arc::clone(&listener));
            }
        }
        let this = self.clone();
        pi_js::Unsubscribe::new(move || {
            this.remove_input_listener(&listener);
        })
    }

    pub fn remove_input_listener(&self, listener: &TuiInputListener) {
        self.lock().input_listeners.retain(|l| !Arc::ptr_eq(l, listener));
    }

    pub fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        {
            let mut state = self.lock();
            if !state
                .terminal_color_scheme_listeners
                .iter()
                .any(|l| Arc::ptr_eq(l, &listener))
            {
                state.terminal_color_scheme_listeners.push(Arc::clone(&listener));
            }
        }
        let this = self.clone();
        pi_js::Unsubscribe::new(move || {
            this.lock()
                .terminal_color_scheme_listeners
                .retain(|l| !Arc::ptr_eq(l, &listener));
        })
    }

    pub fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        let stopped = {
            let mut state = self.lock();
            if state.terminal_color_scheme_notifications_enabled == enabled {
                return;
            }
            state.terminal_color_scheme_notifications_enabled = enabled;
            state.stopped
        };
        if !stopped {
            self.inner
                .terminal
                .write(if enabled { "\x1b[?2031h" } else { "\x1b[?2031l" });
        }
    }

    fn query_cell_size(&self) {
        if crate::terminal_image::get_capabilities().images.is_none() {
            return;
        }
        // Query terminal for cell size in pixels: CSI 16 t
        // Response format: CSI 6 ; height ; width t
        self.inner.terminal.write("\x1b[16t");
    }

    pub fn stop(&self, options: Option<TuiStopOptions>) {
        let opts = options.unwrap_or_default();
        let notifications_enabled = {
            let mut state = self.lock();
            state.stopped = true;
            state.terminal_color_scheme_notifications_enabled
        };
        unregister_tui_exit_restore(&self.inner);
        self.cancel_render_timer();
        if notifications_enabled {
            self.inner.terminal.write("\x1b[?2031l");
        }
        self.before_terminal_stop(&opts);
        self.inner.terminal.show_cursor();
        self.inner.terminal.stop();
        self.after_terminal_stop(&opts);
    }

    pub fn render_now(&self, force: Option<bool>) {
        if force.unwrap_or(false) {
            self.reset_render_state();
        }
        let timer = {
            let mut state = self.lock();
            state.render_requested = false;
            state.last_render_at = pi_js::time::performance_now();
            state.render_timer.take()
        };
        if let Some(t) = timer {
            t.clear();
        }
        self.do_render();
    }

    pub fn request_render(&self, force: Option<bool>) {
        if force.unwrap_or(false) {
            self.reset_render_state();
            self.request_immediate_render();
            return;
        }
        {
            let mut state = self.lock();
            if state.render_requested {
                return;
            }
            state.render_requested = true;
        }
        let this = self.clone();
        pi_js::time::set_timeout(0, move || {
            this.schedule_render();
        });
    }

    fn request_immediate_render(&self) {
        self.cancel_render_timer();
        {
            let mut state = self.lock();
            state.render_requested = true;
            if state.immediate_render_scheduled {
                return;
            }
            state.immediate_render_scheduled = true;
        }
        let this = self.clone();
        pi_js::time::set_timeout(0, move || {
            {
                let mut state = this.lock();
                state.immediate_render_scheduled = false;
                if state.stopped || !state.render_requested {
                    return;
                }
            }
            // A previously queued scheduleRender() can create a timer before this callback runs.
            this.cancel_render_timer();
            {
                let mut state = this.lock();
                if state.stopped || !state.render_requested {
                    return;
                }
                state.render_requested = false;
                state.last_render_at = pi_js::time::performance_now();
            }
            this.do_render();
        });
    }

    fn cancel_render_timer(&self) {
        let timer = self.lock().render_timer.take();
        if let Some(t) = timer {
            t.clear();
        }
    }

    fn schedule_render(&self) {
        let delay_ms = {
            let state = self.lock();
            if state.stopped || state.render_timer.is_some() || !state.render_requested {
                return;
            }
            let elapsed = pi_js::time::performance_now() - state.last_render_at;
            let delay = (Self::MIN_RENDER_INTERVAL_MS as f64 - elapsed).max(0.0);
            delay.ceil().max(0.0) as u64
        };
        let this = self.clone();
        let timer = pi_js::time::set_timeout(delay_ms, move || {
            let should_render = {
                let mut state = this.lock();
                state.render_timer = None;
                if state.stopped || !state.render_requested {
                    false
                } else {
                    state.render_requested = false;
                    state.last_render_at = pi_js::time::performance_now();
                    true
                }
            };
            if !should_render {
                return;
            }
            this.do_render();
            if this.lock().render_requested {
                this.schedule_render();
            }
        });
        let mut state = self.lock();
        if state.stopped || !state.render_requested || state.render_timer.is_some() {
            timer.clear();
        } else {
            state.render_timer = Some(timer);
        }
    }

    pub(crate) fn do_render(&self) {
        if let Some(overrides) = self.overrides() {
            overrides.do_render();
        }
    }

    pub(crate) fn reset_render_state(&self) {
        if let Some(overrides) = self.overrides() {
            overrides.reset_render_state();
        }
    }

    pub(crate) fn before_terminal_start(&self) {
        if let Some(overrides) = self.overrides() {
            overrides.before_terminal_start();
        }
    }

    pub(crate) fn after_terminal_start(&self) {
        if let Some(overrides) = self.overrides() {
            overrides.after_terminal_start();
        }
    }

    pub(crate) fn before_terminal_stop(&self, options: &TuiStopOptions) {
        if let Some(overrides) = self.overrides() {
            overrides.before_terminal_stop(options);
        }
    }

    pub(crate) fn after_terminal_stop(&self, options: &TuiStopOptions) {
        if let Some(overrides) = self.overrides() {
            overrides.after_terminal_stop(options);
        }
    }

    fn handle_terminal_input(&self, data: &str) {
        if self.consume_terminal_color_response(data) {
            return;
        }
        if self.consume_terminal_color_scheme_report(data) {
            return;
        }

        let mut current_data = data.to_string();
        let listeners = self.lock().input_listeners.clone();
        if !listeners.is_empty() {
            for listener in listeners {
                if let Some(result) = listener(&current_data) {
                    if result.consume.unwrap_or(false) {
                        return;
                    }
                    if let Some(next) = result.data {
                        current_data = next;
                    }
                }
            }
            if current_data.is_empty() {
                return;
            }
        }

        if self.consume_cell_size_response(&current_data) {
            return;
        }

        if crate::keys::matches_key(&current_data, "shift+ctrl+d") {
            let on_debug = self.on_debug();
            if let Some(cb) = on_debug {
                cb();
                return;
            }
        }

        let focused = self.get_focused_component();
        let stack = self.lock().overlay_stack.clone();
        let focused_overlay = focused.as_ref().and_then(|fc| {
            stack.iter().find(|o| {
                let comp = o.inner.state.lock().unwrap().component.clone();
                Arc::ptr_eq(&comp, fc)
            })
        });
        if let Some(ov) = focused_overlay {
            if !self.is_overlay_visible(ov) {
                if let Some(top_visible) = self.get_topmost_visible_overlay() {
                    let top_comp = top_visible.inner.state.lock().unwrap().component.clone();
                    self.set_focus(Some(top_comp));
                } else {
                    let pre_focus = ov.inner.state.lock().unwrap().pre_focus.clone();
                    self.set_focus_internal(pre_focus, OverlayFocusRestorePolicy::Preserve);
                }
            }
        }

        let current_focused = self.get_focused_component();
        let focus_is_overlay = current_focused.as_ref().is_some_and(|fc| {
            let stack_now = self.lock().overlay_stack.clone();
            stack_now.iter().any(|o| {
                let comp = o.inner.state.lock().unwrap().component.clone();
                Arc::ptr_eq(&comp, fc)
            })
        });
        if !focus_is_overlay {
            let restore_state = self.get_visible_overlay_focus_restore();
            match restore_state {
                OverlayFocusRestoreState::Eligible { overlay } => {
                    let comp = overlay.inner.state.lock().unwrap().component.clone();
                    self.set_focus(Some(comp));
                }
                OverlayFocusRestoreState::Blocked {
                    overlay,
                    blocked_by,
                    resume,
                } => {
                    let still_blocked_by_focused =
                        current_focused.as_ref().is_some_and(|fc| Arc::ptr_eq(fc, &blocked_by));
                    if !still_blocked_by_focused {
                        match resume {
                            OverlayBlockedFocusResume::RestoreOverlay => {
                                let comp = overlay.inner.state.lock().unwrap().component.clone();
                                self.set_focus(Some(comp));
                            }
                            OverlayBlockedFocusResume::FocusTarget { target } => {
                                self.clear_overlay_focus_restore();
                                self.set_focus(target);
                            }
                        }
                    }
                }
                OverlayFocusRestoreState::Inactive => {}
            }
        }

        if let Some(comp) = self.get_focused_component() {
            if comp.has_handle_input() {
                if crate::keys::is_key_release(&current_data) && !comp.wants_key_release() {
                    return;
                }
                comp.handle_input(&current_data);
                self.request_immediate_render();
            }
        }
    }

    fn consume_terminal_color_response(&self, data: &str) -> bool {
        enum Action {
            None,
            RetTrue,
            Complete {
                deliver: Option<Arc<dyn Fn(TerminalColors) + Send + Sync>>,
                timer: Option<pi_js::time::Timeout>,
                result: TerminalColors,
            },
        }
        if DEVICE_ATTRIBUTES_RESPONSE_PATTERN.is_match(data) {
            let action = {
                let mut state = self.lock();
                if state.pending_terminal_color_queries.is_empty() {
                    Action::None
                } else {
                    let mut query = state.pending_terminal_color_queries.remove(0);
                    let result = self.terminal_color_query_result(&query);
                    let deliver = query.deliver.take();
                    let timer = query.timer.take();
                    Action::Complete { deliver, timer, result }
                }
            };
            return match action {
                Action::None => false,
                Action::RetTrue => true,
                Action::Complete { deliver, timer, result } => {
                    if let Some(t) = timer {
                        t.clear();
                    }
                    if let Some(cb) = deliver {
                        cb(result);
                    }
                    true
                }
            };
        }

        {
            let state = self.lock();
            if state.pending_terminal_color_queries.is_empty() {
                return false;
            }
        }

        let Some(response) = crate::terminal_colors::parse_osc_color_response(data) else {
            return false;
        };

        let key = match response.target {
            crate::terminal_colors::OscColorTarget::Foreground => "foreground".to_string(),
            crate::terminal_colors::OscColorTarget::Background => "background".to_string(),
            crate::terminal_colors::OscColorTarget::Index(idx) => idx.to_string(),
        };

        let action = {
            let mut state = self.lock();
            let Some(query) = state.pending_terminal_color_queries.first_mut() else {
                return false;
            };
            if query.deliver.is_none() || query.replied.contains(&key) {
                Action::RetTrue
            } else {
                query.replied.insert(key);
                match response.target {
                    crate::terminal_colors::OscColorTarget::Foreground => {
                        query.foreground = response.rgb;
                    }
                    crate::terminal_colors::OscColorTarget::Background => {
                        query.background = response.rgb;
                    }
                    crate::terminal_colors::OscColorTarget::Index(idx) => {
                        if idx >= 0 && idx < TERMINAL_PALETTE_SIZE {
                            query.palette[idx as usize] = response.rgb;
                        }
                    }
                }
                if query.replied.len() as i64 == TERMINAL_COLOR_REPLY_COUNT {
                    let result = self.terminal_color_query_result(query);
                    let deliver = query.deliver.take();
                    let timer = query.timer.take();
                    Action::Complete { deliver, timer, result }
                } else {
                    Action::RetTrue
                }
            }
        };

        match action {
            Action::None => false,
            Action::RetTrue => true,
            Action::Complete { deliver, timer, result } => {
                if let Some(t) = timer {
                    t.clear();
                }
                if let Some(cb) = deliver {
                    cb(result);
                }
                true
            }
        }
    }

    fn terminal_color_query_result(&self, query: &PendingTerminalColorQuery) -> TerminalColors {
        let all_palette =
            query.palette.len() as i64 == TERMINAL_PALETTE_SIZE && query.palette.iter().all(Option::is_some);
        let palette = if all_palette {
            Some(query.palette.iter().filter_map(|c| *c).collect())
        } else {
            None
        };
        TerminalColors {
            foreground: query.foreground,
            background: query.background,
            palette,
        }
    }

    fn complete_terminal_color_query(&self, query: &mut PendingTerminalColorQuery) {
        let deliver = query.deliver.take();
        let timer = query.timer.take();
        let result = self.terminal_color_query_result(query);
        if let Some(t) = timer {
            t.clear();
        }
        if let Some(cb) = deliver {
            cb(result);
        }
    }

    fn consume_terminal_color_scheme_report(&self, data: &str) -> bool {
        let Some(scheme) = crate::terminal_colors::parse_terminal_color_scheme_report(data) else {
            return false;
        };
        let listeners = self.lock().terminal_color_scheme_listeners.clone();
        for listener in listeners {
            listener(scheme);
        }
        true
    }

    fn consume_cell_size_response(&self, data: &str) -> bool {
        let Some(caps) = CELL_SIZE_RESPONSE_REGEX.captures(data) else {
            return false;
        };
        let Some(height_px) = caps.get(1).and_then(|m| m.as_str().parse::<i64>().ok()) else {
            return false;
        };
        let Some(width_px) = caps.get(2).and_then(|m| m.as_str().parse::<i64>().ok()) else {
            return false;
        };
        if height_px <= 0 || width_px <= 0 {
            return true;
        }
        crate::terminal_image::set_cell_dimensions(crate::terminal_image::CellDimensions { width_px, height_px });
        self.invalidate();
        self.request_render(None);
        true
    }

    fn resolve_overlay_layout(
        &self,
        options: Option<&OverlayOptions>,
        overlay_height: i64,
        term_width: i64,
        term_height: i64,
    ) -> ResolvedOverlayLayout {
        let default_opt = OverlayOptions::default();
        let opt = options.unwrap_or(&default_opt);

        let (margin_top, margin_right, margin_bottom, margin_left) = match opt.margin {
            Some(OverlayMarginValue::All(m)) => {
                let v = 0.max(m);
                (v, v, v, v)
            }
            Some(OverlayMarginValue::Sides(m)) => (
                0.max(m.top.unwrap_or(0)),
                0.max(m.right.unwrap_or(0)),
                0.max(m.bottom.unwrap_or(0)),
                0.max(m.left.unwrap_or(0)),
            ),
            None => (0, 0, 0, 0),
        };

        let avail_width = 1.max(term_width - margin_left - margin_right);
        let avail_height = 1.max(term_height - margin_top - margin_bottom);

        let mut width = parse_size_value(opt.width.as_ref(), term_width)
            .map(|v| v as i64)
            .unwrap_or_else(|| 80.min(avail_width));
        if let Some(min_width) = opt.min_width {
            width = width.max(min_width);
        }
        width = 1.max(width.min(avail_width));

        let mut max_height = parse_size_value(opt.max_height.as_ref(), term_height).map(|v| v as i64);
        if let Some(mh) = max_height {
            max_height = Some(1.max(mh.min(avail_height)));
        }

        let effective_height = match max_height {
            Some(mh) => overlay_height.min(mh),
            None => overlay_height,
        };

        let anchor = opt.anchor.unwrap_or(OverlayAnchor::Center);

        let mut row = match opt.row.as_ref() {
            Some(SizeValue::Percent(raw)) => {
                if let Some(caps) = PERCENT_SIZE_REGEX.captures(raw) {
                    if let Some(pct) = caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()) {
                        let max_row = 0.max(avail_height - effective_height);
                        margin_top + ((max_row as f64) * (pct / 100.0)).floor() as i64
                    } else {
                        self.resolve_anchor_row(OverlayAnchor::Center, effective_height, avail_height, margin_top)
                    }
                } else {
                    self.resolve_anchor_row(OverlayAnchor::Center, effective_height, avail_height, margin_top)
                }
            }
            Some(SizeValue::Absolute(n)) => *n as i64,
            None => self.resolve_anchor_row(anchor, effective_height, avail_height, margin_top),
        };

        let mut col = match opt.col.as_ref() {
            Some(SizeValue::Percent(raw)) => {
                if let Some(caps) = PERCENT_SIZE_REGEX.captures(raw) {
                    if let Some(pct) = caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()) {
                        let max_col = 0.max(avail_width - width);
                        margin_left + ((max_col as f64) * (pct / 100.0)).floor() as i64
                    } else {
                        self.resolve_anchor_col(OverlayAnchor::Center, width, avail_width, margin_left)
                    }
                } else {
                    self.resolve_anchor_col(OverlayAnchor::Center, width, avail_width, margin_left)
                }
            }
            Some(SizeValue::Absolute(n)) => *n as i64,
            None => self.resolve_anchor_col(anchor, width, avail_width, margin_left),
        };

        if let Some(oy) = opt.offset_y {
            row += oy;
        }
        if let Some(ox) = opt.offset_x {
            col += ox;
        }

        row = margin_top.max(row.min(term_height - margin_bottom - effective_height));
        col = margin_left.max(col.min(term_width - margin_right - width));

        ResolvedOverlayLayout {
            width,
            row,
            col,
            max_height,
        }
    }

    fn resolve_anchor_row(&self, anchor: OverlayAnchor, height: i64, avail_height: i64, margin_top: i64) -> i64 {
        match anchor {
            OverlayAnchor::TopLeft | OverlayAnchor::TopCenter | OverlayAnchor::TopRight => margin_top,
            OverlayAnchor::BottomLeft | OverlayAnchor::BottomCenter | OverlayAnchor::BottomRight => {
                margin_top + avail_height - height
            }
            OverlayAnchor::LeftCenter | OverlayAnchor::Center | OverlayAnchor::RightCenter => {
                margin_top + ((avail_height - height) as f64 / 2.0).floor() as i64
            }
        }
    }

    fn resolve_anchor_col(&self, anchor: OverlayAnchor, width: i64, avail_width: i64, margin_left: i64) -> i64 {
        match anchor {
            OverlayAnchor::TopLeft | OverlayAnchor::LeftCenter | OverlayAnchor::BottomLeft => margin_left,
            OverlayAnchor::TopRight | OverlayAnchor::RightCenter | OverlayAnchor::BottomRight => {
                margin_left + avail_width - width
            }
            OverlayAnchor::TopCenter | OverlayAnchor::Center | OverlayAnchor::BottomCenter => {
                margin_left + ((avail_width - width) as f64 / 2.0).floor() as i64
            }
        }
    }

    /// Composite all overlays into content lines (sorted by focusOrder, higher = on top).
    pub(crate) fn composite_overlays(&self, lines: Vec<String>, term_width: i64, term_height: i64) -> Vec<String> {
        let stack = self.lock().overlay_stack.clone();
        if stack.is_empty() {
            self.lock().rendered_overlay_layouts.clear();
            return lines;
        }
        let mut result = lines;

        for entry in &stack {
            entry.inner.state.lock().unwrap().bounds = None;
        }

        let mut visible_entries: Vec<(OverlayStackEntry, i64)> = Vec::new();
        for entry in &stack {
            if self.is_overlay_visible(entry) {
                let order = entry.inner.state.lock().unwrap().focus_order;
                visible_entries.push((entry.clone(), order));
            }
        }
        visible_entries.sort_by_key(|(_, order)| *order);

        struct RenderedItem {
            entry: OverlayStackEntry,
            overlay_lines: Vec<String>,
            row: i64,
            col: i64,
            w: i64,
        }

        let mut rendered: Vec<RenderedItem> = Vec::with_capacity(visible_entries.len());
        let mut min_lines_needed = result.len() as i64;

        for (entry, _) in visible_entries {
            let (component, options) = {
                let es = entry.inner.state.lock().unwrap();
                (Arc::clone(&es.component), es.options.clone())
            };
            let initial = self.resolve_overlay_layout(options.as_ref(), 0, term_width, term_height);
            let render_width = usize::try_from(initial.width.max(0)).unwrap_or(0);
            let mut overlay_lines = self.resolve_fake_cursors(component.render(render_width));
            if let Some(mh) = initial.max_height {
                let mh_usize = usize::try_from(mh.max(0)).unwrap_or(0);
                if overlay_lines.len() > mh_usize {
                    overlay_lines.truncate(mh_usize);
                }
            }
            let pos =
                self.resolve_overlay_layout(options.as_ref(), overlay_lines.len() as i64, term_width, term_height);
            entry.inner.state.lock().unwrap().bounds = Some(OverlayBounds {
                row: pos.row,
                col: pos.col,
                width: initial.width,
                height: overlay_lines.len() as i64,
            });
            min_lines_needed = min_lines_needed.max(pos.row + overlay_lines.len() as i64);
            rendered.push(RenderedItem {
                entry,
                overlay_lines,
                row: pos.row,
                col: pos.col,
                w: initial.width,
            });
        }

        self.lock().rendered_overlay_layouts = rendered
            .iter()
            .map(|r| RenderedOverlayLayout {
                entry: r.entry.clone(),
                row: r.row,
                col: r.col,
                width: r.w,
                height: r.overlay_lines.len() as i64,
            })
            .collect();

        let working_height = (result.len() as i64).max(term_height).max(min_lines_needed);
        let working_height_usize = usize::try_from(working_height.max(0)).unwrap_or(0);
        while result.len() < working_height_usize {
            result.push(String::new());
        }

        let viewport_start = 0.max(working_height - term_height);
        for r in rendered {
            for (i, ov_line) in r.overlay_lines.into_iter().enumerate() {
                let idx = viewport_start + r.row + i as i64;
                if idx >= 0 && (idx as usize) < result.len() {
                    let uidx = idx as usize;
                    let truncated = if crate::utils::visible_width(&ov_line) > r.w {
                        crate::utils::slice_by_column(&ov_line, 0, r.w, Some(true))
                    } else {
                        ov_line
                    };
                    result[uidx] = self.composite_line_at(&result[uidx], &truncated, r.col, r.w, term_width);
                }
            }
        }

        result
    }

    pub(crate) fn apply_line_resets(&self, mut lines: Vec<String>) -> Vec<String> {
        for line in &mut lines {
            if !crate::terminal_image::is_image_line(line) {
                let cleaned = line.replace(CURSOR_MARKER, "");
                *line = format!("{}{}", crate::utils::normalize_terminal_output(&cleaned), SEGMENT_RESET);
            }
        }
        lines
    }

    fn composite_line_at(
        &self,
        base_line: &str,
        overlay_line: &str,
        start_col: i64,
        overlay_width: i64,
        total_width: i64,
    ) -> String {
        composite_tui_line(base_line, overlay_line, start_col, overlay_width, total_width)
    }

    /// Find and extract cursor position from rendered lines.
    /// Searches for CURSOR_MARKER, calculates its position, and strips it from the output.
    /// Only scans the bottom terminal height lines (visible viewport).
    pub(crate) fn extract_cursor_position(&self, lines: &mut [String], height: i64) -> Option<CursorPosition> {
        let viewport_top = lines.len().saturating_sub(usize::try_from(height.max(0)).unwrap_or(0));
        for row in (viewport_top..lines.len()).rev() {
            if let Some(marker_idx) = lines[row].find(CURSOR_MARKER) {
                let col = crate::utils::visible_width(&lines[row][..marker_idx]);
                lines[row] = lines[row].replace(CURSOR_MARKER, "");
                return Some(CursorPosition { row: row as i64, col });
            }
        }
        None
    }

    /// Resolve fake cursor markers to reverse video. When the hardware cursor is shown, a fake cursor
    /// directly after `CURSOR_MARKER` is dropped because the terminal cursor is drawn there. Fake cursors
    /// of components without focus have no `CURSOR_MARKER` and stay visible.
    pub(crate) fn resolve_fake_cursors(&self, lines: Vec<String>) -> Vec<String> {
        let show_hw = self.get_show_hardware_cursor();
        lines
            .into_iter()
            .map(|line| {
                if !line.contains(FAKE_CURSOR_START) {
                    return line;
                }
                let resolved = if show_hw {
                    FOCUSED_FAKE_CURSOR.replace(&line, "$1$2").into_owned()
                } else {
                    line
                };
                resolved
                    .replace(FAKE_CURSOR_START, "\x1b[7m")
                    .replace(FAKE_CURSOR_END, "\x1b[27m")
            })
            .collect()
    }

    /// Mode resets written before the terminal's own resets when the process exits without `stop()`.
    pub(crate) fn default_exit_restore_prefix(&self) -> String {
        let notifications = if self.lock().terminal_color_scheme_notifications_enabled {
            "\x1b[?2031l"
        } else {
            ""
        };
        format!("{notifications}\x1b[?25h")
    }

    fn exit_restore_prefix(&self) -> String {
        self.overrides().map_or_else(
            || self.default_exit_restore_prefix(),
            |o| o.get_exit_restore_prefix(self),
        )
    }

    fn exit_restore_suffix(&self) -> String {
        self.overrides()
            .map_or_else(String::new, |o| o.get_exit_restore_suffix())
    }

    pub async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        let (tx, rx) = tokio::sync::oneshot::channel::<TerminalColors>();
        let tx_slot = Arc::new(Mutex::new(Some(tx)));
        let deliver_slot = Arc::clone(&tx_slot);
        let deliver: Arc<dyn Fn(TerminalColors) + Send + Sync> = Arc::new(move |colors| {
            if let Some(sender) = deliver_slot.lock().unwrap().take() {
                let _ = sender.send(colors);
            }
        });
        let deliver_id = Arc::clone(&deliver);
        let on_late_reply = options.on_late_reply;

        let this_timeout = self.clone();
        let delay_ms = u64::try_from(options.timeout_ms.max(0)).unwrap_or(0);
        let timer = pi_js::time::set_timeout(delay_ms, move || {
            let result = {
                let mut state = this_timeout.lock();
                if let Some(query) = state
                    .pending_terminal_color_queries
                    .iter_mut()
                    .find(|q| q.deliver.as_ref().is_some_and(|d| Arc::ptr_eq(d, &deliver_id)))
                {
                    query.deliver = on_late_reply.clone();
                    query.timer = None;
                    let all_palette = query.palette.len() as i64 == TERMINAL_PALETTE_SIZE
                        && query.palette.iter().all(Option::is_some);
                    let palette = if all_palette {
                        Some(query.palette.iter().filter_map(|c| *c).collect())
                    } else {
                        None
                    };
                    Some(TerminalColors {
                        foreground: query.foreground,
                        background: query.background,
                        palette,
                    })
                } else {
                    None
                }
            };
            if let Some(colors) = result {
                if let Some(sender) = tx_slot.lock().unwrap().take() {
                    let _ = sender.send(colors);
                }
            }
        });

        let query = PendingTerminalColorQuery {
            foreground: None,
            background: None,
            palette: vec![None; TERMINAL_PALETTE_SIZE as usize],
            replied: IndexSet::new(),
            deliver: Some(deliver),
            timer: Some(timer),
        };
        self.lock().pending_terminal_color_queries.push(query);
        self.inner.terminal.write(TERMINAL_COLOR_QUERY);
        rx.await.unwrap_or_default()
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

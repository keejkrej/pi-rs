//! Port of packages/tui/src/tui-alt-screen.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use regex::Regex;

use pi_js::Unsubscribe;
use pi_js::time::Interval;

use crate::alt_screen_search::{
    AltScreenSearchComponent, AltScreenSearchIndex, AltScreenSearchMatch, NavigationButtonStyle,
};
use crate::components::alt_screen_flash::AltScreenFlashContainer;
use crate::components::scroll_view::ScrollView;
use crate::layout::{LayoutFrame, ScrollbarGeometry};
use crate::terminal::Terminal;
use crate::terminal_colors::{TerminalColorScheme, TerminalColors};
use crate::terminal_image::{ImageProtocol, TerminalCapabilities};
use crate::tui::{
    Component, OverlayHandle, OverlayOptions, QueryTerminalColorsOptions, TUI, TuiBase, TuiBaseOverrides,
    TuiInputListener, TuiInputListenerResult, TuiMode, TuiMouseButton, TuiMouseDispatchResult, TuiMouseDispatchTarget,
    TuiMouseEvent, TuiMouseEventResult, TuiStopOptions, ViewportTUI,
};
use crate::wheel_scroll::{WheelScrollAccelerator, WheelScrollLines};

const ENTER_ALT_SCREEN: &str = "\x1b[?1049h";
const EXIT_ALT_SCREEN: &str = "\x1b[?1049l";
const DISABLE_AUTOWRAP: &str = "\x1b[?7l";
const ENABLE_AUTOWRAP: &str = "\x1b[?7h";
const ENABLE_BUTTON_MOTION_MOUSE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1004h\x1b[?1006h";
const ENABLE_ALL_MOTION_MOUSE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h";
const DISABLE_MOUSE: &str = "\x1b[?1006l\x1b[?1004l\x1b[?1003l\x1b[?1002l\x1b[?1000l";
const FOCUS_IN: &str = "\x1b[I";
const FOCUS_OUT: &str = "\x1b[O";
const BEGIN_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026h";
const END_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026l";
static OSC133_ZONE_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\x1b\]133;[ABC](?:\x07|\x1b\\))+").expect("OSC133_ZONE_PREFIX"));
static OSC133_PROMPT_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\]133;A(?:\x07|\x1b\\)").expect("OSC133_PROMPT_START"));
const PAGE_SCROLL_OVERLAP: i64 = 4;
const ALT_WHEEL_SCROLL_MULTIPLIER: i64 = 5;
const MAX_CACHED_OFFSCREEN_KITTY_IMAGES: i64 = 16;
const MAX_CACHED_OFFSCREEN_KITTY_TRANSMISSION_BYTES: i64 = 32 * 1024 * 1024;
const MAX_CACHED_OFFSCREEN_KITTY_DECODED_BYTES: i64 = 64 * 1024 * 1024;
const DOUBLE_CLICK_INTERVAL_MS: i64 = 500;
const COPY_ERROR_FLASH_DURATION_MS: i64 = 5000;
// Regular mode delegates double-click selection to the terminal emulator. Fullscreen owns mouse selection,
// so mirror common terminal word-selection behavior by keeping paths and kebab-case tokens whole.
static TERMINAL_WORD_SELECTION_JOINERS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    let mut joiners = IndexSet::new();
    joiners.insert("/");
    joiners.insert("-");
    joiners
});

// PORT: shared word segmenter is `crate::utils::get_word_segmenter`. Not redeclared.

type ComponentHandle = Arc<dyn Component>;
type TerminalHandle = Arc<dyn Terminal>;
type DebugCallback = Arc<dyn Fn() + Send + Sync>;
type SchemeListener = Arc<dyn Fn(TerminalColorScheme) + Send + Sync>;

/// `(text) => string` for a non-current or current transcript search match.
pub type SearchMatchStyle = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// `() => string` jump-to-end label.
pub type ScrollToEndIndicator = Arc<dyn Fn() -> String + Send + Sync>;

/// `(url) => void`.
pub type OpenUrl = Arc<dyn Fn(&str) + Send + Sync>;

/// `() => void` secondary-button paste.
pub type RightClickPaste = Arc<dyn Fn() + Send + Sync>;

/// `true` on success, `false` for a generic error, or a message to display.
///
/// TS `boolean | string`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopySelectionResult {
    Bool(bool),
    Message(String),
}

/// `(text) => Promise<boolean | string>`.
///
/// PORT: the argument is an owned `String` because [`pi_js::BoxFuture`] is `'static`.
pub type CopySelectionFn = Arc<dyn Fn(String) -> pi_js::BoxFuture<CopySelectionResult> + Send + Sync>;

struct CachedKittyImage {
    transmission_generation: i64,
    transmission_bytes: i64,
    estimated_decoded_bytes: i64,
}

struct SelectionPoint {
    row: i64,
    col: i64,
    scroll_view: Option<ScrollView>,
    /// Whether this point lies between terminal cells rather than on a cell.
    ///
    /// PORT: TS `boundary?: boolean`. Only `true` is distinct; absent is `false`.
    boundary: bool,
}

struct SelectionRange {
    start: SelectionPoint,
    end: SelectionPoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionGranularity {
    Character,
    Word,
    Line,
}

struct ClickTarget {
    timestamp: i64,
    count: i64,
    row: i64,
    scroll_view: Option<ScrollView>,
    word_start: i64,
    word_end: i64,
}

struct SgrMouseEvent {
    button: i64,
    x: i64,
    y: i64,
    release: bool,
}

struct WheelEvent {
    /// `-1` or `1`.
    direction: i64,
    x: i64,
    y: i64,
    button: i64,
}

struct ScrollbarDrag {
    scroll_view: ScrollView,
    grab_offset: i64,
}

struct ScrollbarTarget {
    scroll_view: ScrollView,
    geometry: ScrollbarGeometry,
}

struct ScrollToEndIndicatorRect {
    row: i64,
    column: i64,
    width: i64,
}

/// `"query" | "retain" | "next" | "previous"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchSelectionMode {
    Query,
    Retain,
    Next,
    Previous,
}

struct ActiveSearch {
    component: AltScreenSearchComponent,
    index: AltScreenSearchIndex,
    overlay: Option<OverlayHandle>,
    query: String,
    matches: Vec<AltScreenSearchMatch>,
    selected_index: i64,
    selected_key: Option<String>,
    anchor_row: i64,
    selection_mode: SearchSelectionMode,
}

struct SearchHighlightRange {
    start_col: i64,
    end_col: i64,
    current: bool,
}

struct PointerPoint {
    x: i64,
    y: i64,
}

struct ComponentClick {
    timestamp: i64,
    count: i64,
    component: ComponentHandle,
    x: i64,
    y: i64,
}

struct MouseEventExtra {
    wheel_delta: Option<i64>,
    click_count: Option<i64>,
}

struct PreparedKittyScreen {
    lines: Vec<String>,
    evicted_image_deletion: String,
}

struct SelectionColumns {
    start: i64,
    end: i64,
}

/// Anonymous `implicitDocument` passed to the implicit [`ScrollView`].
///
/// PORT: TS object literal. `render` / `handleMouse` forward to [`TuiBase`]; `invalidate`
/// invalidates `TuiBase` children.
struct ImplicitDocument {
    screen: TuiAltScreen,
}

impl ImplicitDocument {
    fn render(&self, width: usize) -> Vec<String> {
        todo!("port: ImplicitDocument::render")
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        todo!("port: ImplicitDocument::handle_mouse")
    }

    fn invalidate(&self) {
        todo!("port: ImplicitDocument::invalidate")
    }
}

impl Component for ImplicitDocument {
    fn render(&self, width: usize) -> Vec<String> {
        ImplicitDocument::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        ImplicitDocument::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ImplicitDocument::invalidate(self)
    }
}

struct TuiAltScreenState {
    previous_screen: Vec<String>,
    last_document: Vec<String>,
    previous_screen_width: i64,
    previous_screen_height: i64,
    layout_root: Option<ComponentHandle>,
    current_layout: Option<LayoutFrame>,
    alt_screen_active: bool,
    /// PORT: TS `ImageProtocol` is `"kitty" | "iterm2" | null`. `None` is `null`.
    image_protocol: Option<ImageProtocol>,
    saved_capabilities: Option<TerminalCapabilities>,
    /// Insertion order is the Kitty upload LRU. Deletes use `shift_remove`.
    uploaded_kitty_images: IndexMap<i64, CachedKittyImage>,
    selection_anchor: Option<SelectionPoint>,
    selection_focus: Option<SelectionPoint>,
    selection_granularity: SelectionGranularity,
    selection_initial_range: Option<SelectionRange>,
    last_click: Option<ClickTarget>,
    selection_drag_pointer: Option<PointerPoint>,
    /// `-1`, `0`, or `1`.
    selection_auto_scroll_direction: i64,
    selection_auto_scroll_timer: Option<Interval>,
    selection_press_active: bool,
    scrollbar_drag: Option<ScrollbarDrag>,
    scrollbar_hover: Option<ScrollView>,
    scroll_to_end_indicator_rect: Option<ScrollToEndIndicatorRect>,
    active_search: Option<ActiveSearch>,
    pressed_url: Option<String>,
    selection_dragged: bool,
    mouse_capture: Option<TuiMouseDispatchTarget>,
    mouse_press_target: Option<TuiMouseDispatchTarget>,
    mouse_press_point: Option<PointerPoint>,
    mouse_press_moved: bool,
    last_component_click: Option<ComponentClick>,
    copy_on_select: bool,
}

struct TuiAltScreenInner {
    /// PORT: `extends TuiBase` is composition. Overridden hooks are [`TuiBaseOverrides`].
    /// After [`TuiAltScreen::new`], call [`TuiBase::bind_overrides`] (`Weak` inside the base breaks the cycle).
    base: TuiBase,
    implicit_document: ComponentHandle,
    implicit_scroll_view: ScrollView,
    flashes: AltScreenFlashContainer,
    wheel_scroll: WheelScrollAccelerator,
    mouse_enabled: bool,
    search_match_style: SearchMatchStyle,
    search_current_match_style: SearchMatchStyle,
    search_navigation_button_style: NavigationButtonStyle,
    scroll_to_end_indicator: Option<ScrollToEndIndicator>,
    open_url: Option<OpenUrl>,
    on_right_click_paste: Option<RightClickPaste>,
    copy_selection: Option<CopySelectionFn>,
    state: Mutex<TuiAltScreenState>,
}

#[derive(Clone, Default)]
pub struct TuiAltScreenOptions {
    /// Logical lines moved for each mouse-wheel event (default: 1). `"auto"` accelerates fast wheel
    /// spins on terminals that send one event per notch. Alt+wheel moves five times as far.
    pub wheel_scroll_lines: Option<WheelScrollLines>,
    /// Capture mouse events for viewport scrolling and application-owned text selection.
    pub mouse: Option<bool>,
    /// Style a non-current transcript search match.
    pub search_match_style: Option<SearchMatchStyle>,
    /// Style the current transcript search match.
    pub search_current_match_style: Option<SearchMatchStyle>,
    /// Style a transcript search navigation button.
    pub search_navigation_button_style: Option<NavigationButtonStyle>,
    /// Render a clickable jump-to-end label. It is centered on the last row of a follow-end
    /// primary scroll view while that view is scrolled away from its end.
    pub scroll_to_end_indicator: Option<ScrollToEndIndicator>,
    /// Open an OSC 8 hyperlink activated with a primary-button click.
    pub open_url: Option<OpenUrl>,
    /// Handle an unmodified secondary-button press for clipboard paste. Currently enabled on Windows only.
    pub on_right_click_paste: Option<RightClickPaste>,
    /// Automatically copy selected text to the clipboard on mouse release (default: true).
    pub copy_on_select: Option<bool>,
    /// Copy selected text to the system clipboard. Return `true` on success, an error message to
    /// display on failure, or `false` for a generic error. When omitted, the selection is copied
    /// via an OSC 52 write.
    pub copy_selection: Option<CopySelectionFn>,
}

/// Alternate-screen TUI with a scrollable, application-owned viewport.
///
/// PORT: TS `class TuiAltScreen extends TuiBase implements ViewportTUI`. Handle, not a Rust
/// subclass. `mode` is `"fullscreen"`. [`TUI::as_viewport`] is the `VIEWPORT_TUI` brand.
/// Inherited [`TUI`] methods forward to [`TuiBase`]. Overridden hooks are [`TuiBaseOverrides`].
#[derive(Clone)]
pub struct TuiAltScreen {
    inner: Arc<TuiAltScreenInner>,
}

impl TuiAltScreen {
    fn base(&self) -> &TuiBase {
        &self.inner.base
    }

    fn state(&self) -> MutexGuard<'_, TuiAltScreenState> {
        self.inner.state.lock().unwrap()
    }

    /// `show_hardware_cursor` and `log_directory` `None` omit the TS arguments.
    /// `options` `None` is `{}`.
    pub fn new(
        terminal: TerminalHandle,
        show_hardware_cursor: Option<bool>,
        log_directory: Option<&str>,
        options: Option<TuiAltScreenOptions>,
    ) -> Self {
        todo!("port: TuiAltScreen::new")
    }

    /// TS `readonly mode = "fullscreen"`.
    pub fn mode(&self) -> TuiMode {
        TuiMode::Fullscreen
    }

    /// TS `readonly [VIEWPORT_TUI] = true`.
    pub fn viewport_tui(&self) -> bool {
        true
    }

    pub fn terminal(&self) -> TerminalHandle {
        self.base().terminal()
    }

    pub fn children(&self) -> Vec<ComponentHandle> {
        self.base().children()
    }

    pub fn set_children(&self, children: Vec<ComponentHandle>) {
        self.base().set_children(children);
    }

    pub fn on_debug(&self) -> Option<DebugCallback> {
        self.base().on_debug()
    }

    pub fn set_on_debug(&self, on_debug: Option<DebugCallback>) {
        self.base().set_on_debug(on_debug);
    }

    pub fn full_redraws(&self) -> i64 {
        self.base().full_redraws()
    }

    pub fn add_child(&self, component: ComponentHandle) {
        self.base().add_child(component);
    }

    pub fn remove_child(&self, component: &ComponentHandle) {
        self.base().remove_child(component);
    }

    pub fn clear(&self) {
        self.base().clear();
    }

    pub fn get_show_hardware_cursor(&self) -> bool {
        self.base().get_show_hardware_cursor()
    }

    pub fn set_show_hardware_cursor(&self, enabled: bool) {
        self.base().set_show_hardware_cursor(enabled);
    }

    pub fn get_clear_on_shrink(&self) -> bool {
        self.base().get_clear_on_shrink()
    }

    pub fn set_clear_on_shrink(&self, enabled: bool) {
        self.base().set_clear_on_shrink(enabled);
    }

    pub fn get_focused_component(&self) -> Option<ComponentHandle> {
        self.base().get_focused_component()
    }

    pub fn set_focus(&self, component: Option<ComponentHandle>) {
        self.base().set_focus(component);
    }

    pub fn show_overlay(&self, component: ComponentHandle, options: Option<OverlayOptions>) -> OverlayHandle {
        self.base().show_overlay(component, options)
    }

    pub fn hide_overlay(&self) {
        self.base().hide_overlay();
    }

    pub fn has_overlay(&self) -> bool {
        self.base().has_overlay()
    }

    pub fn has_overlay_entries(&self) -> bool {
        self.base().has_overlay_entries()
    }

    pub fn start(&self) {
        self.base().start();
    }

    /// `options` `None` is `{}`.
    pub fn stop(&self, options: Option<TuiStopOptions>) {
        self.base().stop(options);
    }

    /// `force` `None` is `false`.
    pub fn render_now(&self, force: Option<bool>) {
        self.base().render_now(force);
    }

    /// `force` `None` is `false`.
    pub fn request_render(&self, force: Option<bool>) {
        self.base().request_render(force);
    }

    pub fn add_input_listener(&self, listener: TuiInputListener) -> Unsubscribe {
        self.base().add_input_listener(listener)
    }

    pub fn remove_input_listener(&self, listener: &TuiInputListener) {
        self.base().remove_input_listener(listener);
    }

    pub fn on_terminal_color_scheme_change(&self, listener: SchemeListener) -> Unsubscribe {
        self.base().on_terminal_color_scheme_change(listener)
    }

    pub fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        self.base().set_terminal_color_scheme_notifications(enabled);
    }

    pub async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        self.base().query_terminal_colors(options).await
    }

    pub fn viewport_top(&self) -> i64 {
        todo!("port: TuiAltScreen::viewport_top")
    }

    pub fn is_following_output(&self) -> bool {
        todo!("port: TuiAltScreen::is_following_output")
    }

    pub fn set_wheel_scroll_lines(&self, lines: WheelScrollLines) {
        todo!("port: TuiAltScreen::set_wheel_scroll_lines")
    }

    pub fn get_copy_on_select(&self) -> bool {
        self.state().copy_on_select
    }

    pub fn set_copy_on_select(&self, enabled: bool) {
        self.state().copy_on_select = enabled;
    }

    /// Whether the fullscreen viewport has a non-empty active text selection.
    pub fn has_active_selection(&self) -> bool {
        todo!("port: TuiAltScreen::has_active_selection")
    }

    /// Copy the active fullscreen text selection, if any, using the configured selection clipboard path.
    pub async fn copy_active_selection_to_clipboard(&self) -> bool {
        todo!("port: TuiAltScreen::copy_active_selection_to_clipboard")
    }

    /// The lines of the last rendered frame, one per terminal row, as written to the terminal.
    pub fn get_screen_lines(&self) -> Vec<String> {
        self.state().previous_screen.clone()
    }

    pub fn set_layout_root(&self, component: Option<ComponentHandle>) {
        todo!("port: TuiAltScreen::set_layout_root")
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: TuiAltScreen::render")
    }

    /// PORT: `protected`. Visible to [`TuiBase`] in this crate.
    pub(crate) fn get_mounted_roots(&self) -> Vec<ComponentHandle> {
        todo!("port: TuiAltScreen::get_mounted_roots")
    }

    fn get_primary_scroll_view(&self) -> ScrollView {
        todo!("port: TuiAltScreen::get_primary_scroll_view")
    }

    // Multiplexers can lag when every pointer movement is forwarded. Button-motion
    // tracking preserves clicks, wheel events, selections, and scrollbar dragging.
    /// PORT: `protected`.
    pub(crate) fn before_terminal_start(&self) {
        todo!("port: TuiAltScreen::before_terminal_start")
    }

    /// PORT: `protected`.
    pub(crate) fn before_terminal_stop(&self, options: &TuiStopOptions) {
        todo!("port: TuiAltScreen::before_terminal_stop")
    }

    /// PORT: `protected`.
    pub(crate) fn after_terminal_stop(&self, options: &TuiStopOptions) {
        todo!("port: TuiAltScreen::after_terminal_stop")
    }

    fn delete_kitty_images(&self) -> String {
        todo!("port: TuiAltScreen::delete_kitty_images")
    }

    fn prepare_kitty_screen(&self, screen: &[String]) -> PreparedKittyScreen {
        todo!("port: TuiAltScreen::prepare_kitty_screen")
    }

    /// PORT: `protected`.
    pub(crate) fn reset_render_state(&self) {
        todo!("port: TuiAltScreen::reset_render_state")
    }

    pub fn scroll_by(&self, lines: i64) {
        todo!("port: TuiAltScreen::scroll_by")
    }

    pub fn scroll_to_top(&self) {
        todo!("port: TuiAltScreen::scroll_to_top")
    }

    pub fn scroll_to_bottom(&self) {
        todo!("port: TuiAltScreen::scroll_to_bottom")
    }

    fn scroll_to_prompt(&self, direction: i64) {
        todo!("port: TuiAltScreen::scroll_to_prompt")
    }

    fn toggle_search(&self) {
        todo!("port: TuiAltScreen::toggle_search")
    }

    fn close_search(&self) {
        todo!("port: TuiAltScreen::close_search")
    }

    fn update_search_query(&self, query: &str) {
        todo!("port: TuiAltScreen::update_search_query")
    }

    fn navigate_search(&self, direction: i64) {
        todo!("port: TuiAltScreen::navigate_search")
    }

    fn get_search_navigation_direction_at(&self, x: i64, y: i64) -> Option<i64> {
        todo!("port: TuiAltScreen::get_search_navigation_direction_at")
    }

    fn handle_search_mouse_event(&self, event: &SgrMouseEvent) -> bool {
        todo!("port: TuiAltScreen::handle_search_mouse_event")
    }

    fn refresh_search(&self, layout: &LayoutFrame) -> bool {
        todo!("port: TuiAltScreen::refresh_search")
    }

    /// Show a transient message in the alternate-screen flash stack.
    ///
    /// `duration_ms` `None` lets the flash container apply its default.
    pub fn flash(&self, message: &str, duration_ms: Option<i64>) {
        todo!("port: TuiAltScreen::flash")
    }

    fn should_defer_viewport_input_to_overlay(&self) -> bool {
        todo!("port: TuiAltScreen::should_defer_viewport_input_to_overlay")
    }

    fn clear_component_mouse_gesture(&self) {
        todo!("port: TuiAltScreen::clear_component_mouse_gesture")
    }

    // SGR mouse button codes use bit 3 (value 8) for the Alt modifier.
    fn handle_viewport_input(&self, data: &str) -> Option<TuiInputListenerResult> {
        todo!("port: TuiAltScreen::handle_viewport_input")
    }

    fn decode_mouse_button(&self, button: i64) -> TuiMouseButton {
        todo!("port: TuiAltScreen::decode_mouse_button")
    }

    /// `extra` `None` is `{}`.
    fn create_mouse_event(
        &self,
        r#type: crate::tui::TuiMouseEventType,
        button: i64,
        x: i64,
        y: i64,
        extra: Option<MouseEventExtra>,
    ) -> TuiMouseEvent {
        todo!("port: TuiAltScreen::create_mouse_event")
    }

    fn dispatch_mouse_to_layout(&self, event: &TuiMouseEvent) -> Option<TuiMouseDispatchResult> {
        todo!("port: TuiAltScreen::dispatch_mouse_to_layout")
    }

    fn apply_mouse_dispatch_result(&self, event: &TuiMouseEvent, result: &TuiMouseDispatchResult) -> bool {
        todo!("port: TuiAltScreen::apply_mouse_dispatch_result")
    }

    fn dispatch_mouse_to_target(
        &self,
        event: &TuiMouseEvent,
        target: &TuiMouseDispatchTarget,
    ) -> Option<TuiMouseDispatchResult> {
        todo!("port: TuiAltScreen::dispatch_mouse_to_target")
    }

    fn get_component_click_count(&self, target: &TuiMouseDispatchTarget, x: i64, y: i64) -> i64 {
        todo!("port: TuiAltScreen::get_component_click_count")
    }

    fn clear_text_selection(&self) {
        todo!("port: TuiAltScreen::clear_text_selection")
    }

    fn handle_mouse_event(&self, raw: &SgrMouseEvent) {
        todo!("port: TuiAltScreen::handle_mouse_event")
    }

    fn parse_wheel_event(&self, data: &str) -> Option<WheelEvent> {
        todo!("port: TuiAltScreen::parse_wheel_event")
    }

    fn route_wheel(&self, event: &WheelEvent, delta: i64) {
        todo!("port: TuiAltScreen::route_wheel")
    }

    fn parse_sgr_mouse_event(&self, data: &str) -> Option<SgrMouseEvent> {
        todo!("port: TuiAltScreen::parse_sgr_mouse_event")
    }

    fn handle_right_click_paste(&self, event: &SgrMouseEvent) -> bool {
        todo!("port: TuiAltScreen::handle_right_click_paste")
    }

    fn handle_scroll_to_end_indicator_mouse_event(&self, event: &SgrMouseEvent) -> bool {
        todo!("port: TuiAltScreen::handle_scroll_to_end_indicator_mouse_event")
    }

    /// `include_hidden_auto` `None` is `false`.
    fn get_scrollbar_target_at(&self, x: i64, y: i64, include_hidden_auto: Option<bool>) -> Option<ScrollbarTarget> {
        todo!("port: TuiAltScreen::get_scrollbar_target_at")
    }

    fn set_scrollbar_hover(&self, scroll_view: Option<&ScrollView>) {
        todo!("port: TuiAltScreen::set_scrollbar_hover")
    }

    fn update_scrollbar_hover(&self, x: i64, y: i64) {
        todo!("port: TuiAltScreen::update_scrollbar_hover")
    }

    fn stop_scrollbar_hover(&self) {
        todo!("port: TuiAltScreen::stop_scrollbar_hover")
    }

    fn scroll_scrollbar_to_pointer(
        &self,
        scroll_view: &ScrollView,
        geometry: &ScrollbarGeometry,
        pointer_y: i64,
        grab_offset: i64,
    ) {
        todo!("port: TuiAltScreen::scroll_scrollbar_to_pointer")
    }

    fn handle_scrollbar_mouse_event(&self, event: &SgrMouseEvent) -> bool {
        todo!("port: TuiAltScreen::handle_scrollbar_mouse_event")
    }

    fn stop_scrollbar_drag(&self) {
        todo!("port: TuiAltScreen::stop_scrollbar_drag")
    }

    fn get_scroll_selection_point(&self, scroll_view: &ScrollView, x: i64, y: i64) -> Option<SelectionPoint> {
        todo!("port: TuiAltScreen::get_scroll_selection_point")
    }

    fn get_selection_point(&self, event: &SgrMouseEvent, scroll_view: Option<&ScrollView>) -> SelectionPoint {
        todo!("port: TuiAltScreen::get_selection_point")
    }

    fn get_selection_source_line(&self, point: &SelectionPoint) -> String {
        todo!("port: TuiAltScreen::get_selection_source_line")
    }

    fn get_word_selection(&self, point: &SelectionPoint) -> Option<SelectionRange> {
        todo!("port: TuiAltScreen::get_word_selection")
    }

    fn get_line_selection(&self, point: &SelectionPoint) -> SelectionRange {
        todo!("port: TuiAltScreen::get_line_selection")
    }

    fn update_selection_focus(&self, point: &SelectionPoint) {
        todo!("port: TuiAltScreen::update_selection_focus")
    }

    fn get_click_count(&self, point: &SelectionPoint, word: Option<&SelectionRange>) -> i64 {
        todo!("port: TuiAltScreen::get_click_count")
    }

    fn update_selection_auto_scroll(&self, event: &SgrMouseEvent) {
        todo!("port: TuiAltScreen::update_selection_auto_scroll")
    }

    fn auto_scroll_selection(&self) {
        todo!("port: TuiAltScreen::auto_scroll_selection")
    }

    fn stop_selection_auto_scroll(&self) {
        todo!("port: TuiAltScreen::stop_selection_auto_scroll")
    }

    fn handle_selection_mouse_event(&self, event: &SgrMouseEvent) {
        todo!("port: TuiAltScreen::handle_selection_mouse_event")
    }

    fn get_selection_bounds(&self) -> Option<SelectionRange> {
        todo!("port: TuiAltScreen::get_selection_bounds")
    }

    /// `min_column` `None` is `0`. `max_column` `None` is `visibleWidth(line)`.
    fn get_selection_columns(
        &self,
        line: &str,
        row: i64,
        selection: &SelectionRange,
        min_column: Option<i64>,
        max_column: Option<i64>,
    ) -> SelectionColumns {
        todo!("port: TuiAltScreen::get_selection_columns")
    }

    fn get_active_selection_text(&self) -> Option<String> {
        todo!("port: TuiAltScreen::get_active_selection_text")
    }

    async fn copy_selection_to_clipboard(&self) -> bool {
        todo!("port: TuiAltScreen::copy_selection_to_clipboard")
    }

    // Prefer an injected clipboard implementation (native clipboard + platform tools with a
    // verified success path) when the host app provides one. A bare OSC 52 write can show
    // "Copied!" while leaving the system clipboard untouched (e.g. macOS Terminal.app, tmux
    // without OSC 52 clipboard passthrough), so only report success when it actually copies.
    async fn copy_text_to_clipboard(&self, text: &str) -> bool {
        todo!("port: TuiAltScreen::copy_text_to_clipboard")
    }

    fn apply_search_text_highlight(&self, text: &str, current: bool) -> String {
        todo!("port: TuiAltScreen::apply_search_text_highlight")
    }

    fn apply_search_highlights(&self, screen: &[String], layout: &LayoutFrame) -> Vec<String> {
        todo!("port: TuiAltScreen::apply_search_highlights")
    }

    fn apply_selection_highlight(&self, text: &str) -> String {
        todo!("port: TuiAltScreen::apply_selection_highlight")
    }

    /// `layout` `None` is `this.currentLayout` (also absent when no frame has been rendered).
    fn apply_selection(&self, screen: &[String], layout: Option<&LayoutFrame>) -> Vec<String> {
        todo!("port: TuiAltScreen::apply_selection")
    }

    fn is_mouse_sequence(&self, data: &str) -> bool {
        todo!("port: TuiAltScreen::is_mouse_sequence")
    }

    fn composite_scroll_to_end_indicator(&self, screen: &[String], layout: &LayoutFrame, width: i64) -> Vec<String> {
        todo!("port: TuiAltScreen::composite_scroll_to_end_indicator")
    }

    fn composite_flashes(&self, screen: &[String], width: i64, height: i64) -> Vec<String> {
        todo!("port: TuiAltScreen::composite_flashes")
    }

    // WezTerm erases intersecting Kitty image cells when a later EL clears a covered row.
    // Only separate clearing from drawing for WezTerm frames that place images; preserve the
    // existing interleaved output for text-only frames and every other terminal.
    /// PORT: `protected`.
    pub(crate) fn do_render(&self) {
        todo!("port: TuiAltScreen::do_render")
    }

    /// Inherits [`TuiBase::handle_mouse`] ([`crate::tui::Container::handle_mouse`]). Not overridden.
    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.base().handle_mouse(event)
    }

    pub fn handle_input(&self, _data: &str) {}

    pub fn wants_key_release(&self) -> bool {
        false
    }

    pub fn invalidate(&self) {
        self.base().invalidate();
    }
}

impl Component for TuiAltScreen {
    fn render(&self, width: usize) -> Vec<String> {
        TuiAltScreen::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        TuiAltScreen::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        TuiAltScreen::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        TuiAltScreen::wants_key_release(self)
    }

    fn invalidate(&self) {
        TuiAltScreen::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<ComponentHandle>> {
        Some(self.children())
    }
}

impl TuiBaseOverrides for TuiAltScreen {
    fn mode(&self) -> TuiMode {
        TuiAltScreen::mode(self)
    }

    fn do_render(&self) {
        TuiAltScreen::do_render(self)
    }

    fn reset_render_state(&self) {
        TuiAltScreen::reset_render_state(self)
    }

    fn before_terminal_start(&self) {
        TuiAltScreen::before_terminal_start(self)
    }

    fn before_terminal_stop(&self, options: &TuiStopOptions) {
        TuiAltScreen::before_terminal_stop(self, options)
    }

    fn after_terminal_stop(&self, options: &TuiStopOptions) {
        TuiAltScreen::after_terminal_stop(self, options)
    }

    fn get_mounted_roots(&self, _base: &TuiBase) -> Vec<ComponentHandle> {
        TuiAltScreen::get_mounted_roots(self)
    }
}

#[async_trait]
impl TUI for TuiAltScreen {
    fn mode(&self) -> TuiMode {
        TuiAltScreen::mode(self)
    }

    fn children(&self) -> Vec<ComponentHandle> {
        TuiAltScreen::children(self)
    }

    fn set_children(&self, children: Vec<ComponentHandle>) {
        TuiAltScreen::set_children(self, children);
    }

    fn terminal(&self) -> TerminalHandle {
        TuiAltScreen::terminal(self)
    }

    fn on_debug(&self) -> Option<DebugCallback> {
        TuiAltScreen::on_debug(self)
    }

    fn set_on_debug(&self, on_debug: Option<DebugCallback>) {
        TuiAltScreen::set_on_debug(self, on_debug);
    }

    fn full_redraws(&self) -> i64 {
        TuiAltScreen::full_redraws(self)
    }

    fn add_child(&self, component: ComponentHandle) {
        TuiAltScreen::add_child(self, component);
    }

    fn remove_child(&self, component: &ComponentHandle) {
        TuiAltScreen::remove_child(self, component);
    }

    fn clear(&self) {
        TuiAltScreen::clear(self);
    }

    fn get_show_hardware_cursor(&self) -> bool {
        TuiAltScreen::get_show_hardware_cursor(self)
    }

    fn set_show_hardware_cursor(&self, enabled: bool) {
        TuiAltScreen::set_show_hardware_cursor(self, enabled);
    }

    fn get_clear_on_shrink(&self) -> bool {
        TuiAltScreen::get_clear_on_shrink(self)
    }

    fn set_clear_on_shrink(&self, enabled: bool) {
        TuiAltScreen::set_clear_on_shrink(self, enabled);
    }

    fn set_focus(&self, component: Option<ComponentHandle>) {
        TuiAltScreen::set_focus(self, component);
    }

    fn show_overlay(&self, component: ComponentHandle, options: Option<OverlayOptions>) -> OverlayHandle {
        TuiAltScreen::show_overlay(self, component, options)
    }

    fn hide_overlay(&self) {
        TuiAltScreen::hide_overlay(self);
    }

    fn has_overlay(&self) -> bool {
        TuiAltScreen::has_overlay(self)
    }

    fn start(&self) {
        TuiAltScreen::start(self);
    }

    fn stop(&self, options: Option<TuiStopOptions>) {
        TuiAltScreen::stop(self, options);
    }

    fn render_now(&self, force: Option<bool>) {
        TuiAltScreen::render_now(self, force);
    }

    fn request_render(&self, force: Option<bool>) {
        TuiAltScreen::request_render(self, force);
    }

    fn add_input_listener(&self, listener: TuiInputListener) -> Unsubscribe {
        TuiAltScreen::add_input_listener(self, listener)
    }

    fn remove_input_listener(&self, listener: &TuiInputListener) {
        TuiAltScreen::remove_input_listener(self, listener);
    }

    fn on_terminal_color_scheme_change(&self, listener: SchemeListener) -> Unsubscribe {
        TuiAltScreen::on_terminal_color_scheme_change(self, listener)
    }

    fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        TuiAltScreen::set_terminal_color_scheme_notifications(self, enabled);
    }

    async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        TuiAltScreen::query_terminal_colors(self, options).await
    }

    fn as_viewport(&self) -> Option<&dyn ViewportTUI> {
        Some(self)
    }
}

#[async_trait]
impl ViewportTUI for TuiAltScreen {
    fn set_layout_root(&self, component: Option<ComponentHandle>) {
        TuiAltScreen::set_layout_root(self, component);
    }
}

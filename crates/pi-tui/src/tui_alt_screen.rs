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
static SGR_WHEEL_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[<([0-9]+);([0-9]+);([0-9]+)[Mm]$").expect("SGR_WHEEL_REGEX"));
static SGR_MOUSE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[<([0-9]+);([0-9]+);([0-9]+)([Mm])$").expect("SGR_MOUSE_REGEX"));
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

#[derive(Clone, Copy)]
struct CachedKittyImage {
    transmission_generation: i64,
    transmission_bytes: i64,
    estimated_decoded_bytes: i64,
}

#[derive(Clone)]
struct SelectionPoint {
    row: i64,
    col: i64,
    scroll_view: Option<ScrollView>,
    /// Whether this point lies between terminal cells rather than on a cell.
    ///
    /// PORT: TS `boundary?: boolean`. Only `true` is distinct; absent is `false`.
    boundary: bool,
}

#[derive(Clone)]
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

#[derive(Clone)]
struct ClickTarget {
    timestamp: i64,
    count: i64,
    row: i64,
    scroll_view: Option<ScrollView>,
    word_start: i64,
    word_end: i64,
}

#[derive(Clone, Copy)]
struct SgrMouseEvent {
    button: i64,
    x: i64,
    y: i64,
    release: bool,
}

#[derive(Clone, Copy)]
struct WheelEvent {
    /// `-1` or `1`.
    direction: i64,
    x: i64,
    y: i64,
    button: i64,
}

#[derive(Clone)]
struct ScrollbarDrag {
    scroll_view: ScrollView,
    grab_offset: i64,
}

#[derive(Clone)]
struct ScrollbarTarget {
    scroll_view: ScrollView,
    geometry: ScrollbarGeometry,
}

#[derive(Clone, Copy)]
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

#[derive(Clone)]
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

#[derive(Clone, Copy)]
struct PointerPoint {
    x: i64,
    y: i64,
}

#[derive(Clone)]
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
    screen: std::sync::Weak<TuiAltScreenInner>,
}

impl ImplicitDocument {
    fn render(&self, width: usize) -> Vec<String> {
        self.screen
            .upgrade()
            .map(|inner| inner.base.render(width))
            .unwrap_or_default()
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.screen
            .upgrade()
            .and_then(|inner| inner.base.handle_mouse(event))
    }

    fn invalidate(&self) {
        if let Some(inner) = self.screen.upgrade() {
            for child in inner.base.children() {
                child.invalidate();
            }
        }
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
        let opts = options.unwrap_or_default();
        let base = TuiBase::new(terminal, show_hardware_cursor, log_directory);
        let search_match_style: SearchMatchStyle = opts
            .search_match_style
            .unwrap_or_else(|| Arc::new(|text| format!("\x1b[4m{text}\x1b[24m")));
        let search_current_match_style: SearchMatchStyle = opts
            .search_current_match_style
            .unwrap_or_else(|| Arc::new(|text| format!("\x1b[1;7m{text}\x1b[22;27m")));
        let search_navigation_button_style: NavigationButtonStyle = opts
            .search_navigation_button_style
            .unwrap_or_else(|| Arc::new(|text, _hovered| text.to_string()));
        let wheel_scroll = WheelScrollAccelerator::new(
            Some(opts.wheel_scroll_lines.unwrap_or(WheelScrollLines::Lines(1.0))),
            None,
        );
        let mouse_enabled = opts.mouse.unwrap_or(true);
        let copy_on_select = opts.copy_on_select.unwrap_or(true);

        let inner = Arc::new_cyclic(|weak: &std::sync::Weak<TuiAltScreenInner>| {
            let implicit_document: ComponentHandle = Arc::new(ImplicitDocument {
                screen: std::sync::Weak::clone(weak),
            });
            let implicit_scroll_view = ScrollView::new(
                Arc::clone(&implicit_document),
                Some(crate::components::scroll_view::ScrollViewOptions {
                    follow: Some(crate::components::scroll_view::ScrollViewFollow::End),
                    primary: Some(true),
                    ..Default::default()
                }),
            );
            let flash_render_weak = std::sync::Weak::clone(weak);
            let flashes = AltScreenFlashContainer::new(Arc::new(move || {
                if let Some(inner) = flash_render_weak.upgrade() {
                    inner.base.request_render(None);
                }
            }));
            TuiAltScreenInner {
                base,
                implicit_document,
                implicit_scroll_view,
                flashes,
                wheel_scroll,
                mouse_enabled,
                search_match_style,
                search_current_match_style,
                search_navigation_button_style,
                scroll_to_end_indicator: opts.scroll_to_end_indicator,
                open_url: opts.open_url,
                on_right_click_paste: opts.on_right_click_paste,
                copy_selection: opts.copy_selection,
                state: Mutex::new(TuiAltScreenState {
                    previous_screen: Vec::new(),
                    last_document: Vec::new(),
                    previous_screen_width: 0,
                    previous_screen_height: 0,
                    layout_root: None,
                    current_layout: None,
                    alt_screen_active: false,
                    image_protocol: None,
                    saved_capabilities: None,
                    uploaded_kitty_images: IndexMap::new(),
                    selection_anchor: None,
                    selection_focus: None,
                    selection_granularity: SelectionGranularity::Character,
                    selection_initial_range: None,
                    last_click: None,
                    selection_drag_pointer: None,
                    selection_auto_scroll_direction: 0,
                    selection_auto_scroll_timer: None,
                    selection_press_active: false,
                    scrollbar_drag: None,
                    scrollbar_hover: None,
                    scroll_to_end_indicator_rect: None,
                    active_search: None,
                    pressed_url: None,
                    selection_dragged: false,
                    mouse_capture: None,
                    mouse_press_target: None,
                    mouse_press_point: None,
                    mouse_press_moved: false,
                    last_component_click: None,
                    copy_on_select,
                }),
            }
        });

        let screen = Self { inner };
        screen.base().bind_overrides(Arc::new(screen.clone()));
        let input_weak = Arc::downgrade(&screen.inner);
        screen.base().add_input_listener(Arc::new(move |data| {
            input_weak
                .upgrade()
                .and_then(|inner| TuiAltScreen { inner }.handle_viewport_input(data))
        }));
        screen
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
        self.get_primary_scroll_view().scroll_top()
    }

    pub fn is_following_output(&self) -> bool {
        self.get_primary_scroll_view().is_following_end()
    }

    pub fn set_wheel_scroll_lines(&self, lines: WheelScrollLines) {
        self.inner.wheel_scroll.set_lines(lines);
    }

    pub fn get_copy_on_select(&self) -> bool {
        self.state().copy_on_select
    }

    pub fn set_copy_on_select(&self, enabled: bool) {
        self.state().copy_on_select = enabled;
    }

    /// Whether the fullscreen viewport has a non-empty active text selection.
    pub fn has_active_selection(&self) -> bool {
        self.get_active_selection_text().is_some()
    }

    /// Copy the active fullscreen text selection, if any, using the configured selection clipboard path.
    pub async fn copy_active_selection_to_clipboard(&self) -> bool {
        let Some(text) = self.get_active_selection_text() else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        self.copy_text_to_clipboard(&text).await
    }

    /// The lines of the last rendered frame, one per terminal row, as written to the terminal.
    pub fn get_screen_lines(&self) -> Vec<String> {
        self.state().previous_screen.clone()
    }

    pub fn set_layout_root(&self, component: Option<ComponentHandle>) {
        let changed = {
            let mut state = self.state();
            let same = match (&state.layout_root, &component) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if same {
                false
            } else {
                state.layout_root = component;
                state.current_layout = None;
                true
            }
        };
        if changed {
            self.request_render(None);
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let layout_root = self.state().layout_root.clone();
        if let Some(root) = layout_root {
            root.render(width)
        } else {
            self.base().render(width)
        }
    }

    /// PORT: `protected`. Visible to [`TuiBase`] in this crate.
    pub(crate) fn get_mounted_roots(&self) -> Vec<ComponentHandle> {
        if let Some(root) = self.state().layout_root.clone() {
            vec![root]
        } else {
            self.children()
        }
    }

    fn get_primary_scroll_view(&self) -> ScrollView {
        self.state()
            .current_layout
            .as_ref()
            .and_then(|layout| layout.primary_scroll_view.clone())
            .unwrap_or_else(|| self.inner.implicit_scroll_view.clone())
    }

    // Multiplexers can lag when every pointer movement is forwarded. Button-motion
    // tracking preserves clicks, wheel events, selections, and scrollbar dragging.
    /// PORT: `protected`.
    pub(crate) fn before_terminal_start(&self) {
        self.stop_selection_auto_scroll();
        self.stop_scrollbar_hover();
        self.stop_scrollbar_drag();
        self.inner.flashes.dispose();
        self.clear_component_mouse_gesture();

        let capabilities = crate::terminal_image::get_capabilities();
        let saved_iterm2 = capabilities.images == Some(ImageProtocol::Iterm2);
        {
            let mut state = self.state();
            state.selection_press_active = false;
            state.alt_screen_active = true;
            state.image_protocol = capabilities.images;
            state.uploaded_kitty_images.clear();
            if saved_iterm2 {
                state.saved_capabilities = Some(capabilities);
            }
            state.last_document.clear();
            state.selection_anchor = None;
            state.selection_focus = None;
            state.selection_granularity = SelectionGranularity::Character;
            state.selection_initial_range = None;
            state.last_click = None;
            state.pressed_url = None;
            state.selection_dragged = false;
            state.last_component_click = None;
        }
        if saved_iterm2 {
            crate::terminal_image::set_capabilities(TerminalCapabilities {
                images: None,
                ..capabilities
            });
            self.invalidate();
        }
        self.reset_render_state();

        let term = pi_js::env::var("TERM").unwrap_or_default().to_ascii_lowercase();
        let mouse_sequence = if pi_js::env::var("TMUX").is_some()
            || pi_js::env::var("ZELLIJ").is_some()
            || pi_js::env::var("STY").is_some()
            || term.starts_with("tmux")
            || term.starts_with("screen")
        {
            ENABLE_BUTTON_MOTION_MOUSE
        } else {
            ENABLE_ALL_MOTION_MOUSE
        };
        let mouse = if self.inner.mouse_enabled { mouse_sequence } else { "" };
        self.terminal()
            .write(&format!("{ENTER_ALT_SCREEN}{DISABLE_AUTOWRAP}{mouse}\x1b[2J\x1b[H\x1b[?25l"));
    }

    /// PORT: `protected`.
    pub(crate) fn before_terminal_stop(&self, _options: &TuiStopOptions) {
        self.close_search();
        self.stop_selection_auto_scroll();
        self.stop_scrollbar_hover();
        self.stop_scrollbar_drag();
        self.clear_component_mouse_gesture();
        self.inner.flashes.dispose();
        let active = {
            let mut state = self.state();
            state.selection_press_active = false;
            state.alt_screen_active
        };
        if !active {
            return;
        }
        let del = self.delete_kitty_images();
        let mouse = if self.inner.mouse_enabled { DISABLE_MOUSE } else { "" };
        self.terminal().write(&format!(
            "{BEGIN_SYNCHRONIZED_OUTPUT}{del}{mouse}{ENABLE_AUTOWRAP}{END_SYNCHRONIZED_OUTPUT}"
        ));
        self.state().uploaded_kitty_images.clear();
    }

    /// PORT: `protected`.
    pub(crate) fn after_terminal_stop(&self, options: &TuiStopOptions) {
        let was_active = {
            let mut state = self.state();
            if !state.alt_screen_active {
                false
            } else {
                state.alt_screen_active = false;
                true
            }
        };
        if !was_active {
            return;
        }

        if options.preserve_screen.unwrap_or(false) {
            self.terminal().write(&format!(
                "{BEGIN_SYNCHRONIZED_OUTPUT}{EXIT_ALT_SCREEN}\x1b[?25h{END_SYNCHRONIZED_OUTPUT}"
            ));
        } else {
            let terminal = self.terminal();
            let width = 1.max(terminal.columns());
            let document_lines: Vec<String> = self
                .render(width as usize)
                .into_iter()
                .map(|line| OSC133_ZONE_PREFIX.replace(&line, "").into_owned())
                .map(|line| line.replace(crate::tui::CURSOR_MARKER, ""))
                .collect();
            let last_document: Vec<String> = self
                .base()
                .apply_line_resets(document_lines)
                .into_iter()
                .map(|line| {
                    if crate::terminal_image::is_image_line(&line) || crate::utils::visible_width(&line) <= width {
                        line
                    } else {
                        crate::utils::slice_by_column(&line, 0, width, Some(true))
                    }
                })
                .collect();
            let mut buffer = format!("{BEGIN_SYNCHRONIZED_OUTPUT}{EXIT_ALT_SCREEN}{DISABLE_AUTOWRAP}");
            for (row, line) in last_document.iter().enumerate() {
                if row > 0 {
                    buffer.push_str("\r\n");
                }
                buffer.push_str("\r\x1b[2K");
                buffer.push_str(line);
            }
            buffer.push_str(&format!("\x1b[0m{ENABLE_AUTOWRAP}\r\n\x1b[?25h{END_SYNCHRONIZED_OUTPUT}"));
            terminal.write(&buffer);
            self.state().last_document = last_document;
        }

        let saved = self.state().saved_capabilities.take();
        if let Some(capabilities) = saved {
            crate::terminal_image::set_capabilities(capabilities);
        }
    }

    fn delete_kitty_images(&self) -> String {
        if self.state().image_protocol == Some(ImageProtocol::Kitty) {
            crate::terminal_image::delete_all_kitty_images()
        } else {
            String::new()
        }
    }

    fn prepare_kitty_screen(&self, screen: &[String]) -> PreparedKittyScreen {
        let mut state = self.state();
        let mut visible_image_ids = IndexSet::new();
        let mut lines = Vec::with_capacity(screen.len());
        for line in screen {
            let Some(placement) = crate::terminal_image::get_kitty_image_placement(line) else {
                lines.push(line.clone());
                continue;
            };
            visible_image_ids.insert(placement.image_id);
            let cached_image = state.uploaded_kitty_images.get(&placement.image_id).copied();
            let next_cached_image = CachedKittyImage {
                transmission_generation: placement.transmission_generation,
                transmission_bytes: placement.transmission_bytes,
                estimated_decoded_bytes: placement.estimated_decoded_bytes,
            };
            if cached_image.is_some() {
                state.uploaded_kitty_images.shift_remove(&placement.image_id);
            }
            state.uploaded_kitty_images.insert(placement.image_id, next_cached_image);
            if cached_image.is_some_and(|c| c.transmission_generation == placement.transmission_generation) {
                lines.push(placement.replacement_line);
            } else {
                lines.push(line.clone());
            }
        }

        let mut cached_offscreen_image_count = 0i64;
        let mut cached_offscreen_transmission_bytes = 0i64;
        let mut cached_offscreen_decoded_bytes = 0i64;
        for (&image_id, cached_image) in &state.uploaded_kitty_images {
            if visible_image_ids.contains(&image_id) {
                continue;
            }
            cached_offscreen_image_count += 1;
            cached_offscreen_transmission_bytes += cached_image.transmission_bytes;
            cached_offscreen_decoded_bytes += cached_image.estimated_decoded_bytes;
        }

        let mut to_evict = Vec::new();
        for (&image_id, cached_image) in &state.uploaded_kitty_images {
            if cached_offscreen_image_count <= MAX_CACHED_OFFSCREEN_KITTY_IMAGES
                && cached_offscreen_transmission_bytes <= MAX_CACHED_OFFSCREEN_KITTY_TRANSMISSION_BYTES
                && cached_offscreen_decoded_bytes <= MAX_CACHED_OFFSCREEN_KITTY_DECODED_BYTES
            {
                break;
            }
            if visible_image_ids.contains(&image_id) {
                continue;
            }
            to_evict.push(image_id);
            cached_offscreen_image_count -= 1;
            cached_offscreen_transmission_bytes -= cached_image.transmission_bytes;
            cached_offscreen_decoded_bytes -= cached_image.estimated_decoded_bytes;
        }

        let mut evicted_image_deletion = String::new();
        for image_id in to_evict {
            evicted_image_deletion.push_str(&crate::terminal_image::delete_kitty_image(image_id));
            state.uploaded_kitty_images.shift_remove(&image_id);
        }

        PreparedKittyScreen {
            lines,
            evicted_image_deletion,
        }
    }

    /// PORT: `protected`.
    pub(crate) fn reset_render_state(&self) {
        let mut state = self.state();
        state.previous_screen.clear();
        state.previous_screen_width = 0;
        state.previous_screen_height = 0;
        state.current_layout = None;
    }

    pub fn scroll_by(&self, lines: i64) {
        self.get_primary_scroll_view().scroll_by(lines as f64);
        self.request_render(None);
    }

    pub fn scroll_to_top(&self) {
        self.get_primary_scroll_view().scroll_to_start();
        self.request_render(None);
    }

    pub fn scroll_to_bottom(&self) {
        self.get_primary_scroll_view().scroll_to_end();
        self.request_render(None);
    }

    fn scroll_to_prompt(&self, direction: i64) {
        let scroll_view = {
            let state = self.state();
            if state.current_layout.is_none() {
                return;
            }
            state
                .current_layout
                .as_ref()
                .and_then(|l| l.primary_scroll_view.clone())
                .unwrap_or_else(|| self.inner.implicit_scroll_view.clone())
        };
        let lines = {
            let state = self.state();
            let Some(ref layout) = state.current_layout else {
                return;
            };
            crate::layout::get_scroll_view_box(layout, &scroll_view).and_then(|b| b.scroll_content_lines())
        };
        let Some(lines) = lines else {
            return;
        };
        let mut row = scroll_view.scroll_top() + direction;
        while row >= 0 && (row as usize) < lines.len() {
            if OSC133_PROMPT_START.is_match(&lines[row as usize]) {
                scroll_view.scroll_to(row as f64, None);
                self.request_render(None);
                return;
            }
            row += direction;
        }
    }

    fn toggle_search(&self) {
        if self.state().active_search.is_some() {
            self.close_search();
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let component = AltScreenSearchComponent::new(
            Arc::new(move |query| {
                if let Some(inner) = weak.upgrade() {
                    TuiAltScreen { inner }.update_search_query(query);
                }
            }),
            Some(Arc::clone(&self.inner.search_navigation_button_style)),
        );
        let comp_handle: ComponentHandle = Arc::new(component.clone());
        let anchor_row = self.get_primary_scroll_view().scroll_top();
        {
            let mut state = self.state();
            state.active_search = Some(ActiveSearch {
                component,
                index: AltScreenSearchIndex::new(),
                overlay: None,
                query: String::new(),
                matches: Vec::new(),
                selected_index: -1,
                selected_key: None,
                anchor_row,
                selection_mode: SearchSelectionMode::Query,
            });
        }
        let overlay = self.show_overlay(
            comp_handle,
            Some(OverlayOptions {
                anchor: Some(crate::tui::OverlayAnchor::TopRight),
                width: Some(crate::tui::SizeValue::Percent("40%".to_string())),
                min_width: Some(32),
                margin: Some(crate::tui::OverlayMarginValue::All(1)),
                ..Default::default()
            }),
        );
        let mut state = self.state();
        if let Some(ref mut search) = state.active_search {
            search.overlay = Some(overlay);
        }
    }

    fn close_search(&self) {
        let overlay = {
            let mut state = self.state();
            let Some(search) = state.active_search.take() else {
                return;
            };
            search.overlay
        };
        if let Some(ov) = overlay {
            ov.hide();
        }
        self.request_render(None);
    }

    fn update_search_query(&self, query: &str) {
        let scroll_top = self.get_primary_scroll_view().scroll_top();
        let search_component = {
            let mut state = self.state();
            let Some(ref mut search) = state.active_search else {
                return;
            };
            if search.query == query {
                return;
            }
            let selected_row = usize::try_from(search.selected_index)
                .ok()
                .and_then(|idx| search.matches.get(idx))
                .and_then(|m| m.segments.first())
                .map(|s| s.row)
                .unwrap_or(scroll_top);
            search.anchor_row = selected_row;
            search.query = query.to_string();
            search.selection_mode = SearchSelectionMode::Query;
            search.component.clone()
        };
        search_component.set_result(-1, 0);
        self.request_render(None);
    }

    fn navigate_search(&self, direction: i64) {
        let should_render = {
            let mut state = self.state();
            let Some(ref mut search) = state.active_search else {
                return;
            };
            if search.query.is_empty() {
                false
            } else {
                search.selection_mode = if direction < 0 {
                    SearchSelectionMode::Previous
                } else {
                    SearchSelectionMode::Next
                };
                true
            }
        };
        if should_render {
            self.request_render(None);
        }
    }

    fn get_search_navigation_direction_at(&self, x: i64, y: i64) -> Option<i64> {
        let (component, overlay) = {
            let state = self.state();
            let search = state.active_search.as_ref()?;
            (search.component.clone(), search.overlay.clone()?)
        };
        let bounds = overlay.get_bounds()?;
        if x < bounds.col || x >= bounds.col + bounds.width || y < bounds.row || y >= bounds.row + bounds.height {
            return None;
        }
        component.get_navigation_direction_at(y - bounds.row, x - bounds.col)
    }

    fn handle_search_mouse_event(&self, event: &SgrMouseEvent) -> bool {
        let component = {
            let state = self.state();
            let Some(ref search) = state.active_search else {
                return false;
            };
            search.component.clone()
        };
        let direction = self.get_search_navigation_direction_at(event.x, event.y);
        if component.set_hovered_navigation_direction(direction) {
            self.request_render(None);
        }
        let Some(dir) = direction else {
            return false;
        };
        if event.release || (event.button & 32) != 0 || (event.button & 3) != 0 {
            return false;
        }
        self.navigate_search(dir);
        true
    }

    fn refresh_search(&self, layout: &LayoutFrame) -> bool {
        let (component, index, query, prev_matches, prev_selected_index, prev_selected_key, anchor_row, mode) = {
            let state = self.state();
            let Some(ref search) = state.active_search else {
                return false;
            };
            (
                search.component.clone(),
                search.index.clone(),
                search.query.clone(),
                search.matches.clone(),
                search.selected_index,
                search.selected_key.clone(),
                search.anchor_row,
                search.selection_mode,
            )
        };
        let scroll_view = layout
            .primary_scroll_view
            .clone()
            .unwrap_or_else(|| self.inner.implicit_scroll_view.clone());
        let r#box = crate::layout::get_scroll_view_box(layout, &scroll_view);
        let lines = r#box.as_ref().and_then(|b| b.scroll_content_lines());
        let Some(lines) = lines.filter(|_| !query.trim().is_empty()) else {
            {
                let mut state = self.state();
                if let Some(ref mut search) = state.active_search {
                    search.matches.clear();
                    search.selected_index = -1;
                    search.selected_key = None;
                    search.selection_mode = SearchSelectionMode::Retain;
                }
            }
            component.set_result(-1, 0);
            return false;
        };

        let should_reveal_selection = mode != SearchSelectionMode::Retain;
        let result = index.search(&lines, &query);
        let matches = result.matches;
        if !result.changed && mode == SearchSelectionMode::Retain {
            let mut state = self.state();
            if let Some(ref mut search) = state.active_search {
                search.matches = matches;
            }
            return false;
        }
        let _ = prev_matches;

        let exact_index = if result.changed {
            prev_selected_key
                .as_deref()
                .and_then(|key| {
                    matches
                        .iter()
                        .position(|m| crate::alt_screen_search::get_alt_screen_search_match_key(m) == key)
                        .map(|u| u as i64)
                })
                .unwrap_or(-1)
        } else {
            prev_selected_index
        };

        let mut selected_index = -1i64;
        if !matches.is_empty() {
            let len_i64 = matches.len() as i64;
            selected_index = match mode {
                SearchSelectionMode::Query => {
                    let mut low = 0usize;
                    let mut high = matches.len();
                    while low < high {
                        let middle = low + (high - low) / 2;
                        let seg_row = matches[middle].segments.first().map(|s| s.row).unwrap_or(0);
                        if seg_row < anchor_row {
                            low = middle + 1;
                        } else {
                            high = middle;
                        }
                    }
                    if low < matches.len() { low as i64 } else { 0 }
                }
                SearchSelectionMode::Next => {
                    let base_index = if exact_index >= 0 {
                        exact_index
                    } else {
                        prev_selected_index.min(len_i64 - 1)
                    };
                    if base_index < 0 { 0 } else { (base_index + 1) % len_i64 }
                }
                SearchSelectionMode::Previous => {
                    let base_index = if exact_index >= 0 {
                        exact_index
                    } else {
                        prev_selected_index.min(len_i64 - 1)
                    };
                    if base_index < 0 {
                        len_i64 - 1
                    } else {
                        (base_index - 1 + len_i64) % len_i64
                    }
                }
                SearchSelectionMode::Retain => {
                    if exact_index >= 0 {
                        exact_index
                    } else {
                        0.max(prev_selected_index).min(len_i64 - 1)
                    }
                }
            };
        }

        let selected_match = usize::try_from(selected_index)
            .ok()
            .and_then(|idx| matches.get(idx))
            .cloned();
        let selected_key = selected_match
            .as_ref()
            .map(crate::alt_screen_search::get_alt_screen_search_match_key);
        let match_count = matches.len() as i64;

        {
            let mut state = self.state();
            if let Some(ref mut search) = state.active_search {
                search.matches = matches;
                search.selected_index = selected_index;
                search.selected_key = selected_key;
                search.selection_mode = SearchSelectionMode::Retain;
            }
        }
        component.set_result(selected_index, match_count);
        if !should_reveal_selection {
            return false;
        }

        let Some(selected) = selected_match else {
            return false;
        };
        let (Some(first_segment), Some(last_segment)) = (selected.segments.first(), selected.segments.last()) else {
            return false;
        };
        let viewport_height = scroll_view.viewport_height();
        if r#box.is_none() || viewport_height <= 0 {
            return false;
        }
        let before = scroll_view.scroll_top();
        let visible_bottom = before + viewport_height - 1;
        let mut target = before;
        if first_segment.row < before || last_segment.row > visible_bottom {
            target = first_segment.row - viewport_height / 3;
        }
        scroll_view.scroll_to(
            target as f64,
            Some(crate::components::scroll_view::ScrollViewScrollToOptions {
                disable_follow: Some(true),
            }),
        );
        scroll_view.scroll_top() != before
    }

    /// Show a transient message in the alternate-screen flash stack.
    ///
    /// `duration_ms` `None` lets the flash container apply its default.
    pub fn flash(&self, message: &str, duration_ms: Option<i64>) {
        self.inner.flashes.flash(message, duration_ms);
    }

    fn should_defer_viewport_input_to_overlay(&self) -> bool {
        let search_overlay_focused = self
            .state()
            .active_search
            .as_ref()
            .and_then(|s| s.overlay.as_ref())
            .is_some_and(OverlayHandle::is_focused);
        self.base().is_overlay_focused() && !search_overlay_focused
    }

    fn clear_component_mouse_gesture(&self) {
        let mut state = self.state();
        state.mouse_capture = None;
        state.mouse_press_target = None;
        state.mouse_press_point = None;
        state.mouse_press_moved = false;
    }

    // SGR mouse button codes use bit 3 (value 8) for the Alt modifier.
    fn handle_viewport_input(&self, data: &str) -> Option<TuiInputListenerResult> {
        if data == FOCUS_OUT {
            let had_active_selection = self.state().selection_press_active;
            let had_non_empty_active_selection = had_active_selection && self.get_selection_bounds().is_some();
            {
                let mut state = self.state();
                state.selection_press_active = false;
            }
            self.stop_selection_auto_scroll();
            self.stop_scrollbar_hover();
            let search_comp = self.state().active_search.as_ref().map(|s| s.component.clone());
            if let Some(comp) = search_comp {
                if comp.set_hovered_navigation_direction(None) {
                    self.request_render(None);
                }
            }
            self.stop_scrollbar_drag();
            {
                let mut state = self.state();
                state.pressed_url = None;
                state.selection_dragged = false;
                state.last_component_click = None;
            }
            self.clear_component_mouse_gesture();
            if had_active_selection {
                {
                    let mut state = self.state();
                    state.selection_anchor = None;
                    state.selection_focus = None;
                    state.selection_granularity = SelectionGranularity::Character;
                    state.selection_initial_range = None;
                }
                if had_non_empty_active_selection {
                    self.request_render(None);
                }
            }
            self.state().last_click = None;
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if data == FOCUS_IN {
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }

        if let Some(wheel_event) = self.parse_wheel_event(data) {
            let lines = self
                .inner
                .wheel_scroll
                .next(wheel_event.direction, pi_js::time::performance_now());
            // SGR mouse button codes use bit 3 (value 8) for the Alt modifier.
            let wheel_delta = wheel_event.direction
                * if (wheel_event.button & 8) != 0 {
                    lines * ALT_WHEEL_SCROLL_MULTIPLIER
                } else {
                    lines
                };
            let event = self.create_mouse_event(
                crate::tui::TuiMouseEventType::Wheel,
                wheel_event.button,
                wheel_event.x,
                wheel_event.y,
                Some(MouseEventExtra {
                    wheel_delta: Some(wheel_delta),
                    click_count: None,
                }),
            );
            let overlay = self.base().dispatch_mouse_to_overlay(&event);
            let result = overlay
                .result
                .or_else(|| if overlay.hit { None } else { self.dispatch_mouse_to_layout(&event) });
            if let Some(res) = result {
                if self.apply_mouse_dispatch_result(&event, &res) {
                    self.request_render(None);
                }
                return Some(TuiInputListenerResult {
                    consume: Some(true),
                    data: None,
                });
            }
            if self.should_defer_viewport_input_to_overlay() {
                return None;
            }
            self.route_wheel(&wheel_event, wheel_delta);
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if let Some(mouse_event) = self.parse_sgr_mouse_event(data) {
            self.handle_mouse_event(&mouse_event);
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if self.is_mouse_sequence(data) {
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }

        let keybindings = crate::keybindings::get_keybindings();
        let is_release = crate::keys::is_key_release(data);
        if keybindings.matches(data, "tui.altScreen.search") {
            if !is_release {
                self.toggle_search();
            }
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        let search_focused = self
            .state()
            .active_search
            .as_ref()
            .and_then(|s| s.overlay.as_ref())
            .is_some_and(OverlayHandle::is_focused);
        if search_focused {
            if keybindings.matches(data, "tui.altScreen.searchNext") {
                if !is_release {
                    self.navigate_search(1);
                }
                return Some(TuiInputListenerResult {
                    consume: Some(true),
                    data: None,
                });
            }
            if keybindings.matches(data, "tui.altScreen.searchPrevious") {
                if !is_release {
                    self.navigate_search(-1);
                }
                return Some(TuiInputListenerResult {
                    consume: Some(true),
                    data: None,
                });
            }
            if keybindings.matches(data, "tui.altScreen.searchClose") {
                if !is_release {
                    self.close_search();
                }
                return Some(TuiInputListenerResult {
                    consume: Some(true),
                    data: None,
                });
            }
        }
        if self.should_defer_viewport_input_to_overlay() {
            return None;
        }
        if keybindings.matches(data, "tui.altScreen.pageUp") {
            if !is_release {
                let step = 1.max(self.get_primary_scroll_view().viewport_height() - PAGE_SCROLL_OVERLAP);
                self.scroll_by(-step);
            }
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if keybindings.matches(data, "tui.altScreen.pageDown") {
            if !is_release {
                let step = 1.max(self.get_primary_scroll_view().viewport_height() - PAGE_SCROLL_OVERLAP);
                self.scroll_by(step);
            }
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if keybindings.matches(data, "tui.altScreen.halfPageUp") {
            if !is_release {
                let step = 1.max(self.get_primary_scroll_view().viewport_height() / 2);
                self.scroll_by(-step);
            }
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if keybindings.matches(data, "tui.altScreen.halfPageDown") {
            if !is_release {
                let step = 1.max(self.get_primary_scroll_view().viewport_height() / 2);
                self.scroll_by(step);
            }
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if keybindings.matches(data, "tui.altScreen.lineUp") {
            if !is_release {
                self.scroll_by(-1);
            }
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if keybindings.matches(data, "tui.altScreen.lineDown") {
            if !is_release {
                self.scroll_by(1);
            }
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if keybindings.matches(data, "tui.altScreen.previousPrompt") {
            if !is_release {
                self.scroll_to_prompt(-1);
            }
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if keybindings.matches(data, "tui.altScreen.nextPrompt") {
            if !is_release {
                self.scroll_to_prompt(1);
            }
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if keybindings.matches(data, "tui.altScreen.top") {
            if !is_release {
                self.scroll_to_top();
            }
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        if keybindings.matches(data, "tui.altScreen.bottom") {
            if !is_release {
                self.scroll_to_bottom();
            }
            return Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            });
        }
        None
    }

    fn decode_mouse_button(&self, button: i64) -> TuiMouseButton {
        match button & 3 {
            0 => TuiMouseButton::Left,
            1 => TuiMouseButton::Middle,
            2 => TuiMouseButton::Right,
            _ => TuiMouseButton::None,
        }
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
        let extra = extra.unwrap_or(MouseEventExtra {
            wheel_delta: None,
            click_count: None,
        });
        let terminal = self.terminal();
        TuiMouseEvent {
            r#type,
            button: if r#type == crate::tui::TuiMouseEventType::Wheel {
                TuiMouseButton::None
            } else {
                self.decode_mouse_button(button)
            },
            x,
            y,
            screen_x: x,
            screen_y: y,
            width: 1.max(terminal.columns()),
            height: 1.max(terminal.rows()),
            shift: (button & 4) != 0,
            alt: (button & 8) != 0,
            ctrl: (button & 16) != 0,
            wheel_delta: extra.wheel_delta,
            click_count: extra.click_count,
        }
    }

    fn dispatch_mouse_to_layout(&self, event: &TuiMouseEvent) -> Option<TuiMouseDispatchResult> {
        let layout = self.state().current_layout.clone()?;
        let mut visited = IndexSet::new();
        let boxes = crate::layout::get_layout_boxes_at(&layout, event.screen_x, event.screen_y);
        for layout_box in boxes {
            let comp = layout_box.component();
            let comp_addr = Arc::as_ptr(&comp) as *const () as usize;
            if visited.contains(&comp_addr) {
                continue;
            }
            if crate::layout_node::get_layout_node(comp.as_ref()).is_some() && comp.is_container_handle_mouse() {
                continue;
            }
            visited.insert(comp_addr);
            let rect = layout_box.rect();
            let local_event = TuiMouseEvent {
                x: event.screen_x - rect.x,
                y: event.screen_y - rect.y,
                width: rect.width,
                height: rect.height,
                ..*event
            };
            if let Some(result) = crate::tui::dispatch_mouse_event_arc(&comp, &local_event) {
                return Some(result);
            }
        }
        None
    }

    fn apply_mouse_dispatch_result(&self, event: &TuiMouseEvent, result: &TuiMouseDispatchResult) -> bool {
        let raw_focus = result
            .focus_target
            .as_ref()
            .cloned()
            .unwrap_or_else(|| Arc::clone(&result.target.component));
        let focus_target = self.base().resolve_mouse_focus_target(raw_focus);
        let focus_changed = result.focus == Some(true)
            && self
                .get_focused_component()
                .is_none_or(|fc| !Arc::ptr_eq(&fc, &focus_target));
        if result.focus == Some(true) {
            self.set_focus(Some(focus_target));
        }
        if result.capture == Some(true) {
            self.state().mouse_capture = Some(result.target.clone());
        }
        result.render.unwrap_or_else(|| {
            focus_changed
                || matches!(
                    event.r#type,
                    crate::tui::TuiMouseEventType::Press
                        | crate::tui::TuiMouseEventType::Click
                        | crate::tui::TuiMouseEventType::Drag
                        | crate::tui::TuiMouseEventType::Wheel
                )
        })
    }

    fn dispatch_mouse_to_target(
        &self,
        event: &TuiMouseEvent,
        target: &TuiMouseDispatchTarget,
    ) -> Option<TuiMouseDispatchResult> {
        let retargeted = crate::tui::retarget_mouse_event(event, target);
        crate::tui::dispatch_mouse_event_arc(&target.component, &retargeted)
    }

    fn get_component_click_count(&self, target: &TuiMouseDispatchTarget, x: i64, y: i64) -> i64 {
        let now = pi_js::time::now_ms();
        let mut state = self.state();
        let count = match state.last_component_click.as_ref() {
            Some(previous)
                if now - previous.timestamp <= DOUBLE_CLICK_INTERVAL_MS
                    && Arc::ptr_eq(&previous.component, &target.component)
                    && previous.x == x
                    && previous.y == y =>
            {
                (previous.count % 3) + 1
            }
            _ => 1,
        };
        state.last_component_click = Some(ComponentClick {
            timestamp: now,
            count,
            component: Arc::clone(&target.component),
            x,
            y,
        });
        count
    }

    fn clear_text_selection(&self) {
        self.stop_selection_auto_scroll();
        let mut state = self.state();
        state.selection_press_active = false;
        state.selection_anchor = None;
        state.selection_focus = None;
        state.selection_granularity = SelectionGranularity::Character;
        state.selection_initial_range = None;
        state.pressed_url = None;
        state.selection_dragged = false;
    }

    fn handle_mouse_event(&self, raw: &SgrMouseEvent) {
        let is_motion = (raw.button & 32) != 0;
        let event_type = if raw.release {
            crate::tui::TuiMouseEventType::Release
        } else if is_motion {
            if self.decode_mouse_button(raw.button) == TuiMouseButton::None {
                crate::tui::TuiMouseEventType::Move
            } else {
                crate::tui::TuiMouseEventType::Drag
            }
        } else {
            crate::tui::TuiMouseEventType::Press
        };
        let event = self.create_mouse_event(event_type, raw.button, raw.x, raw.y, None);

        let active_target = {
            let state = self.state();
            state
                .mouse_capture
                .clone()
                .or_else(|| state.mouse_press_target.clone())
        };
        if let Some(target) = active_target {
            let (moved_now, match_press_point) = {
                let mut state = self.state();
                if let Some(pp) = state.mouse_press_point {
                    if raw.x != pp.x || raw.y != pp.y {
                        state.mouse_press_moved = true;
                        state.last_component_click = None;
                    }
                    (!state.mouse_press_moved && pp.x == raw.x && pp.y == raw.y, true)
                } else {
                    (false, false)
                }
            };
            let _ = match_press_point;
            let mut render = false;
            if let Some(target_result) = self.dispatch_mouse_to_target(&event, &target) {
                render = self.apply_mouse_dispatch_result(&event, &target_result);
            }
            if raw.release {
                if moved_now {
                    let click_count = self.get_component_click_count(&target, raw.x, raw.y);
                    let click_event = self.create_mouse_event(
                        crate::tui::TuiMouseEventType::Click,
                        raw.button,
                        raw.x,
                        raw.y,
                        Some(MouseEventExtra {
                            wheel_delta: None,
                            click_count: Some(click_count),
                        }),
                    );
                    if let Some(click_result) = self.dispatch_mouse_to_target(&click_event, &target) {
                        render = self.apply_mouse_dispatch_result(&click_event, &click_result) || render;
                    }
                }
                self.clear_component_mouse_gesture();
            }
            if render {
                self.request_render(None);
            }
            return;
        }

        if self.handle_search_mouse_event(raw) {
            return;
        }

        let overlay = self.base().dispatch_mouse_to_overlay(&event);
        if !overlay.hit {
            if self.handle_scroll_to_end_indicator_mouse_event(raw) {
                return;
            }
            let scrollbar_handled = self.handle_scrollbar_mouse_event(raw);
            if self.state().scrollbar_drag.is_none() {
                self.update_scrollbar_hover(raw.x, raw.y);
            }
            if scrollbar_handled {
                return;
            }
        } else {
            self.stop_scrollbar_hover();
        }

        let result = overlay
            .result
            .or_else(|| if overlay.hit { None } else { self.dispatch_mouse_to_layout(&event) });
        if let Some(res) = result {
            let render = self.apply_mouse_dispatch_result(&event, &res);
            if event_type == crate::tui::TuiMouseEventType::Press {
                self.clear_text_selection();
                let mut state = self.state();
                state.mouse_press_target = Some(res.target);
                state.mouse_press_point = Some(PointerPoint { x: raw.x, y: raw.y });
                state.mouse_press_moved = false;
            }
            if render {
                self.request_render(None);
            }
            return;
        }

        if self.handle_right_click_paste(raw) {
            return;
        }
        self.handle_selection_mouse_event(raw);
    }

    fn parse_wheel_event(&self, data: &str) -> Option<WheelEvent> {
        if let Some(caps) = SGR_WHEEL_REGEX.captures(data) {
            let button: i64 = caps.get(1)?.as_str().parse().ok()?;
            if (button & 64) == 0 {
                return None;
            }
            let dir_bits = button & 3;
            if dir_bits != 0 && dir_bits != 1 {
                return None;
            }
            let x: i64 = caps.get(2)?.as_str().parse().ok()?;
            let y: i64 = caps.get(3)?.as_str().parse().ok()?;
            return Some(WheelEvent {
                direction: if dir_bits == 0 { -1 } else { 1 },
                x: x - 1,
                y: y - 1,
                button,
            });
        }
        if data.starts_with("\x1b[M") && data.chars().count() == 6 {
            let mut chars = data.chars().skip(3);
            let b_ch = chars.next()?;
            let x_ch = chars.next()?;
            let y_ch = chars.next()?;
            let button = b_ch as i64 - 32;
            if (button & 64) == 0 {
                return None;
            }
            let dir_bits = button & 3;
            if dir_bits != 0 && dir_bits != 1 {
                return None;
            }
            return Some(WheelEvent {
                direction: if dir_bits == 0 { -1 } else { 1 },
                x: x_ch as i64 - 33,
                y: y_ch as i64 - 33,
                button,
            });
        }
        None
    }

    fn route_wheel(&self, event: &WheelEvent, delta: i64) {
        let mut remaining = delta;
        let mut seen: Vec<ScrollView> = Vec::new();
        let views = self
            .state()
            .current_layout
            .as_ref()
            .map(|l| crate::layout::get_scroll_views_at(l, event.x, event.y))
            .unwrap_or_default();
        for scroll_view in views {
            seen.push(scroll_view.clone());
            remaining = scroll_view.scroll_by(remaining as f64);
            if remaining == 0 || scroll_view.overscroll() == crate::layout_node::ScrollOverscroll::Contain {
                break;
            }
        }
        let primary = self.get_primary_scroll_view();
        if remaining != 0 && !seen.iter().any(|s| s == &primary) {
            primary.scroll_by(remaining as f64);
        }
        self.update_scrollbar_hover(event.x, event.y);
        self.request_render(None);
    }

    fn parse_sgr_mouse_event(&self, data: &str) -> Option<SgrMouseEvent> {
        let caps = SGR_MOUSE_REGEX.captures(data)?;
        let button: i64 = caps.get(1)?.as_str().parse().ok()?;
        let x: i64 = caps.get(2)?.as_str().parse().ok()?;
        let y: i64 = caps.get(3)?.as_str().parse().ok()?;
        let release = caps.get(4)?.as_str() == "m";
        Some(SgrMouseEvent {
            button,
            x: x - 1,
            y: y - 1,
            release,
        })
    }

    fn handle_right_click_paste(&self, event: &SgrMouseEvent) -> bool {
        let Some(ref on_paste) = self.inner.on_right_click_paste else {
            return false;
        };
        if !cfg!(windows)
            || pi_js::env::var("TERM_PROGRAM")
                .map(|v| v.to_ascii_lowercase())
                .as_deref()
                == Some("vscode")
            || event.release
            || event.button != 2
        {
            return false;
        }
        on_paste();
        true
    }

    fn handle_scroll_to_end_indicator_mouse_event(&self, event: &SgrMouseEvent) -> bool {
        let rect = self.state().scroll_to_end_indicator_rect;
        let Some(rect) = rect else {
            return false;
        };
        if event.release || (event.button & 32) != 0 || (event.button & 3) != 0 {
            return false;
        }
        if event.y != rect.row || event.x < rect.column || event.x >= rect.column + rect.width {
            return false;
        }
        self.scroll_to_bottom();
        true
    }

    /// `include_hidden_auto` `None` is `false`.
    fn get_scrollbar_target_at(&self, x: i64, y: i64, include_hidden_auto: Option<bool>) -> Option<ScrollbarTarget> {
        if self.has_overlay() {
            return None;
        }
        let layout = self.state().current_layout.clone()?;
        for scroll_view in crate::layout::get_scroll_views_at(&layout, x, y) {
            let Some(r#box) = crate::layout::get_scroll_view_box(&layout, &scroll_view) else {
                continue;
            };
            let Some(geometry) = crate::layout::get_scrollbar_geometry(&r#box, include_hidden_auto) else {
                continue;
            };
            if x == geometry.column && y >= geometry.track_top && y < geometry.track_top + geometry.track_height {
                return Some(ScrollbarTarget { scroll_view, geometry });
            }
        }
        None
    }

    fn set_scrollbar_hover(&self, scroll_view: Option<&ScrollView>) {
        let prev = {
            let mut state = self.state();
            if state.scrollbar_hover.as_ref() == scroll_view {
                return;
            }
            let prev = state.scrollbar_hover.take();
            state.scrollbar_hover = scroll_view.cloned();
            prev
        };
        if let Some(p) = prev {
            p.set_scrollbar_active(false);
        }
        if let Some(sv) = scroll_view {
            sv.set_scrollbar_active(true);
        }
    }

    fn update_scrollbar_hover(&self, x: i64, y: i64) {
        let target = self.get_scrollbar_target_at(x, y, Some(true));
        self.set_scrollbar_hover(target.as_ref().map(|t| &t.scroll_view));
    }

    fn stop_scrollbar_hover(&self) {
        self.set_scrollbar_hover(None);
    }

    fn scroll_scrollbar_to_pointer(
        &self,
        scroll_view: &ScrollView,
        geometry: &ScrollbarGeometry,
        pointer_y: i64,
        grab_offset: i64,
    ) {
        let max_thumb_offset = geometry.track_height - geometry.thumb_height;
        let thumb_offset = 0.max(max_thumb_offset.min(pointer_y - geometry.track_top - grab_offset));
        let scroll_top = if max_thumb_offset == 0 {
            0.0
        } else {
            ((thumb_offset as f64 / max_thumb_offset as f64) * geometry.max_scroll_top as f64).round()
        };
        scroll_view.scroll_to(scroll_top, None);
    }

    fn handle_scrollbar_mouse_event(&self, event: &SgrMouseEvent) -> bool {
        let drag = self.state().scrollbar_drag.clone();
        if let Some(drag) = drag {
            if event.release {
                self.stop_scrollbar_drag();
                return true;
            }
            let layout = self.state().current_layout.clone();
            if let Some(ref l) = layout {
                if let Some(r#box) = crate::layout::get_scroll_view_box(l, &drag.scroll_view) {
                    if let Some(geometry) = crate::layout::get_scrollbar_geometry(&r#box, None) {
                        self.scroll_scrollbar_to_pointer(&drag.scroll_view, &geometry, event.y, drag.grab_offset);
                    }
                }
            }
            return true;
        }

        if event.release || (event.button & 32) != 0 || (event.button & 3) != 0 {
            return false;
        }
        let Some(target) = self.get_scrollbar_target_at(event.x, event.y, None) else {
            return false;
        };
        self.stop_selection_auto_scroll();
        {
            let mut state = self.state();
            state.selection_press_active = false;
            state.selection_anchor = None;
            state.selection_focus = None;
            state.selection_granularity = SelectionGranularity::Character;
            state.selection_initial_range = None;
            state.last_click = None;
            state.pressed_url = None;
            state.selection_dragged = false;
        }
        self.set_scrollbar_hover(Some(&target.scroll_view));
        let on_thumb = event.y >= target.geometry.thumb_top
            && event.y < target.geometry.thumb_top + target.geometry.thumb_height;
        let grab_offset = if on_thumb {
            event.y - target.geometry.thumb_top
        } else {
            target.geometry.thumb_height / 2
        };
        if !on_thumb {
            self.scroll_scrollbar_to_pointer(&target.scroll_view, &target.geometry, event.y, grab_offset);
        }
        self.state().scrollbar_drag = Some(ScrollbarDrag {
            scroll_view: target.scroll_view,
            grab_offset,
        });
        true
    }

    fn stop_scrollbar_drag(&self) {
        self.state().scrollbar_drag = None;
    }

    fn get_scroll_selection_point(&self, scroll_view: &ScrollView, x: i64, y: i64) -> Option<SelectionPoint> {
        let layout = self.state().current_layout.clone()?;
        let r#box = crate::layout::get_scroll_view_box(&layout, scroll_view)?;
        let rect = r#box.rect();
        let clip = r#box.clip();
        if rect.height <= 0 || clip.height <= 0 {
            return None;
        }
        let rows = self.terminal().rows();
        let visible_top = 0.max(rect.y).max(clip.y);
        let visible_bottom = (rows - 1)
            .min(rect.y + rect.height - 1)
            .min(clip.y + clip.height - 1);
        if visible_bottom < visible_top {
            return None;
        }
        let pointer_row = visible_top.max(visible_bottom.min(y));
        let content_len = r#box.scroll_content_lines().map(|l| l.len()).unwrap_or(1) as i64;
        let max_content_row = 0.max(content_len - 1);
        Some(SelectionPoint {
            row: 0.max(max_content_row.min(scroll_view.scroll_top() + pointer_row - rect.y)),
            col: 0.max((rect.width - 1).min(x - rect.x)),
            scroll_view: Some(scroll_view.clone()),
            boundary: false,
        })
    }

    fn get_selection_point(&self, event: &SgrMouseEvent, scroll_view: Option<&ScrollView>) -> SelectionPoint {
        if let Some(sv) = scroll_view {
            if let Some(point) = self.get_scroll_selection_point(sv, event.x, event.y) {
                return point;
            }
        }
        let terminal = self.terminal();
        SelectionPoint {
            row: 0.max((terminal.rows() - 1).min(event.y)),
            col: 0.max((terminal.columns() - 1).min(event.x)),
            scroll_view: None,
            boundary: false,
        }
    }

    fn get_selection_source_line(&self, point: &SelectionPoint) -> String {
        let state = self.state();
        if let (Some(sv), Some(layout)) = (&point.scroll_view, &state.current_layout) {
            if let Some(lines) = crate::layout::get_scroll_view_box(layout, sv).and_then(|b| b.scroll_content_lines())
            {
                return usize::try_from(point.row)
                    .ok()
                    .and_then(|r| lines.get(r).cloned())
                    .unwrap_or_default();
            }
        }
        usize::try_from(point.row)
            .ok()
            .and_then(|r| state.previous_screen.get(r).cloned())
            .unwrap_or_default()
    }

    fn get_word_selection(&self, point: &SelectionPoint) -> Option<SelectionRange> {
        let line = crate::utils::strip_terminal_sequences(&self.get_selection_source_line(point));
        struct WordSeg {
            start: i64,
            end: i64,
            selectable: bool,
            joiner: bool,
        }
        let mut segments = Vec::new();
        let mut start = 0i64;
        for seg in crate::utils::get_word_segmenter().segment(&line) {
            let end = start + crate::utils::visible_width(&seg.segment);
            let joiner = TERMINAL_WORD_SELECTION_JOINERS.contains(seg.segment.as_str());
            segments.push(WordSeg {
                start,
                end,
                selectable: seg.is_word_like == Some(true) || joiner,
                joiner,
            });
            start = end;
        }
        let clicked_index = segments
            .iter()
            .position(|seg| point.col >= seg.start && point.col < seg.end)?;

        let can_join = |left: &WordSeg, right: &WordSeg| -> bool {
            left.selectable && right.selectable && (left.joiner || right.joiner)
        };
        let mut selection_start = segments[clicked_index].start;
        let mut selection_end = segments[clicked_index].end;
        let mut index = clicked_index;
        while index > 0 && can_join(&segments[index - 1], &segments[index]) {
            selection_start = segments[index - 1].start;
            index -= 1;
        }
        let mut index = clicked_index;
        while index + 1 < segments.len() && can_join(&segments[index], &segments[index + 1]) {
            selection_end = segments[index + 1].end;
            index += 1;
        }
        Some(SelectionRange {
            start: SelectionPoint {
                col: selection_start,
                boundary: false,
                ..point.clone()
            },
            end: SelectionPoint {
                col: selection_end,
                boundary: true,
                ..point.clone()
            },
        })
    }

    fn get_line_selection(&self, point: &SelectionPoint) -> SelectionRange {
        let line_width = crate::utils::visible_width(&self.get_selection_source_line(point));
        SelectionRange {
            start: SelectionPoint {
                col: 0,
                boundary: false,
                ..point.clone()
            },
            end: SelectionPoint {
                col: line_width,
                boundary: true,
                ..point.clone()
            },
        }
    }

    fn update_selection_focus(&self, point: &SelectionPoint) {
        let (granularity, initial_opt) = {
            let mut state = self.state();
            if state.selection_granularity == SelectionGranularity::Character || state.selection_initial_range.is_none()
            {
                state.selection_focus = Some(point.clone());
                return;
            }
            (state.selection_granularity, state.selection_initial_range.clone())
        };
        let Some(initial) = initial_opt else {
            return;
        };
        let range = match granularity {
            SelectionGranularity::Word => self.get_word_selection(point),
            SelectionGranularity::Line => Some(self.get_line_selection(point)),
            SelectionGranularity::Character => None,
        };
        let Some(range) = range else {
            return;
        };
        let target_before_initial = range.start.row < initial.start.row
            || (range.start.row == initial.start.row && range.start.col < initial.start.col);
        let mut state = self.state();
        if target_before_initial {
            state.selection_anchor = Some(initial.end);
            state.selection_focus = Some(range.start);
        } else {
            state.selection_anchor = Some(initial.start);
            state.selection_focus = Some(range.end);
        }
    }

    fn get_click_count(&self, point: &SelectionPoint, word: Option<&SelectionRange>) -> i64 {
        let now = pi_js::time::now_ms();
        let mut state = self.state();
        let count = match (word, state.last_click.as_ref()) {
            (Some(w), Some(previous))
                if now - previous.timestamp <= DOUBLE_CLICK_INTERVAL_MS
                    && previous.row == point.row
                    && previous.scroll_view == point.scroll_view
                    && previous.word_start == w.start.col
                    && previous.word_end == w.end.col =>
            {
                (previous.count % 3) + 1
            }
            _ => 1,
        };
        state.last_click = word.map(|w| ClickTarget {
            timestamp: now,
            count,
            row: point.row,
            scroll_view: point.scroll_view.clone(),
            word_start: w.start.col,
            word_end: w.end.col,
        });
        count
    }

    fn update_selection_auto_scroll(&self, event: &SgrMouseEvent) {
        let (scroll_view, layout) = {
            let state = self.state();
            (
                state.selection_anchor.as_ref().and_then(|a| a.scroll_view.clone()),
                state.current_layout.clone(),
            )
        };
        let (Some(scroll_view), Some(layout)) = (scroll_view, layout) else {
            self.stop_selection_auto_scroll();
            return;
        };
        let Some(r#box) = crate::layout::get_scroll_view_box(&layout, &scroll_view) else {
            self.stop_selection_auto_scroll();
            return;
        };
        let rect = r#box.rect();
        let clip = r#box.clip();
        if rect.height <= 0 || clip.height <= 0 {
            self.stop_selection_auto_scroll();
            return;
        }
        let rows = self.terminal().rows();
        let visible_top = 0.max(rect.y).max(clip.y);
        let visible_bottom = (rows - 1)
            .min(rect.y + rect.height - 1)
            .min(clip.y + clip.height - 1);
        let direction = if event.y <= visible_top {
            -1
        } else if event.y >= visible_bottom {
            1
        } else {
            0
        };
        {
            let mut state = self.state();
            state.selection_drag_pointer = Some(PointerPoint { x: event.x, y: event.y });
            state.selection_auto_scroll_direction = direction;
        }
        if direction == 0 {
            self.stop_selection_auto_scroll();
            return;
        }
        if self.state().selection_auto_scroll_timer.is_some() {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let timer = pi_js::time::set_interval(50, move || {
            if let Some(inner) = weak.upgrade() {
                TuiAltScreen { inner }.auto_scroll_selection();
            }
        });
        timer.unref();
        let mut state = self.state();
        if state.selection_auto_scroll_direction != 0 && state.selection_auto_scroll_timer.is_none() {
            state.selection_auto_scroll_timer = Some(timer);
        } else {
            timer.clear();
        }
    }

    fn auto_scroll_selection(&self) {
        let (scroll_view, pointer, direction) = {
            let state = self.state();
            (
                state.selection_anchor.as_ref().and_then(|a| a.scroll_view.clone()),
                state.selection_drag_pointer,
                state.selection_auto_scroll_direction,
            )
        };
        let (Some(scroll_view), Some(pointer)) = (scroll_view, pointer) else {
            self.stop_selection_auto_scroll();
            return;
        };
        if direction == 0 {
            self.stop_selection_auto_scroll();
            return;
        }
        let remaining = scroll_view.scroll_by(direction as f64);
        if remaining == direction {
            self.stop_selection_auto_scroll();
            return;
        }
        if let Some(point) = self.get_scroll_selection_point(&scroll_view, pointer.x, pointer.y) {
            self.update_selection_focus(&point);
        }
        self.request_render(None);
    }

    fn stop_selection_auto_scroll(&self) {
        let timer = {
            let mut state = self.state();
            state.selection_auto_scroll_direction = 0;
            state.selection_drag_pointer = None;
            state.selection_auto_scroll_timer.take()
        };
        if let Some(t) = timer {
            t.clear();
        }
    }

    fn handle_selection_mouse_event(&self, event: &SgrMouseEvent) {
        let button = event.button & 3;
        if button != 0 && !(event.release && button == 3) {
            return;
        }
        let anchor_scroll_view = self
            .state()
            .selection_anchor
            .as_ref()
            .and_then(|a| a.scroll_view.clone());
        let point = self.get_selection_point(event, anchor_scroll_view.as_ref());
        if event.release {
            let (press_active, anchor, dragged, pressed_url) = {
                let mut state = self.state();
                if !state.selection_press_active {
                    return;
                }
                state.selection_press_active = false;
                (
                    true,
                    state.selection_anchor.clone(),
                    state.selection_dragged,
                    state.pressed_url.take(),
                )
            };
            let _ = press_active;
            self.stop_selection_auto_scroll();
            let Some(anchor) = anchor else {
                return;
            };
            self.update_selection_focus(&point);
            let is_click =
                !dragged && anchor.scroll_view == point.scroll_view && anchor.row == point.row && anchor.col == point.col;
            let clicked_url = if is_click { pressed_url } else { None };
            if let (Some(url), Some(open_url)) = (clicked_url, self.inner.open_url.as_ref()) {
                {
                    let mut state = self.state();
                    state.selection_anchor = None;
                    state.selection_focus = None;
                }
                open_url(&url);
                self.request_render(None);
                return;
            }
            if is_click {
                let click_count = self.state().last_click.as_ref().map(|c| c.count).unwrap_or(1);
                let click_event = self.create_mouse_event(
                    crate::tui::TuiMouseEventType::Click,
                    event.button,
                    event.x,
                    event.y,
                    Some(MouseEventExtra {
                        wheel_delta: None,
                        click_count: Some(click_count),
                    }),
                );
                let overlay = self.base().dispatch_mouse_to_overlay(&click_event);
                let result = overlay.result.or_else(|| {
                    if overlay.hit {
                        None
                    } else {
                        self.dispatch_mouse_to_layout(&click_event)
                    }
                });
                if let Some(res) = result {
                    let render = self.apply_mouse_dispatch_result(&click_event, &res);
                    self.clear_text_selection();
                    if render {
                        self.request_render(None);
                    }
                    return;
                }
            }
            if self.state().copy_on_select {
                if self.inner.copy_selection.is_some() {
                    if let Ok(rt) = tokio::runtime::Handle::try_current() {
                        let this = self.clone();
                        rt.spawn(async move {
                            let _ = this.copy_selection_to_clipboard().await;
                        });
                    }
                } else if let Some(text) = self.get_active_selection_text() {
                    self.copy_text_osc52(&text);
                }
            }
            self.request_render(None);
            return;
        }
        if (event.button & 32) != 0 {
            {
                let mut state = self.state();
                if !state.selection_press_active || state.selection_anchor.is_none() {
                    return;
                }
                state.selection_dragged = true;
                state.last_click = None;
                state.pressed_url = None;
            }
            self.update_selection_focus(&point);
            self.update_selection_auto_scroll(event);
            self.request_render(None);
            return;
        }

        self.stop_selection_auto_scroll();
        let scroll_view = if !self.has_overlay() {
            self.state()
                .current_layout
                .as_ref()
                .and_then(|l| crate::layout::get_scroll_views_at(l, event.x, event.y).into_iter().next())
        } else {
            None
        };
        let anchor = self.get_selection_point(event, scroll_view.as_ref());
        let word = self.get_word_selection(&anchor);
        let click_count = self.get_click_count(&anchor, word.as_ref());
        let range = if click_count == 2 {
            word
        } else if click_count == 3 {
            Some(self.get_line_selection(&anchor))
        } else {
            None
        };
        let pressed_url = if range.is_some() {
            None
        } else {
            let terminal = self.terminal();
            let row_idx = 0.max((terminal.rows() - 1).min(event.y)) as usize;
            let col_idx = 0.max((terminal.columns() - 1).min(event.x));
            let line = self
                .state()
                .previous_screen
                .get(row_idx)
                .cloned()
                .unwrap_or_default();
            crate::utils::get_osc8_link_at_column(&line, col_idx)
        };
        {
            let mut state = self.state();
            state.selection_press_active = true;
            state.selection_granularity = if range.is_some() {
                if click_count == 2 {
                    SelectionGranularity::Word
                } else {
                    SelectionGranularity::Line
                }
            } else {
                SelectionGranularity::Character
            };
            state.selection_initial_range = range.clone();
            state.selection_anchor = Some(range.as_ref().map(|r| r.start.clone()).unwrap_or_else(|| anchor.clone()));
            state.selection_focus = Some(range.map(|r| r.end).unwrap_or(anchor));
            state.selection_dragged = false;
            state.pressed_url = pressed_url;
        }
        self.request_render(None);
    }

    fn get_selection_bounds(&self) -> Option<SelectionRange> {
        let state = self.state();
        let anchor = state.selection_anchor.as_ref()?;
        let focus = state.selection_focus.as_ref()?;
        if anchor.scroll_view != focus.scroll_view {
            return None;
        }
        if anchor.row == focus.row && anchor.col == focus.col {
            return None;
        }
        let anchor_before_focus =
            anchor.row < focus.row || (anchor.row == focus.row && anchor.col < focus.col);
        if anchor_before_focus {
            Some(SelectionRange {
                start: anchor.clone(),
                end: focus.clone(),
            })
        } else {
            Some(SelectionRange {
                start: focus.clone(),
                end: anchor.clone(),
            })
        }
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
        let line_width = crate::utils::visible_width(line);
        let min_col = min_column.unwrap_or(0);
        let max_col = max_column.unwrap_or(line_width);
        let mut start = 0.max(min_col);
        let mut end = line_width.min(max_col);
        if row == selection.start.row {
            start = crate::utils::get_grapheme_cell_range(line, selection.start.col)
                .map(|r| r.start)
                .unwrap_or_else(|| selection.start.col.min(line_width));
        }
        if row == selection.end.row {
            end = if selection.end.boundary {
                selection.end.col.min(line_width)
            } else {
                crate::utils::get_grapheme_cell_range(line, selection.end.col)
                    .map(|r| r.end)
                    .unwrap_or_else(|| (selection.end.col + 1).min(line_width))
            };
        }
        SelectionColumns {
            start: min_col.max(start),
            end: max_col.min(end),
        }
    }

    fn get_active_selection_text(&self) -> Option<String> {
        let selection = self.get_selection_bounds()?;
        let source_lines = {
            let state = self.state();
            if let Some(ref sv) = selection.start.scroll_view {
                let layout = state.current_layout.as_ref()?;
                let r#box = crate::layout::get_scroll_view_box(layout, sv)?;
                r#box.scroll_content_lines()?
            } else {
                state.previous_screen.clone()
            }
        };
        let mut lines = Vec::new();
        for row in selection.start.row..=selection.end.row {
            let line = usize::try_from(row)
                .ok()
                .and_then(|r| source_lines.get(r))
                .map(String::as_str)
                .unwrap_or("");
            let columns = self.get_selection_columns(line, row, &selection, None, None);
            let len = 0.max(columns.end - columns.start);
            let sliced = crate::utils::slice_by_column(line, columns.start, len, Some(true));
            let stripped = crate::utils::strip_terminal_sequences(&sliced);
            lines.push(stripped.trim_end().to_string());
        }
        let text = lines.join("\n");
        if text.is_empty() { None } else { Some(text) }
    }

    async fn copy_selection_to_clipboard(&self) -> bool {
        let Some(text) = self.get_active_selection_text() else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        self.copy_text_to_clipboard(&text).await
    }

    fn copy_text_osc52(&self, text: &str) {
        self.terminal()
            .write(&format!("\x1b]52;c;{}\x07", pi_js::b64::encode(text.as_bytes())));
        self.flash("Copied!", None);
    }

    // Prefer an injected clipboard implementation (native clipboard + platform tools with a
    // verified success path) when the host app provides one. A bare OSC 52 write can show
    // "Copied!" while leaving the system clipboard untouched (e.g. macOS Terminal.app, tmux
    // without OSC 52 clipboard passthrough), so only report success when it actually copies.
    async fn copy_text_to_clipboard(&self, text: &str) -> bool {
        if let Some(ref copy_fn) = self.inner.copy_selection {
            let result = copy_fn(text.to_string()).await;
            let (ok, message, duration) = match result {
                CopySelectionResult::Bool(true) => (true, "Copied!".to_string(), None),
                CopySelectionResult::Bool(false) => {
                    (false, "Copy failed".to_string(), Some(COPY_ERROR_FLASH_DURATION_MS))
                }
                CopySelectionResult::Message(msg) => (false, msg, Some(COPY_ERROR_FLASH_DURATION_MS)),
            };
            self.flash(&message, duration);
            return ok;
        }
        self.copy_text_osc52(text);
        true
    }

    fn apply_search_text_highlight(&self, text: &str, current: bool) -> String {
        let style = if current {
            &self.inner.search_current_match_style
        } else {
            &self.inner.search_match_style
        };
        let mut result = String::new();
        let mut plain_start = 0usize;
        let mut byte_idx = 0usize;
        let bytes = text.as_bytes();
        while byte_idx < bytes.len() {
            if bytes[byte_idx] == b'\x1b' {
                if let Some(ansi) = crate::utils::extract_ansi_code(&text[byte_idx..], 0) {
                    if byte_idx > plain_start {
                        result.push_str(&style(&text[plain_start..byte_idx]));
                    }
                    result.push_str(&ansi.code);
                    byte_idx += ansi.code.len();
                    plain_start = byte_idx;
                    continue;
                }
            }
            if let Some(ch) = text[byte_idx..].chars().next() {
                byte_idx += ch.len_utf8();
            } else {
                break;
            }
        }
        if plain_start < text.len() {
            result.push_str(&style(&text[plain_start..]));
        }
        result
    }

    fn apply_search_highlights(&self, screen: &[String], layout: &LayoutFrame) -> Vec<String> {
        let (matches, selected_index) = {
            let state = self.state();
            let Some(ref search) = state.active_search else {
                return screen.to_vec();
            };
            if search.selected_index < 0 || search.matches.is_empty() {
                return screen.to_vec();
            }
            (search.matches.clone(), search.selected_index)
        };
        let scroll_view = layout
            .primary_scroll_view
            .clone()
            .unwrap_or_else(|| self.inner.implicit_scroll_view.clone());
        let Some(r#box) = crate::layout::get_scroll_view_box(layout, &scroll_view) else {
            return screen.to_vec();
        };

        let mut ranges_by_row: IndexMap<i64, Vec<SearchHighlightRange>> = IndexMap::new();
        let scrollbar_column = crate::layout::get_scrollbar_geometry(&r#box, None).map(|g| g.column);
        let rect = r#box.rect();
        let clip = r#box.clip();
        let min_row = 0.max(rect.y).max(clip.y);
        let max_row = (screen.len() as i64)
            .min(rect.y + rect.height)
            .min(clip.y + clip.height);
        let min_column = 0.max(rect.x).max(clip.x);
        let cols = self.terminal().columns();
        let mut max_column = cols.min(rect.x + rect.width).min(clip.x + clip.width);
        if let Some(sb_col) = scrollbar_column {
            max_column = max_column.min(sb_col);
        }
        let scroll_top = scroll_view.scroll_top();
        let min_content_row = scroll_top + min_row - rect.y;
        let max_content_row = scroll_top + max_row - rect.y - 1;

        let mut low = 0usize;
        let mut high = matches.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let last_row = matches[middle].segments.last().map(|s| s.row).unwrap_or(-1);
            if last_row < min_content_row {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        for (match_index, r#match) in matches.iter().enumerate().skip(low) {
            let first_row = r#match.segments.first().map(|s| s.row).unwrap_or(0);
            if first_row > max_content_row {
                break;
            }
            for segment in &r#match.segments {
                let row = rect.y + segment.row - scroll_top;
                if row < min_row || row >= max_row {
                    continue;
                }
                let start_col = min_column.max(rect.x + segment.start_col);
                let end_col = max_column.min(rect.x + segment.end_col);
                if end_col <= start_col {
                    continue;
                }
                ranges_by_row.entry(row).or_default().push(SearchHighlightRange {
                    start_col,
                    end_col,
                    current: match_index as i64 == selected_index,
                });
            }
        }

        let mut result = screen.to_vec();
        for (row, mut ranges) in ranges_by_row {
            let Ok(row_idx) = usize::try_from(row) else {
                continue;
            };
            if row_idx >= result.len() {
                continue;
            }
            if crate::terminal_image::is_image_line(&result[row_idx]) {
                continue;
            }
            let mut line = result[row_idx].clone();
            let line_width = crate::utils::visible_width(&line);
            ranges.sort_by(|a, b| b.start_col.cmp(&a.start_col));
            for range in ranges {
                let start_col = range.start_col.min(line_width);
                let end_col = range.end_col.min(line_width);
                if end_col <= start_col {
                    continue;
                }
                let before = crate::utils::slice_by_column(&line, 0, start_col, Some(true));
                let highlighted = crate::utils::slice_by_column(&line, start_col, end_col - start_col, Some(true));
                let after = crate::utils::slice_by_column(&line, end_col, 0.max(line_width - end_col), Some(true));
                line = format!(
                    "{before}{}{after}",
                    self.apply_search_text_highlight(&highlighted, range.current)
                );
            }
            result[row_idx] = line;
        }
        result
    }

    fn apply_selection_highlight(&self, text: &str) -> String {
        let mut result = String::from("\x1b[7m");
        let mut byte_idx = 0usize;
        let bytes = text.as_bytes();
        while byte_idx < bytes.len() {
            if bytes[byte_idx] == b'\x1b' {
                if let Some(ansi) = crate::utils::extract_ansi_code(&text[byte_idx..], 0) {
                    result.push_str(&ansi.code);
                    if ansi.code.ends_with('m') {
                        result.push_str("\x1b[7m");
                    }
                    byte_idx += ansi.code.len();
                    continue;
                }
            }
            if let Some(ch) = text[byte_idx..].chars().next() {
                result.push(ch);
                byte_idx += ch.len_utf8();
            } else {
                break;
            }
        }
        result.push_str("\x1b[27m");
        result
    }

    /// `layout` `None` is `this.currentLayout` (also absent when no frame has been rendered).
    fn apply_selection(&self, screen: &[String], layout: Option<&LayoutFrame>) -> Vec<String> {
        let Some(selection) = self.get_selection_bounds() else {
            return screen.to_vec();
        };
        let owned_layout = if layout.is_none() {
            self.state().current_layout.clone()
        } else {
            None
        };
        let layout_ref = layout.or(owned_layout.as_ref());

        let mut screen_selection = selection.clone();
        let mut min_row = 0i64;
        let mut max_row = screen.len() as i64 - 1;
        let mut min_column = 0i64;
        let mut max_column = self.terminal().columns();

        if let Some(ref sv) = selection.start.scroll_view {
            let Some(l) = layout_ref else {
                return screen.to_vec();
            };
            let Some(r#box) = crate::layout::get_scroll_view_box(l, sv) else {
                return screen.to_vec();
            };
            let rect = r#box.rect();
            let clip = r#box.clip();
            min_row = 0.max(rect.y).max(clip.y);
            max_row = (screen.len() as i64 - 1)
                .min(rect.y + rect.height - 1)
                .min(clip.y + clip.height - 1);
            min_column = 0.max(rect.x).max(clip.x);
            max_column = max_column
                .min(rect.x + rect.width)
                .min(clip.x + clip.width);
            let scroll_top = sv.scroll_top();
            screen_selection = SelectionRange {
                start: SelectionPoint {
                    row: rect.y + selection.start.row - scroll_top,
                    col: rect.x + selection.start.col,
                    ..selection.start.clone()
                },
                end: SelectionPoint {
                    row: rect.y + selection.end.row - scroll_top,
                    col: rect.x + selection.end.col,
                    ..selection.end
                },
            };
        }

        screen
            .iter()
            .enumerate()
            .map(|(row_usize, line)| {
                let row = row_usize as i64;
                if row < min_row
                    || row > max_row
                    || row < screen_selection.start.row
                    || row > screen_selection.end.row
                    || crate::terminal_image::is_image_line(line)
                {
                    return line.clone();
                }
                let line_width = crate::utils::visible_width(line);
                let columns =
                    self.get_selection_columns(line, row, &screen_selection, Some(min_column), Some(max_column));
                if columns.end <= columns.start {
                    return line.clone();
                }
                let before = crate::utils::slice_by_column(line, 0, columns.start, Some(true));
                let selected =
                    crate::utils::slice_by_column(line, columns.start, columns.end - columns.start, Some(true));
                let after = crate::utils::slice_by_column(line, columns.end, 0.max(line_width - columns.end), Some(true));
                format!("{before}{}{after}", self.apply_selection_highlight(&selected))
            })
            .collect()
    }

    fn is_mouse_sequence(&self, data: &str) -> bool {
        SGR_MOUSE_REGEX.is_match(data) || (data.starts_with("\x1b[M") && data.chars().count() == 6)
    }

    fn composite_scroll_to_end_indicator(&self, screen: &[String], layout: &LayoutFrame, width: i64) -> Vec<String> {
        self.state().scroll_to_end_indicator_rect = None;
        let scroll_view = layout
            .primary_scroll_view
            .clone()
            .unwrap_or_else(|| self.inner.implicit_scroll_view.clone());
        let Some(ref indicator) = self.inner.scroll_to_end_indicator else {
            return screen.to_vec();
        };
        if !scroll_view.follow_end() || scroll_view.is_following_end() {
            return screen.to_vec();
        }
        let Some(r#box) = crate::layout::get_scroll_view_box(layout, &scroll_view) else {
            return screen.to_vec();
        };
        let clip = r#box.clip();
        if clip.width <= 0 || clip.height <= 0 {
            return screen.to_vec();
        }
        let row = clip.y + clip.height - 1;
        if row < 0
            || (row as usize) >= screen.len()
            || crate::terminal_image::is_image_line(&screen[row as usize])
        {
            return screen.to_vec();
        }
        let scrollbar_column = crate::layout::get_scrollbar_geometry(&r#box, None).map(|g| g.column);
        let label = crate::utils::truncate_to_width(&indicator(), clip.width, Some(""), None);
        let label_width = crate::utils::visible_width(&label);
        let column = clip.x + (clip.width - label_width).div_euclid(2);
        let right_edge = scrollbar_column.unwrap_or(clip.x + clip.width);
        let available_width = 0.max(right_edge - column);
        let text = crate::utils::truncate_to_width(&label, available_width, Some(""), None);
        let text_width = crate::utils::visible_width(&text);
        if text_width == 0 {
            return screen.to_vec();
        }
        let mut result = screen.to_vec();
        let row_idx = row as usize;
        result[row_idx] = crate::tui::composite_tui_line(&result[row_idx], &text, column, text_width, width);
        self.state().scroll_to_end_indicator_rect = Some(ScrollToEndIndicatorRect {
            row,
            column,
            width: text_width,
        });
        result
    }

    fn composite_flashes(&self, screen: &[String], width: i64, height: i64) -> Vec<String> {
        let all_flash_lines = self
            .inner
            .flashes
            .render(usize::try_from(width.max(0)).unwrap_or(0));
        let h_usize = usize::try_from(height.max(0)).unwrap_or(0);
        let start = all_flash_lines.len().saturating_sub(h_usize);
        let flash_lines = &all_flash_lines[start..];
        if flash_lines.is_empty() {
            return screen.to_vec();
        }
        let mut result = screen.to_vec();
        while result.len() < h_usize {
            result.push(String::new());
        }
        for (row, line) in flash_lines.iter().enumerate() {
            let flash_width = crate::utils::visible_width(line);
            if flash_width == 0 {
                continue;
            }
            result[row] = crate::tui::composite_tui_line(&result[row], line, width - flash_width, flash_width, width);
        }
        result
    }

    // WezTerm erases intersecting Kitty image cells when a later EL clears a covered row.
    // Only separate clearing from drawing for WezTerm frames that place images; preserve the
    // existing interleaved output for text-only frames and every other terminal.
    /// PORT: `protected`.
    pub(crate) fn do_render(&self) {
        let (stopped, alt_screen_active, layout_root) = {
            let state = self.state();
            (self.base().stopped(), state.alt_screen_active, state.layout_root.clone())
        };
        if stopped || !alt_screen_active {
            return;
        }
        let terminal = self.terminal();
        let width = 1.max(terminal.columns());
        let height = 1.max(terminal.rows());
        let root: ComponentHandle = layout_root.unwrap_or_else(|| Arc::new(self.inner.implicit_scroll_view.clone()));

        let weak = Arc::downgrade(&self.inner);
        let req_render: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.base.request_render(None);
            }
        });
        let mut next_layout =
            crate::layout::render_layout_frame(Arc::clone(&root), width, height, Arc::clone(&req_render));
        if self.refresh_search(&next_layout) {
            next_layout = crate::layout::render_layout_frame(root, width, height, req_render);
        }

        let mut screen: Vec<String> = next_layout
            .lines
            .iter()
            .map(|line| OSC133_ZONE_PREFIX.replace(line, "").into_owned())
            .collect();
        screen = self.apply_search_highlights(&screen, &next_layout);
        screen = self.composite_scroll_to_end_indicator(&screen, &next_layout, width);
        screen = self.base().composite_overlays(screen, width, height);
        let h_usize = usize::try_from(height).unwrap_or(0);
        if screen.len() > h_usize {
            let keep_from = screen.len() - h_usize;
            screen.drain(..keep_from);
        }
        screen = self.apply_selection(&screen, Some(&next_layout));
        screen = self.composite_flashes(&screen, width, height);

        let cursor_pos = self.base().extract_cursor_position(&mut screen, height);
        screen = self
            .base()
            .apply_line_resets(screen)
            .into_iter()
            .map(|line| {
                if crate::terminal_image::is_image_line(&line) || crate::utils::visible_width(&line) <= width {
                    line
                } else {
                    crate::utils::slice_by_column(&line, 0, width, Some(true))
                }
            })
            .collect();

        let (prev_screen, prev_w, prev_h, image_protocol, had_uploaded_kitty_images) = {
            let state = self.state();
            (
                state.previous_screen.clone(),
                state.previous_screen_width,
                state.previous_screen_height,
                state.image_protocol,
                !state.uploaded_kitty_images.is_empty(),
            )
        };

        let full_redraw = prev_screen.is_empty() || prev_w != width || prev_h != height;
        let images_need_redraw = screen.iter().enumerate().any(|(row, line)| {
            let prev = prev_screen.get(row).map(String::as_str).unwrap_or("");
            line.as_str() != prev
                && (crate::terminal_image::is_image_line(line) || crate::terminal_image::is_image_line(prev))
        });
        let redraw_images = full_redraw || images_need_redraw;
        let prepared_kitty_screen = if redraw_images && image_protocol == Some(ImageProtocol::Kitty) {
            self.prepare_kitty_screen(&screen)
        } else {
            PreparedKittyScreen {
                lines: screen.clone(),
                evicted_image_deletion: String::new(),
            }
        };

        let mut buffer = String::from(BEGIN_SYNCHRONIZED_OUTPUT);
        if full_redraw {
            self.base().set_full_redraw_count(self.base().full_redraw_count() + 1);
            let clear_images = if image_protocol == Some(ImageProtocol::Kitty) && had_uploaded_kitty_images {
                crate::terminal_image::delete_all_kitty_placements()
            } else {
                self.delete_kitty_images()
            };
            buffer.push_str(&clear_images);
            buffer.push_str("\x1b[2J");
        } else if images_need_redraw {
            if image_protocol == Some(ImageProtocol::Iterm2) {
                buffer.push_str("\x1b[2J");
            } else if image_protocol == Some(ImageProtocol::Kitty) {
                buffer.push_str(&crate::terminal_image::delete_all_kitty_placements());
            }
        }
        buffer.push_str(&prepared_kitty_screen.evicted_image_deletion);

        let clear_rows_before_kitty_images = redraw_images
            && image_protocol == Some(ImageProtocol::Kitty)
            && screen.iter().any(|l| crate::terminal_image::is_image_line(l))
            && (pi_js::env::var("WEZTERM_PANE").is_some_and(|v| !v.is_empty())
                || pi_js::env::var("TERM_PROGRAM")
                    .map(|v| v.to_ascii_lowercase())
                    .as_deref()
                    == Some("wezterm"));
        if clear_rows_before_kitty_images {
            for row in 0..h_usize {
                if !full_redraw && !images_need_redraw && screen.get(row) == prev_screen.get(row) {
                    continue;
                }
                buffer.push_str(&format!("\x1b[{};1H\x1b[2K", row + 1));
            }
        }

        for row in 0..h_usize {
            if !full_redraw && !images_need_redraw && screen.get(row) == prev_screen.get(row) {
                continue;
            }
            let line = prepared_kitty_screen
                .lines
                .get(row)
                .map(String::as_str)
                .unwrap_or("");
            if clear_rows_before_kitty_images {
                buffer.push_str(&format!("\x1b[{};1H{line}", row + 1));
            } else {
                buffer.push_str(&format!("\x1b[{};1H\x1b[2K{line}", row + 1));
            }
        }

        if let Some(pos) = cursor_pos {
            buffer.push_str(&format!("\x1b[{};{}H", pos.row + 1, width.min(pos.col) + 1));
            buffer.push_str(if self.get_show_hardware_cursor() {
                "\x1b[?25h"
            } else {
                "\x1b[?25l"
            });
        } else {
            buffer.push_str("\x1b[?25l");
        }
        buffer.push_str(END_SYNCHRONIZED_OUTPUT);
        terminal.write(&buffer);

        let mut state = self.state();
        state.previous_screen = screen;
        state.previous_screen_width = width;
        state.previous_screen_height = height;
        state.current_layout = Some(next_layout);
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

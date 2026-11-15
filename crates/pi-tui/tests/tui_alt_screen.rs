//! Port of packages/tui/test/tui-alt-screen.test.ts

#![allow(clippy::too_many_lines)]

mod support;

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_js::b64;
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use pi_tui::alt_screen_search::{
    AltScreenSearchComponent, AltScreenSearchIndex, AltScreenSearchMatch, AltScreenSearchSegment,
    find_alt_screen_search_matches,
};
use pi_tui::components::h_stack::HStack;
use pi_tui::components::image::{Image, ImageTheme};
use pi_tui::components::mouse_region::MouseRegion;
use pi_tui::components::scroll_view::{ScrollView, ScrollViewFollow, ScrollViewOptions, ScrollViewScrollbar};
use pi_tui::components::select_list::{SelectItem, SelectList, SelectListTheme};
use pi_tui::components::stack::{StackChild, StackEntry, StackOptions};
use pi_tui::components::text::Text;
use pi_tui::components::v_stack::VStack;
use pi_tui::keybindings::{KeyIdOrList, KeybindingsManager, TUI_KEYBINDINGS, get_keybindings, set_keybindings};
use pi_tui::layout_node::StackBasis;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::terminal_image::{
    EncodeKittyOptions, ImageDimensions, ImageProtocol, KittyImageMetadata, TerminalCapabilities, encode_kitty,
    hyperlink, register_kitty_image_metadata, reset_capabilities_cache, set_capabilities,
};
use pi_tui::tui::{Component, Focusable, OverlayOptions, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType};
use pi_tui::tui_alt_screen::{CopySelectionResult, TuiAltScreen, TuiAltScreenOptions};
use pi_tui::utils::{strip_terminal_sequences, visible_width};
use pi_tui::wheel_scroll::WheelScrollLines;
use regex::Regex;
use serial_test::serial;

use support::virtual_terminal::VirtualTerminal;

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";

fn line_items(count: usize) -> String {
    (1..=count)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn trimmed_viewport(terminal: &VirtualTerminal) -> Vec<String> {
    terminal
        .get_viewport()
        .into_iter()
        .map(|line| line.trim_end().to_string())
        .collect()
}

fn cell_col_of(haystack: &str, needle: &str) -> Option<i64> {
    haystack
        .find(needle)
        .map(|byte_idx| visible_width(&haystack[..byte_idx]))
}

fn last_cell_col_of(haystack: &str, needle: &str) -> Option<i64> {
    haystack
        .rfind(needle)
        .map(|byte_idx| visible_width(&haystack[..byte_idx]))
}

fn default_tui_options() -> TuiAltScreenOptions {
    TuiAltScreenOptions {
        wheel_scroll_lines: None,
        mouse: None,
        search_match_style: None,
        search_current_match_style: None,
        search_navigation_button_style: None,
        scroll_to_end_indicator: None,
        open_url: None,
        on_right_click_paste: None,
        copy_on_select: None,
        copy_selection: None,
    }
}

fn identity_image_theme() -> ImageTheme {
    ImageTheme {
        fallback_color: Arc::new(|value: &str| value.to_string()),
    }
}

fn identity_select_list_theme() -> SelectListTheme {
    SelectListTheme {
        selected_prefix: Arc::new(|text| text.to_string()),
        selected_text: Arc::new(|text| text.to_string()),
        description: Arc::new(|text| text.to_string()),
        scroll_info: Arc::new(|text| text.to_string()),
        no_match: Arc::new(|text| text.to_string()),
    }
}

struct CapsGuard;

impl CapsGuard {
    fn set(caps: TerminalCapabilities) -> Self {
        set_capabilities(caps);
        Self
    }

    fn kitty() -> Self {
        Self::set(TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        })
    }

    fn iterm2() -> Self {
        Self::set(TerminalCapabilities {
            images: Some(ImageProtocol::Iterm2),
            true_color: true,
            hyperlinks: true,
        })
    }
}

impl Drop for CapsGuard {
    fn drop(&mut self) {
        reset_capabilities_cache();
    }
}

struct KeybindingsGuard {
    original: KeybindingsManager,
}

impl KeybindingsGuard {
    fn new() -> Self {
        Self {
            original: get_keybindings(),
        }
    }
}

impl Drop for KeybindingsGuard {
    fn drop(&mut self) {
        set_keybindings(self.original.clone());
    }
}

struct StopGuard(Option<TuiAltScreen>);

impl StopGuard {
    fn new(tui: TuiAltScreen) -> Self {
        Self(Some(tui))
    }

    fn stop_now(&mut self) {
        if let Some(tui) = self.0.take() {
            tui.stop(None);
        }
    }
}

impl Drop for StopGuard {
    fn drop(&mut self) {
        self.stop_now();
    }
}

struct InputOverlay {
    focused: AtomicBool,
    inputs: Mutex<Vec<String>>,
}

impl InputOverlay {
    fn new() -> Self {
        Self {
            focused: AtomicBool::new(false),
            inputs: Mutex::new(Vec::new()),
        }
    }

    fn inputs(&self) -> Vec<String> {
        self.inputs.lock().expect("inputs lock").clone()
    }
}

impl Focusable for InputOverlay {
    fn focused(&self) -> bool {
        self.focused.load(Ordering::SeqCst)
    }

    fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::SeqCst);
    }
}

impl Component for InputOverlay {
    fn render(&self, _width: usize) -> Vec<String> {
        vec!["overlay".to_string()]
    }

    fn handle_input(&self, data: &str) {
        self.inputs.lock().expect("inputs lock").push(data.to_string());
    }

    fn invalidate(&self) {}

    fn has_handle_input(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }
}

struct MockEditorComponent {
    focused: AtomicBool,
    inputs: Mutex<Vec<String>>,
}

impl MockEditorComponent {
    fn new() -> Self {
        Self {
            focused: AtomicBool::new(false),
            inputs: Mutex::new(Vec::new()),
        }
    }

    fn inputs(&self) -> Vec<String> {
        self.inputs.lock().expect("inputs lock").clone()
    }
}

impl Focusable for MockEditorComponent {
    fn focused(&self) -> bool {
        self.focused.load(Ordering::SeqCst)
    }

    fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::SeqCst);
    }
}

impl Component for MockEditorComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        vec!["editor".to_string()]
    }

    fn handle_input(&self, data: &str) {
        self.inputs.lock().expect("inputs lock").push(data.to_string());
    }

    fn invalidate(&self) {}

    fn has_handle_input(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }
}

struct InvalidateSpyComponent {
    inner: Text,
    invalidated: AtomicBool,
}

impl InvalidateSpyComponent {
    fn new(inner: Text) -> Self {
        Self {
            inner,
            invalidated: AtomicBool::new(false),
        }
    }

    fn was_invalidated(&self) -> bool {
        self.invalidated.load(Ordering::SeqCst)
    }
}

impl Component for InvalidateSpyComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }

    fn invalidate(&self) {
        self.invalidated.store(true, Ordering::SeqCst);
        self.inner.invalidate();
    }
}

struct RenderClosureComponent {
    render_fn: Box<dyn Fn(usize) -> Vec<String> + Send + Sync>,
}

impl RenderClosureComponent {
    fn new(render_fn: impl Fn(usize) -> Vec<String> + Send + Sync + 'static) -> Self {
        Self {
            render_fn: Box::new(render_fn),
        }
    }
}

impl Component for RenderClosureComponent {
    fn render(&self, width: usize) -> Vec<String> {
        (self.render_fn)(width)
    }

    fn invalidate(&self) {}
}

struct MouseHandlerComponent {
    render_fn: Box<dyn Fn(usize) -> Vec<String> + Send + Sync>,
    mouse_fn: Box<dyn Fn(&TuiMouseEvent) -> Option<TuiMouseEventResult> + Send + Sync>,
}

impl MouseHandlerComponent {
    fn new(
        render_fn: impl Fn(usize) -> Vec<String> + Send + Sync + 'static,
        mouse_fn: impl Fn(&TuiMouseEvent) -> Option<TuiMouseEventResult> + Send + Sync + 'static,
    ) -> Self {
        Self {
            render_fn: Box::new(render_fn),
            mouse_fn: Box::new(mouse_fn),
        }
    }
}

impl Component for MouseHandlerComponent {
    fn render(&self, width: usize) -> Vec<String> {
        (self.render_fn)(width)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        (self.mouse_fn)(event)
    }

    fn invalidate(&self) {}
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecordingEvent {
    Start,
    Write(String),
    Stop,
}

#[derive(Clone)]
struct RecordingTerminal {
    inner: VirtualTerminal,
    events: Arc<Mutex<Vec<RecordingEvent>>>,
}

impl Default for RecordingTerminal {
    fn default() -> Self {
        Self::new(80, 24)
    }
}

impl RecordingTerminal {
    fn new(columns: i64, rows: i64) -> Self {
        Self {
            inner: VirtualTerminal::new(columns, rows),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn send_input(&self, data: &str) {
        self.inner.send_input(data);
    }

    fn get_viewport(&self) -> Vec<String> {
        self.inner.get_viewport()
    }

    async fn wait_for_render(&self) {
        self.inner.wait_for_render().await;
    }

    fn event_count(&self) -> usize {
        self.events.lock().expect("events lock").len()
    }

    fn write_count(&self) -> usize {
        self.events
            .lock()
            .expect("events lock")
            .iter()
            .filter(|e| matches!(e, RecordingEvent::Write(_)))
            .count()
    }

    fn events(&self) -> Vec<RecordingEvent> {
        self.events.lock().expect("events lock").clone()
    }

    fn writes(&self) -> Vec<String> {
        self.events
            .lock()
            .expect("events lock")
            .iter()
            .filter_map(|e| match e {
                RecordingEvent::Write(data) => Some(data.clone()),
                RecordingEvent::Start | RecordingEvent::Stop => None,
            })
            .collect()
    }

    fn writes_since(&self, index: usize) -> String {
        self.events
            .lock()
            .expect("events lock")
            .iter()
            .skip(index)
            .filter_map(|e| match e {
                RecordingEvent::Write(data) => Some(data.as_str()),
                RecordingEvent::Start | RecordingEvent::Stop => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    fn all_writes_joined(&self) -> String {
        self.writes_since(0)
    }
}

#[async_trait]
impl Terminal for RecordingTerminal {
    fn start(&self, on_input: TerminalInputHandler, on_resize: TerminalResizeHandler) {
        self.events.lock().expect("events lock").push(RecordingEvent::Start);
        self.inner.start(on_input, on_resize);
    }

    fn stop(&self) {
        self.events.lock().expect("events lock").push(RecordingEvent::Stop);
        self.inner.stop();
    }

    async fn drain_input(&self, max_ms: Option<i64>, idle_ms: Option<i64>) {
        self.inner.drain_input(max_ms, idle_ms).await;
    }

    fn write(&self, data: &str) {
        self.events
            .lock()
            .expect("events lock")
            .push(RecordingEvent::Write(data.to_string()));
        self.inner.write(data);
    }

    fn columns(&self) -> i64 {
        self.inner.columns()
    }

    fn rows(&self) -> i64 {
        self.inner.rows()
    }

    fn kitty_protocol_active(&self) -> bool {
        self.inner.kitty_protocol_active()
    }

    fn move_by(&self, lines: i64) {
        self.inner.move_by(lines);
    }

    fn hide_cursor(&self) {
        self.inner.hide_cursor();
    }

    fn show_cursor(&self) {
        self.inner.show_cursor();
    }

    fn clear_line(&self) {
        self.inner.clear_line();
    }

    fn clear_from_cursor(&self) {
        self.inner.clear_from_cursor();
    }

    fn clear_screen(&self) {
        self.inner.clear_screen();
    }

    fn set_title(&self, title: &str) {
        self.inner.set_title(title);
    }

    fn set_progress(&self, active: bool) {
        self.inner.set_progress(active);
    }
}

#[tokio::test]
async fn renders_a_terminal_height_viewport_and_preserves_manual_scroll_position() {
    let terminal = VirtualTerminal::new(20, 4);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let text = Text::new(Some(&line_items(10)), Some(0), Some(0), None);
    tui.add_child(Arc::new(text.clone()));
    tui.start();
    terminal.wait_for_render().await;

    assert_eq!(
        trimmed_viewport(&terminal),
        vec!["line 7", "line 8", "line 9", "line 10"]
    );
    assert!(tui.is_following_output());

    terminal.send_input("\x1b[<64;1;1M");
    terminal.wait_for_render().await;
    assert_eq!(
        trimmed_viewport(&terminal),
        vec!["line 6", "line 7", "line 8", "line 9"]
    );
    assert_eq!(tui.viewport_top(), 5);
    assert!(!tui.is_following_output());

    text.set_text(&line_items(12));
    tui.request_render(None);
    terminal.wait_for_render().await;
    assert_eq!(
        trimmed_viewport(&terminal),
        vec!["line 6", "line 7", "line 8", "line 9"]
    );

    tui.stop(None);
}

#[tokio::test]
async fn shows_a_clickable_jump_to_end_indicator_on_the_transcripts_last_row_while_scrolled_up() {
    let terminal = VirtualTerminal::new(30, 6);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            scroll_to_end_indicator: Some(Arc::new(|| "\x1b[7m ↓ Jump to end \x1b[27m".to_string())),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    let transcript = ScrollView::new(
        Arc::new(Text::new(Some(&line_items(8)), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            primary: Some(true),
            ..Default::default()
        }),
    );
    tui.set_layout_root(Some(Arc::new(VStack::new(
        Some(vec![
            StackChild::Entry(StackEntry {
                component: Arc::new(transcript.clone()),
                basis: Some(StackBasis::Fixed(0.0)),
                grow: Some(1),
                shrink: None,
                min_size: Some(1),
                max_size: None,
                visible: None,
            }),
            StackChild::Entry(StackEntry {
                component: Arc::new(Text::new(Some("editor\nfooter"), Some(0), Some(0), None)),
                basis: Some(StackBasis::Auto),
                grow: None,
                shrink: None,
                min_size: Some(1),
                max_size: None,
                visible: None,
            }),
        ]),
        None,
    ))));
    tui.start();
    terminal.wait_for_render().await;
    assert!(!terminal.get_viewport().iter().any(|line| line.contains("Jump to end")));

    terminal.send_input("\x1b[<64;1;1M");
    terminal.wait_for_render().await;
    assert!(!transcript.is_following_end());
    let viewport = terminal.get_viewport();
    assert_eq!(viewport[3], "line 7  ↓ Jump to end         ");
    assert_eq!(viewport[4].trim_end(), "editor");

    // Pressing next to the label starts a selection instead of jumping.
    terminal.send_input("\x1b[<0;2;4M");
    terminal.send_input("\x1b[<0;2;4m");
    terminal.wait_for_render().await;
    assert!(!transcript.is_following_end());

    terminal.send_input("\x1b[<0;15;4M");
    terminal.send_input("\x1b[<0;15;4m");
    terminal.wait_for_render().await;
    assert!(transcript.is_following_end());
    assert_eq!(
        trimmed_viewport(&terminal),
        vec!["line 5", "line 6", "line 7", "line 8", "editor", "footer"]
    );
    tui.stop(None);
}

#[tokio::test]
async fn keeps_the_jump_to_end_indicator_centered_as_the_auto_scrollbar_hides_and_reappears() {
    // Regression test for #9136: auto scrollbar visibility must not move the indicator.
    let terminal = VirtualTerminal::new(80, 6);
    let label = " ↓ Jump to latest message · End ";
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            scroll_to_end_indicator: Some(Arc::new(move || label.to_string())),
            ..default_tui_options()
        }),
    );
    let mut guard = StopGuard::new(tui.clone());
    let transcript = ScrollView::new(
        Arc::new(Text::new(Some(&line_items(20)), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            primary: Some(true),
            scrollbar: Some(ScrollViewScrollbar::Auto),
            scrollbar_hide_delay_ms: Some(0),
            ..Default::default()
        }),
    );
    tui.set_layout_root(Some(Arc::new(transcript.clone())));
    tui.start();
    terminal.wait_for_render().await;

    // Scrolling over the track keeps the scrollbar visible until the pointer leaves.
    terminal.send_input("\x1b[<64;80;1M");
    terminal.wait_for_render().await;
    assert!(transcript.is_scrollbar_visible());
    assert!(!transcript.is_following_end());
    let scroll_top = transcript.scroll_top();
    let visible_column = cell_col_of(&terminal.get_viewport()[5], label);

    // Leaving the track lets the auto-hide timer expire without changing the content.
    terminal.send_input("\x1b[<35;79;1M");
    terminal.wait_for_render().await;
    assert!(!transcript.is_scrollbar_visible());
    assert_eq!(transcript.scroll_top(), scroll_top);
    let hidden_column = cell_col_of(&terminal.get_viewport()[5], label);

    terminal.send_input("\x1b[<35;80;1M");
    terminal.wait_for_render().await;
    assert!(transcript.is_scrollbar_visible());
    assert_eq!(transcript.scroll_top(), scroll_top);
    let revealed_column = cell_col_of(&terminal.get_viewport()[5], label);

    assert_eq!(
        [visible_column, hidden_column, revealed_column],
        [Some(24), Some(24), Some(24)]
    );
    guard.stop_now();
}

#[tokio::test]
async fn leaves_the_scrollbar_visible_and_clickable_when_the_jump_to_end_indicator_spans_the_transcript() {
    // Regression coverage for #9136: centering must not paint or capture clicks over the scrollbar.
    let terminal = VirtualTerminal::new(30, 6);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            scroll_to_end_indicator: Some(Arc::new(|| "↓".repeat(30))),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    let transcript = ScrollView::new(
        Arc::new(Text::new(Some(&line_items(12)), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            primary: Some(true),
            scrollbar: Some(ScrollViewScrollbar::Always),
            ..Default::default()
        }),
    );
    tui.set_layout_root(Some(Arc::new(VStack::new(
        Some(vec![
            StackChild::Entry(StackEntry {
                component: Arc::new(transcript.clone()),
                basis: Some(StackBasis::Fixed(0.0)),
                grow: Some(1),
                shrink: None,
                min_size: Some(1),
                max_size: None,
                visible: None,
            }),
            StackChild::Entry(StackEntry {
                component: Arc::new(Text::new(Some("editor\nfooter"), Some(0), Some(0), None)),
                basis: Some(StackBasis::Auto),
                grow: None,
                shrink: None,
                min_size: Some(1),
                max_size: None,
                visible: None,
            }),
        ]),
        None,
    ))));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<64;1;1M");
    terminal.wait_for_render().await;
    assert!(!transcript.is_following_end());

    // The indicator must not intercept a press on the scrollbar's last column.
    assert_eq!(terminal.get_viewport()[3], format!("{}┃", "↓".repeat(29)));
    terminal.send_input("\x1b[<0;30;4M");
    terminal.send_input("\x1b[<0;30;4m");
    terminal.wait_for_render().await;
    assert!(!transcript.is_following_end());
    tui.stop(None);
}

#[tokio::test]
async fn never_shows_the_jump_to_end_indicator_for_a_primary_scroll_view_without_follow_end() {
    let terminal = VirtualTerminal::new(30, 3);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            scroll_to_end_indicator: Some(Arc::new(|| " ↓ Jump to end ".to_string())),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    let transcript = ScrollView::new(
        Arc::new(Text::new(Some("one\ntwo\nthree\nfour\nfive"), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            primary: Some(true),
            ..Default::default()
        }),
    );
    tui.set_layout_root(Some(Arc::new(transcript.clone())));
    tui.start();
    terminal.wait_for_render().await;

    assert!(!transcript.is_following_end());
    assert!(!terminal.get_viewport().iter().any(|line| line.contains("Jump to end")));
    tui.stop(None);
}

#[tokio::test]
async fn keeps_an_explicit_dock_fixed_while_the_transcript_scrolls() {
    let terminal = VirtualTerminal::new(20, 6);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let transcript_text = Text::new(Some(&line_items(8)), Some(0), Some(0), None);
    let transcript = ScrollView::new(
        Arc::new(transcript_text.clone()),
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            primary: Some(true),
            ..Default::default()
        }),
    );
    let dock = VStack::new(
        Some(vec![
            StackChild::Component(Arc::new(Text::new(Some("editor"), Some(0), Some(0), None))),
            StackChild::Component(Arc::new(Text::new(Some("footer"), Some(0), Some(0), None))),
        ]),
        None,
    );
    tui.set_layout_root(Some(Arc::new(VStack::new(
        Some(vec![
            StackChild::Entry(StackEntry {
                component: Arc::new(transcript.clone()),
                basis: Some(StackBasis::Fixed(0.0)),
                grow: Some(1),
                shrink: None,
                min_size: Some(1),
                max_size: None,
                visible: None,
            }),
            StackChild::Entry(StackEntry {
                component: Arc::new(dock),
                basis: Some(StackBasis::Auto),
                grow: None,
                shrink: None,
                min_size: Some(1),
                max_size: None,
                visible: None,
            }),
        ]),
        None,
    ))));
    tui.start();
    terminal.wait_for_render().await;

    assert_eq!(
        trimmed_viewport(&terminal),
        vec!["line 5", "line 6", "line 7", "line 8", "editor", "footer"]
    );

    // Wheel over the dock falls back to the primary transcript scroll view.
    terminal.send_input("\x1b[<64;1;6M");
    terminal.wait_for_render().await;
    assert_eq!(
        trimmed_viewport(&terminal),
        vec!["line 4", "line 5", "line 6", "line 7", "editor", "footer"]
    );
    assert!(!transcript.is_following_end());

    transcript_text.set_text(&line_items(10));
    tui.request_render(None);
    terminal.wait_for_render().await;
    assert_eq!(
        trimmed_viewport(&terminal),
        vec!["line 4", "line 5", "line 6", "line 7", "editor", "footer"]
    );

    tui.scroll_to_bottom();
    terminal.wait_for_render().await;
    assert_eq!(
        trimmed_viewport(&terminal),
        vec!["line 7", "line 8", "line 9", "line 10", "editor", "footer"]
    );
    tui.stop(None);
}

#[test]
fn invalidates_overlays_with_an_explicit_layout_root() {
    let tui = TuiAltScreen::new(Arc::new(VirtualTerminal::default()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let overlay = Arc::new(InvalidateSpyComponent::new(Text::new(
        Some("overlay"),
        Some(0),
        Some(0),
        None,
    )));
    tui.set_layout_root(Some(Arc::new(Text::new(Some("root"), Some(0), Some(0), None))));
    tui.show_overlay(Arc::clone(&overlay) as Arc<dyn Component>, None);

    tui.invalidate();

    assert!(overlay.was_invalidated());
    tui.stop(None);
}

#[tokio::test]
async fn routes_wheel_input_to_the_scroll_view_under_the_pointer() {
    let terminal = VirtualTerminal::new(20, 4);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let left = ScrollView::new(
        Arc::new(Text::new(Some("a1\na2\na3\na4\na5\na6\na7"), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            primary: Some(true),
            ..Default::default()
        }),
    );
    let right = ScrollView::new(
        Arc::new(Text::new(Some("b1\nb2\nb3\nb4\nb5\nb6\nb7"), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            ..Default::default()
        }),
    );
    tui.set_layout_root(Some(Arc::new(HStack::new(
        Some(vec![
            StackChild::Entry(StackEntry {
                component: Arc::new(left.clone()),
                basis: Some(StackBasis::Fixed(10.0)),
                grow: None,
                shrink: Some(0),
                min_size: None,
                max_size: None,
                visible: None,
            }),
            StackChild::Entry(StackEntry {
                component: Arc::new(right.clone()),
                basis: Some(StackBasis::Fixed(10.0)),
                grow: None,
                shrink: Some(0),
                min_size: None,
                max_size: None,
                visible: None,
            }),
        ]),
        None::<StackOptions>,
    ))));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<64;15;1M");
    terminal.wait_for_render().await;
    assert_eq!(left.scroll_top(), 3);
    assert_eq!(right.scroll_top(), 2);
    assert_eq!(
        trimmed_viewport(&terminal),
        vec!["a4        b3", "a5        b4", "a6        b5", "a7        b6"]
    );
    tui.stop(None);
}

#[tokio::test]
async fn scrolls_faster_while_alt_is_held_during_wheel_input() {
    let terminal = VirtualTerminal::new(20, 4);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let text = Text::new(Some(&line_items(12)), Some(0), Some(0), None);
    tui.add_child(Arc::new(text));
    tui.start();
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 8);

    // Alt modifier sets bit 8 on the wheel button (72 = 64 + 8).
    terminal.send_input("\x1b[<72;1;1M");
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 3);
    tui.stop(None);
}

#[tokio::test]
async fn does_not_vertically_redispatch_misses_through_horizontal_layout_containers() {
    let terminal = VirtualTerminal::new(20, 2);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let selections = Arc::new(AtomicI64::new(0));
    let list = SelectList::new(
        vec![
            SelectItem {
                value: "a".to_string(),
                label: "A".to_string(),
                description: None,
            },
            SelectItem {
                value: "b".to_string(),
                label: "B".to_string(),
                description: None,
            },
        ],
        2,
        identity_select_list_theme(),
        None,
    );
    let selections_ref = Arc::clone(&selections);
    list.set_on_select(Some(Arc::new(move |_| {
        selections_ref.fetch_add(1, Ordering::SeqCst);
    })));
    tui.set_layout_root(Some(Arc::new(HStack::new(
        Some(vec![
            StackChild::Entry(StackEntry {
                component: Arc::new(list),
                basis: Some(StackBasis::Fixed(10.0)),
                grow: None,
                shrink: None,
                min_size: None,
                max_size: None,
                visible: None,
            }),
            StackChild::Entry(StackEntry {
                component: Arc::new(Text::new(Some("plain"), Some(0), Some(0), None)),
                basis: Some(StackBasis::Fixed(10.0)),
                grow: None,
                shrink: None,
                min_size: None,
                max_size: None,
                visible: None,
            }),
        ]),
        None,
    ))));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;15;1M");
    terminal.send_input("\x1b[<0;15;1m");
    terminal.wait_for_render().await;
    assert_eq!(selections.load(Ordering::SeqCst), 0);
    tui.stop(None);
}

#[test]
#[serial]
fn uses_button_motion_tracking_inside_terminal_multiplexers() {
    let mut env = EnvGuard::set_many(&[
        ("TMUX", None),
        ("ZELLIJ", None),
        ("STY", None),
        ("TERM", Some("xterm-256color")),
    ]);

    let direct_terminal = RecordingTerminal::default();
    let direct_tui = TuiAltScreen::new(Arc::new(direct_terminal.clone()), None, None, None);
    direct_tui.start();
    let direct_writes = direct_terminal.all_writes_joined();
    assert!(direct_writes.contains("\x1b[?1003h"));
    direct_tui.stop(None);

    let multiplexers: &[(&str, &[(&str, &str)])] = &[
        ("tmux environment", &[("TMUX", "/tmp/tmux/default,1,0")]),
        ("tmux TERM", &[("TERM", "tmux-256color")]),
        ("Zellij environment", &[("ZELLIJ", "0")]),
        ("Screen environment", &[("STY", "123.session")]),
        ("Screen TERM", &[("TERM", "screen-256color")]),
    ];

    for (name, entries) in multiplexers {
        for key in ["TMUX", "ZELLIJ", "STY", "TERM"] {
            env.stub(key, None);
        }
        for (key, value) in *entries {
            env.stub(key, Some(value));
        }
        let terminal = RecordingTerminal::default();
        let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
        tui.start();
        let writes = terminal.all_writes_joined();
        assert!(
            writes.contains("\x1b[?1002h"),
            "{name} should enable button-motion tracking"
        );
        assert!(
            !writes.contains("\x1b[?1003h"),
            "{name} should not enable all-motion tracking"
        );
        assert!(
            writes.contains("\x1b[?1006h"),
            "{name} should enable SGR mouse encoding"
        );
        tui.stop(None);
    }
}

#[test]
#[serial]
fn invokes_the_right_click_paste_handler_only_on_windows_outside_vs_code() {
    let terminal = VirtualTerminal::default();
    let paste_count = Arc::new(AtomicI64::new(0));
    let paste_count_ref = Arc::clone(&paste_count);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            on_right_click_paste: Some(Arc::new(move || {
                paste_count_ref.fetch_add(1, Ordering::SeqCst);
            })),
            ..default_tui_options()
        }),
    );
    let mut guard = StopGuard::new(tui.clone());

    let mut env = EnvGuard::set("TERM_PROGRAM", None);
    let _win = PlatformGuard::set("win32");
    tui.start();
    terminal.send_input("\x1b[<2;1;1M");
    terminal.send_input("\x1b[<2;1;1m");
    assert_eq!(paste_count.load(Ordering::SeqCst), 1);

    env.stub("TERM_PROGRAM", Some("vscode"));
    terminal.send_input("\x1b[<2;1;1M");
    assert_eq!(paste_count.load(Ordering::SeqCst), 1);

    let _linux = PlatformGuard::set("linux");
    env.stub("TERM_PROGRAM", None);
    terminal.send_input("\x1b[<2;1;1M");
    assert_eq!(paste_count.load(Ordering::SeqCst), 1);

    guard.stop_now();
}

#[tokio::test]
async fn reveals_an_auto_scrollbar_when_the_pointer_enters_its_hidden_track() {
    let terminal = RecordingTerminal::new(10, 5);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let scroll_view = ScrollView::new(
        Arc::new(Text::new(Some(&line_items(20)), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            primary: Some(true),
            scrollbar: Some(ScrollViewScrollbar::Auto),
            scrollbar_hide_delay_ms: Some(20),
            ..Default::default()
        }),
    );
    tui.set_layout_root(Some(Arc::new(scroll_view.clone())));
    tui.start();
    terminal.wait_for_render().await;
    assert!(!scroll_view.is_scrollbar_visible());

    terminal.send_input("\x1b[<35;10;3M");
    terminal.wait_for_render().await;
    assert!(scroll_view.is_scrollbar_visible());
    assert!(scroll_view.is_scrollbar_active());
    let sb_re = Regex::new("[│█]").expect("valid regex");
    assert!(terminal.get_viewport().iter().any(|line| sb_re.is_match(line)));

    terminal.send_input("\x1b[<35;9;3M");
    tokio::time::sleep(Duration::from_millis(40)).await;
    terminal.wait_for_render().await;
    assert!(!scroll_view.is_scrollbar_visible());
    tui.stop(None);
}

#[tokio::test]
async fn jumps_to_a_scrollbar_track_position_and_continues_dragging_from_there() {
    let terminal = RecordingTerminal::new(10, 10);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let scroll_view = ScrollView::new(
        Arc::new(Text::new(Some(&line_items(50)), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            primary: Some(true),
            scrollbar: Some(ScrollViewScrollbar::Always),
            ..Default::default()
        }),
    );
    tui.set_layout_root(Some(Arc::new(scroll_view.clone())));
    tui.start();
    terminal.wait_for_render().await;
    assert_eq!(scroll_view.scroll_top(), 0);

    terminal.send_input("\x1b[<0;10;6M");
    terminal.wait_for_render().await;
    assert_eq!(scroll_view.scroll_top(), 20);

    terminal.send_input("\x1b[<32;10;10M");
    terminal.wait_for_render().await;
    assert_eq!(scroll_view.scroll_top(), 40);

    terminal.send_input("\x1b[<0;10;10m");
    terminal.wait_for_render().await;
    assert!(!terminal.all_writes_joined().contains("\x1b]52;c;"));
    tui.stop(None);
}

// #9758: wheel line counts can change at runtime; Alt keeps its multiplier.
#[tokio::test]
async fn applies_runtime_wheel_line_count_updates() {
    let terminal = VirtualTerminal::new(20, 4);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            wheel_scroll_lines: Some(WheelScrollLines::Lines(3.0)),
            ..default_tui_options()
        }),
    );
    let mut guard = StopGuard::new(tui.clone());
    let deltas: Arc<Mutex<Vec<Option<i64>>>> = Arc::new(Mutex::new(Vec::new()));
    let deltas_ref = Arc::clone(&deltas);
    tui.add_child(Arc::new(MouseRegion::new(
        Arc::new(Text::new(Some("wheel target"), Some(0), Some(0), None)),
        Arc::new(move |event| {
            if event.r#type != TuiMouseEventType::Wheel {
                return None;
            }
            deltas_ref.lock().expect("deltas lock").push(event.wheel_delta);
            Some(TuiMouseEventResult {
                handled: Some(true),
                ..Default::default()
            })
        }),
    )));
    tui.start();
    terminal.wait_for_render().await;
    terminal.send_input("\x1b[<64;1;1M");
    tui.set_wheel_scroll_lines(WheelScrollLines::Lines(2.0));
    terminal.send_input("\x1b[<65;1;1M");
    terminal.send_input("\x1b[<72;1;1M");
    assert_eq!(*deltas.lock().expect("deltas lock"), vec![Some(-3), Some(2), Some(-10)]);
    guard.stop_now();
}

#[tokio::test]
async fn chains_unused_wheel_delta_to_an_outer_scroll_view() {
    let terminal = VirtualTerminal::new(20, 4);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            wheel_scroll_lines: Some(WheelScrollLines::Lines(3.0)),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    let inner = ScrollView::new(
        Arc::new(Text::new(Some("i1\ni2\ni3\ni4\ni5\ni6"), Some(0), Some(0), None)),
        None,
    );
    let outer = ScrollView::new(
        Arc::new(VStack::new(
            Some(vec![
                StackChild::Entry(StackEntry {
                    component: Arc::new(inner.clone()),
                    basis: Some(StackBasis::Fixed(2.0)),
                    grow: None,
                    shrink: None,
                    min_size: None,
                    max_size: None,
                    visible: None,
                }),
                StackChild::Component(Arc::new(Text::new(
                    Some("tail1\ntail2\ntail3\ntail4\ntail5"),
                    Some(0),
                    Some(0),
                    None,
                ))),
            ]),
            None,
        )),
        Some(ScrollViewOptions {
            primary: Some(true),
            ..Default::default()
        }),
    );
    tui.set_layout_root(Some(Arc::new(outer.clone())));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<65;1;1M");
    terminal.wait_for_render().await;
    assert_eq!(inner.scroll_top(), 3);
    assert_eq!(outer.scroll_top(), 0);

    terminal.send_input("\x1b[<65;1;1M");
    terminal.wait_for_render().await;
    assert_eq!(inner.scroll_top(), 4);
    assert_eq!(outer.scroll_top(), 2);
    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn supports_configurable_keyboard_viewport_navigation_with_four_rows_of_page_overlap() {
    let terminal = VirtualTerminal::new(20, 8);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(Some(&line_items(12)), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[57421u");
    terminal.send_input("\x1b[57421;1:3u");
    terminal.wait_for_render().await;
    assert_eq!(
        trimmed_viewport(&terminal),
        vec![
            "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8"
        ]
    );

    terminal.send_input("\x1b[57422u");
    terminal.send_input("\x1b[57422;1:3u");
    terminal.wait_for_render().await;
    assert_eq!(
        trimmed_viewport(&terminal),
        vec![
            "line 5", "line 6", "line 7", "line 8", "line 9", "line 10", "line 11", "line 12"
        ]
    );

    terminal.send_input("\x1bOH");
    terminal.wait_for_render().await;
    assert_eq!(
        trimmed_viewport(&terminal),
        vec![
            "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8"
        ]
    );

    terminal.send_input("\x1bOF");
    terminal.wait_for_render().await;
    assert_eq!(
        trimmed_viewport(&terminal),
        vec![
            "line 5", "line 6", "line 7", "line 8", "line 9", "line 10", "line 11", "line 12"
        ]
    );

    tui.stop(None);
}

#[test]
fn searches_normalized_rendered_transcript_text_across_rows() {
    assert_eq!(
        find_alt_screen_search_matches(&["alpha QUICK".to_string(), "brown fox".to_string()], "quick brown"),
        vec![AltScreenSearchMatch {
            segments: vec![
                AltScreenSearchSegment {
                    row: 0,
                    start_col: 6,
                    end_col: 11,
                },
                AltScreenSearchSegment {
                    row: 1,
                    start_col: 0,
                    end_col: 5,
                },
            ],
        }]
    );
}

#[test]
fn maps_normalized_ascii_and_unicode_search_matches_back_to_rendered_columns() {
    assert_eq!(
        find_alt_screen_search_matches(
            &["\x1b[31mfoo  bar\x1b[0m".to_string(), "A界🙂e\u{0301}Z".to_string()],
            "oo   bar\nA界🙂e\u{0301}",
        ),
        vec![AltScreenSearchMatch {
            segments: vec![
                AltScreenSearchSegment {
                    row: 0,
                    start_col: 1,
                    end_col: 3,
                },
                AltScreenSearchSegment {
                    row: 0,
                    start_col: 5,
                    end_col: 8,
                },
                AltScreenSearchSegment {
                    row: 1,
                    start_col: 0,
                    end_col: 6,
                },
            ],
        }]
    );
}

#[test]
fn reuses_indexed_transcript_matches_until_the_query_or_rendered_lines_change() {
    let index = AltScreenSearchIndex::new();
    let initial = index.search(&["alpha needle".to_string(), "omega".to_string()], "needle");
    assert!(initial.changed);
    assert_eq!(initial.matches.len(), 1);

    let cached = index.search(&["alpha needle".to_string(), "omega".to_string()], "needle");
    assert!(!cached.changed);
    assert_eq!(cached.matches, initial.matches);

    let changed_query = index.search(&["alpha needle".to_string(), "omega".to_string()], "omega");
    assert!(changed_query.changed);
    assert_ne!(changed_query.matches, initial.matches);
    assert_eq!(
        changed_query.matches[0].segments,
        vec![AltScreenSearchSegment {
            row: 1,
            start_col: 0,
            end_col: 5,
        }]
    );

    let changed_lines = index.search(&["alpha needle".to_string(), "no match".to_string()], "omega");
    assert!(changed_lines.changed);
    assert!(changed_lines.matches.is_empty());
}

#[test]
#[serial]
fn renders_transcript_search_with_a_muted_placeholder_and_right_aligned_controls() {
    let component = AltScreenSearchComponent::new(Arc::new(|_| {}), None);
    let rendered = component.render(48);
    let lines: Vec<String> = rendered.iter().map(|line| strip_terminal_sequences(line)).collect();

    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|line| visible_width(line) == 48));
    let top_re = Regex::new("^┌─+┐$").expect("valid regex");
    let mid_re = Regex::new("^│ Find in transcript +│$").expect("valid regex");
    let bot_re = Regex::new(r"^└─+ ↑ Shift\+Enter · ↓ Enter ─┘$").expect("valid regex");
    assert!(top_re.is_match(&lines[0]));
    assert!(mid_re.is_match(&lines[1]));
    assert!(rendered[1].contains("\x1b[2m"));
    assert!(bot_re.is_match(&lines[2]));
    let controls = &lines[2];
    assert_eq!(
        component.get_navigation_direction_at(2, cell_col_of(controls, "↑").expect("↑ col")),
        Some(-1)
    );
    assert_eq!(
        component.get_navigation_direction_at(2, cell_col_of(controls, "Shift+Enter").expect("Shift+Enter col") + 5),
        Some(-1)
    );
    assert_eq!(
        component.get_navigation_direction_at(2, cell_col_of(controls, "·").expect("· col")),
        None
    );
    assert_eq!(
        component.get_navigation_direction_at(2, cell_col_of(controls, "↓").expect("↓ col")),
        Some(1)
    );
    assert_eq!(
        component.get_navigation_direction_at(2, last_cell_col_of(controls, "Enter").expect("Enter col") + 2),
        Some(1)
    );

    component.handle_input("n");
    component.set_result(0, 2);
    let populated_render = component.render(48);
    let populated: Vec<String> = populated_render
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect();
    assert!(populated[1].contains("n"));
    assert!(populated[1].contains("1/2"));
    assert!(populated_render[1].contains("\x1b[2m 1/2 \x1b[22m"));
    assert!(!populated.iter().any(|line| line.contains("Find in transcript")));
}

#[tokio::test]
#[serial]
async fn navigates_transcript_search_with_hoverable_arrow_buttons_and_toggles_it_with_its_shortcut() {
    let terminal = RecordingTerminal::new(120, 6);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            search_navigation_button_style: Some(Arc::new(|text, hovered| {
                format!("{}{text}\x1b[49m", if hovered { "\x1b[45m" } else { "\x1b[44m" })
            })),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("needle one\nmiddle\nneedle two\nend"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[102;6u");
    terminal.send_input("needle");
    terminal.wait_for_render().await;
    let mut viewport = terminal.get_viewport();
    assert!(viewport.iter().any(|line| line.contains("1/2")));
    assert!(viewport.iter().any(|line| line.contains("↑ Shift+Enter · ↓ Enter")));

    let arrow_row = viewport
        .iter()
        .position(|line| line.contains('↑') && line.contains('↓'))
        .expect("arrow row present");
    let arrow_column = last_cell_col_of(&viewport[arrow_row], "Enter").expect("Enter col present");
    let hover_event_count = terminal.event_count();
    terminal.send_input(&format!("\x1b[<35;{};{}M", arrow_column + 1, arrow_row + 1));
    terminal.wait_for_render().await;
    assert!(
        terminal
            .writes_since(hover_event_count)
            .contains("\x1b[45m↓ Enter\x1b[49m")
    );

    terminal.send_input(&format!("\x1b[<0;{};{}M", arrow_column + 1, arrow_row + 1));
    terminal.wait_for_render().await;
    viewport = terminal.get_viewport();
    assert!(viewport.iter().any(|line| line.contains("2/2")));
    assert!(viewport.iter().any(|line| line.contains("↑ Shift+Enter · ↓ Enter")));

    let arrow_row = viewport
        .iter()
        .position(|line| line.contains('↑') && line.contains('↓'))
        .expect("arrow row present");
    let arrow_column = cell_col_of(&viewport[arrow_row], "Shift+Enter").expect("Shift+Enter col") + 3;
    terminal.send_input(&format!("\x1b[<0;{};{}M", arrow_column + 1, arrow_row + 1));
    terminal.wait_for_render().await;
    assert!(terminal.get_viewport().iter().any(|line| line.contains("1/2")));
    assert!(
        terminal
            .get_viewport()
            .iter()
            .any(|line| line.contains("↑ Shift+Enter · ↓ Enter"))
    );

    terminal.send_input("\x1b[102;6u");
    terminal.wait_for_render().await;
    assert!(
        !terminal
            .get_viewport()
            .iter()
            .any(|line| line.contains("↑ Shift+Enter · ↓ Enter"))
    );
    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn does_not_treat_transcript_box_drawing_as_search_navigation_buttons() {
    let terminal = VirtualTerminal::new(80, 10);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let text = [
        "needle one",
        "middle",
        "needle two",
        "filler",
        "┌────────────────────────────────────────┐",
        "│ box                                    │",
        "└────────────────────────────────────────┘",
        "end",
    ]
    .join("\n");
    tui.add_child(Arc::new(Text::new(Some(&text), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[102;6u");
    terminal.send_input("needle");
    terminal.wait_for_render().await;
    let mut viewport = terminal.get_viewport();
    assert!(viewport.iter().any(|line| line.contains("1/2")));
    assert!(!viewport.iter().any(|line| line.contains("2/2")));

    let box_bottom_row = viewport
        .iter()
        .position(|line| line.starts_with('└'))
        .expect("box bottom row present");
    terminal.send_input(&format!("\x1b[<0;24;{}M", box_bottom_row + 1));
    terminal.wait_for_render().await;

    viewport = terminal.get_viewport();
    assert!(viewport.iter().any(|line| line.contains("1/2")));
    assert!(!viewport.iter().any(|line| line.contains("2/2")));
    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn uses_configured_styles_for_current_and_non_current_search_matches() {
    let terminal = RecordingTerminal::new(60, 4);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            search_match_style: Some(Arc::new(|text| format!("\x1b[41m{text}\x1b[49m"))),
            search_current_match_style: Some(Arc::new(|text| format!("\x1b[42m{text}\x1b[49m"))),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("needle first\nmiddle\nneedle second\nend"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[102;6u");
    terminal.send_input("needle");
    terminal.wait_for_render().await;

    let all_writes = terminal.all_writes_joined();
    assert!(all_writes.contains("\x1b[42mneedle\x1b[49m"));
    assert!(all_writes.contains("\x1b[41mneedle\x1b[49m"));
    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn searches_the_transcript_with_ctrl_shift_f_and_restores_editor_focus_on_close() {
    let terminal = RecordingTerminal::new(60, 8);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let transcript_lines = (0..12)
        .map(|index| {
            if index == 4 {
                "line 5 needle one".to_string()
            } else if index == 9 {
                "line 10 needle two".to_string()
            } else {
                format!("line {}", index + 1)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let transcript = ScrollView::new(
        Arc::new(Text::new(Some(&transcript_lines), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            primary: Some(true),
            ..Default::default()
        }),
    );
    let editor = Arc::new(MockEditorComponent::new());
    tui.set_layout_root(Some(Arc::new(VStack::new(
        Some(vec![
            StackChild::Entry(StackEntry {
                component: Arc::new(transcript.clone()),
                basis: Some(StackBasis::Fixed(0.0)),
                grow: Some(1),
                shrink: None,
                min_size: Some(1),
                max_size: None,
                visible: None,
            }),
            StackChild::Entry(StackEntry {
                component: Arc::clone(&editor) as Arc<dyn Component>,
                basis: Some(StackBasis::Fixed(1.0)),
                grow: None,
                shrink: Some(0),
                min_size: None,
                max_size: None,
                visible: None,
            }),
        ]),
        None,
    ))));
    tui.set_focus(Some(Arc::clone(&editor) as Arc<dyn Component>));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[102;6u");
    terminal.send_input("needle");
    terminal.wait_for_render().await;
    assert!(!transcript.is_following_end());
    let viewport = terminal.get_viewport();
    assert!(viewport.iter().any(|line| line.contains("2/2")));
    assert!(viewport.iter().any(|line| line.contains("↑ Shift+Enter · ↓ Enter")));
    assert!(viewport.iter().any(|line| line.contains("line 10 needle two")));
    assert!(editor.inputs().is_empty());
    assert!(terminal.all_writes_joined().contains("\x1b[1;7mneedle\x1b[22;27m"));

    for _ in 0..6 {
        terminal.send_input("\x1b[<64;1;4M");
    }
    terminal.wait_for_render().await;
    assert_eq!(transcript.scroll_top(), 0);
    assert!(
        terminal
            .get_viewport()
            .iter()
            .any(|line| line.contains("needle") && line.contains("2/2"))
    );

    terminal.send_input("\x07");
    terminal.wait_for_render().await;
    let viewport = terminal.get_viewport();
    assert!(viewport.iter().any(|line| line.contains("1/2")));
    assert!(viewport.iter().any(|line| line.contains("line 5 needle one")));

    terminal.send_input("\x1b[103;6u");
    terminal.wait_for_render().await;
    let viewport = terminal.get_viewport();
    assert!(viewport.iter().any(|line| line.contains("2/2")));
    assert!(viewport.iter().any(|line| line.contains("line 10 needle two")));

    terminal.send_input("\x1b");
    terminal.send_input("x");
    terminal.wait_for_render().await;
    assert!(
        !terminal
            .get_viewport()
            .iter()
            .any(|line| line.contains("↑ Shift+Enter · ↓ Enter"))
    );
    assert_eq!(editor.inputs(), vec!["x".to_string()]);

    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn scrolls_the_transcript_by_half_a_page_with_custom_bindings() {
    let _kb_guard = KeybindingsGuard::new();
    let terminal = VirtualTerminal::new(20, 10);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let mut stop_guard = StopGuard::new(tui.clone());

    let mut cfg = IndexMap::new();
    cfg.insert(
        "tui.altScreen.halfPageUp".to_string(),
        Some(KeyIdOrList::One("ctrl+u".to_string())),
    );
    cfg.insert(
        "tui.altScreen.halfPageDown".to_string(),
        Some(KeyIdOrList::One("ctrl+d".to_string())),
    );
    set_keybindings(KeybindingsManager::new((&*TUI_KEYBINDINGS).clone(), Some(cfg)));

    tui.add_child(Arc::new(Text::new(Some(&line_items(30)), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 20);

    terminal.send_input("\x15");
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 15);

    terminal.send_input("\x04");
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 20);

    stop_guard.stop_now();
}

#[tokio::test]
#[serial]
async fn scrolls_the_transcript_by_one_line_with_custom_bindings() {
    let _kb_guard = KeybindingsGuard::new();
    let terminal = VirtualTerminal::new(20, 10);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let mut stop_guard = StopGuard::new(tui.clone());

    let mut cfg = IndexMap::new();
    cfg.insert(
        "tui.altScreen.lineUp".to_string(),
        Some(KeyIdOrList::One("ctrl+y".to_string())),
    );
    cfg.insert(
        "tui.altScreen.lineDown".to_string(),
        Some(KeyIdOrList::One("ctrl+e".to_string())),
    );
    set_keybindings(KeybindingsManager::new((&*TUI_KEYBINDINGS).clone(), Some(cfg)));

    tui.add_child(Arc::new(Text::new(Some(&line_items(30)), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 20);

    terminal.send_input("\x19");
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 19);

    terminal.send_input("\x05");
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 20);

    stop_guard.stop_now();
}

#[tokio::test]
#[serial]
async fn routes_ctrl_modified_viewport_navigation_to_the_focused_component() {
    let terminal = VirtualTerminal::new(20, 6);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let transcript = ScrollView::new(
        Arc::new(Text::new(Some(&line_items(12)), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            primary: Some(true),
            ..Default::default()
        }),
    );
    let editor = Arc::new(MockEditorComponent::new());
    tui.set_layout_root(Some(Arc::new(VStack::new(
        Some(vec![
            StackChild::Entry(StackEntry {
                component: Arc::new(transcript.clone()),
                basis: Some(StackBasis::Fixed(0.0)),
                grow: Some(1),
                shrink: None,
                min_size: Some(1),
                max_size: None,
                visible: None,
            }),
            StackChild::Entry(StackEntry {
                component: Arc::clone(&editor) as Arc<dyn Component>,
                basis: Some(StackBasis::Fixed(1.0)),
                grow: None,
                shrink: Some(0),
                min_size: None,
                max_size: None,
                visible: None,
            }),
        ]),
        None,
    ))));
    tui.set_focus(Some(Arc::clone(&editor) as Arc<dyn Component>));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1bOH");
    terminal.wait_for_render().await;
    assert_eq!(transcript.scroll_top(), 0);
    assert!(editor.inputs().is_empty());

    let modified_inputs = vec![
        "\x1b[1;5H".to_string(),
        "\x1b[1;5F".to_string(),
        "\x1b[5;5~".to_string(),
        "\x1b[6;5~".to_string(),
        "\x1b[57423;5u".to_string(),
    ];
    for input in &modified_inputs {
        terminal.send_input(input);
    }
    terminal.send_input("\x1b[57423;5:3u");
    terminal.wait_for_render().await;
    assert_eq!(transcript.scroll_top(), 0);
    assert_eq!(editor.inputs(), modified_inputs);

    terminal.send_input("\x1b[6~");
    terminal.wait_for_render().await;
    assert_eq!(transcript.scroll_top(), 1);
    assert_eq!(editor.inputs(), modified_inputs);

    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn jumps_between_osc_133_semantic_prompt_markers() {
    let terminal = VirtualTerminal::new(20, 3);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let prompt_text = (1..=4)
        .flat_map(|message| vec![format!("{OSC133_ZONE_START}message {message}"), "detail".to_string()])
        .collect::<Vec<_>>()
        .join("\n");
    tui.add_child(Arc::new(Text::new(Some(&prompt_text), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 5);

    terminal.send_input("\x1b[57419;6u");
    terminal.send_input("\x1b[57419;6:3u");
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 4);
    assert_eq!(terminal.get_viewport()[0].trim_end(), "message 3");

    terminal.send_input("\x1b[1;6A");
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 2);
    assert_eq!(terminal.get_viewport()[0].trim_end(), "message 2");

    terminal.send_input("\x1b[57420;6u");
    terminal.send_input("\x1b[57420;6:3u");
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 4);
    assert_eq!(terminal.get_viewport()[0].trim_end(), "message 3");

    terminal.send_input("\x1b[1;6B");
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 5);
    assert_eq!(terminal.get_viewport()[1].trim_end(), "message 4");
    assert!(tui.is_following_output());

    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn does_not_emit_kitty_graphics_commands_or_osc_133_zones_in_iterm2() {
    let _caps = CapsGuard::iterm2();
    let terminal = RecordingTerminal::new(20, 3);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let mut guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(RenderClosureComponent::new(|_| {
        vec!["\x1b]133;B\x07\x1b]133;C\x07\x1b]133;A\x07content".to_string()]
    })));
    tui.add_child(Arc::new(Image::new(
        "AAAA",
        "image/png",
        identity_image_theme(),
        Some(pi_tui::components::image::ImageOptions {
            filename: Some("example.png".to_string()),
            ..Default::default()
        }),
        Some(ImageDimensions {
            width_px: 10,
            height_px: 10,
        }),
    )));
    tui.start();
    terminal.wait_for_render().await;
    guard.stop_now();

    let writes = terminal.all_writes_joined();
    assert!(!writes.contains("\x1b_G"));
    assert!(!writes.contains("\x1b]133;"));
    assert!(!writes.contains("\x1b]1337;File="));
    assert!(writes.contains("[Image:"));
}

#[tokio::test]
#[serial]
async fn clears_stale_iterm2_image_placements_when_they_leave_the_viewport() {
    let _caps = CapsGuard::iterm2();
    let terminal = RecordingTerminal::new(20, 3);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let image_line = "\x1b]1337;File=inline=1;width=2;height=auto:AAAA\x07".to_string();
    tui.add_child(Arc::new(RenderClosureComponent::new(move |_| {
        vec![
            image_line.clone(),
            String::new(),
            String::new(),
            "after".to_string(),
            "more".to_string(),
            "end".to_string(),
        ]
    })));
    tui.start();
    terminal.wait_for_render().await;
    tui.scroll_to_top();
    terminal.wait_for_render().await;
    let event_count = terminal.event_count();

    tui.scroll_by(1);
    terminal.wait_for_render().await;
    assert!(terminal.writes_since(event_count).contains("\x1b[2J"));
    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn crops_a_kitty_image_whose_first_line_is_above_the_viewport() {
    let _caps = CapsGuard::kitty();
    let terminal = RecordingTerminal::new(20, 3);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let image_id = 123;
    let image_line = encode_kitty(
        "AAAA",
        Some(EncodeKittyOptions {
            columns: Some(2),
            rows: Some(3),
            image_id: Some(image_id),
            move_cursor: Some(false),
        }),
    );
    register_kitty_image_metadata(KittyImageMetadata {
        image_id,
        columns: 2,
        rows: 3,
        width_px: 100,
        height_px: 100,
    });
    tui.add_child(Arc::new(RenderClosureComponent::new(move |_| {
        vec![
            "before".to_string(),
            image_line.clone(),
            String::new(),
            String::new(),
            "after".to_string(),
            "end".to_string(),
        ]
    })));
    tui.start();
    terminal.wait_for_render().await;

    assert_eq!(tui.viewport_top(), 3);
    assert!(
        terminal
            .writes()
            .iter()
            .any(|w| w.contains("i=123") && w.contains("y=66,h=34,r=1"))
    );

    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn reuses_moved_kitty_images_without_dropping_hstack_siblings() {
    let _caps = CapsGuard::kitty();
    let terminal = RecordingTerminal::new(20, 6);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let label = Text::new(Some("left"), Some(0), Some(0), None);
    let image = Image::new(
        &"A".repeat(8192),
        "image/png",
        identity_image_theme(),
        None,
        Some(ImageDimensions {
            width_px: 100,
            height_px: 100,
        }),
    );
    let header = Text::new(Some("header"), Some(0), Some(0), None);
    let row = HStack::new(
        Some(vec![
            StackChild::Entry(StackEntry {
                component: Arc::new(label.clone()),
                basis: Some(StackBasis::Fixed(10.0)),
                grow: None,
                shrink: None,
                min_size: None,
                max_size: None,
                visible: None,
            }),
            StackChild::Entry(StackEntry {
                component: Arc::new(image),
                basis: Some(StackBasis::Fixed(10.0)),
                grow: None,
                shrink: None,
                min_size: None,
                max_size: None,
                visible: None,
            }),
        ]),
        None,
    );
    tui.set_layout_root(Some(Arc::new(VStack::new(
        Some(vec![
            StackChild::Entry(StackEntry {
                component: Arc::new(header.clone()),
                basis: Some(StackBasis::Auto),
                grow: None,
                shrink: None,
                min_size: None,
                max_size: None,
                visible: None,
            }),
            StackChild::Entry(StackEntry {
                component: Arc::new(row),
                basis: Some(StackBasis::Fixed(4.0)),
                grow: None,
                shrink: None,
                min_size: None,
                max_size: None,
                visible: None,
            }),
        ]),
        None,
    ))));
    tui.start();
    terminal.wait_for_render().await;
    assert!(terminal.all_writes_joined().contains("\x1b_Ga=T"));

    let event_count = terminal.event_count();
    label.set_text("changed");
    header.set_text("header\nsecond");
    tui.request_render(None);
    terminal.wait_for_render().await;
    let redraw_writes = terminal.writes_since(event_count);
    let placement_index = redraw_writes.find("\x1b_Ga=p,q=2").expect("placement write");
    let changed_index = redraw_writes.find("changed").expect("changed text write");
    assert!(redraw_writes.contains("\x1b_Ga=d,d=a,q=2\x1b\\"));
    assert!(placement_index > changed_index);
    assert!(!redraw_writes.contains("\x1b_Ga=T"));
    assert!(
        redraw_writes.len() < 2000,
        "expected placement-only redraw, got {} bytes",
        redraw_writes.len()
    );
    assert!(terminal.get_viewport().iter().any(|line| line.trim_end() == "changed"));
    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn retains_recently_offscreen_kitty_images_for_placement_only_reuse() {
    let _caps = CapsGuard::kitty();
    let terminal = RecordingTerminal::new(20, 1);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let image_id = 321;
    let image_line = encode_kitty(
        "AAAA",
        Some(EncodeKittyOptions {
            columns: Some(2),
            rows: Some(1),
            image_id: Some(image_id),
            move_cursor: Some(false),
        }),
    );
    register_kitty_image_metadata(KittyImageMetadata {
        image_id,
        columns: 2,
        rows: 1,
        width_px: 100,
        height_px: 50,
    });
    tui.set_layout_root(Some(Arc::new(ScrollView::new(
        Arc::new(RenderClosureComponent::new(move |_| {
            vec![image_line.clone(), "after".to_string()]
        })),
        Some(ScrollViewOptions {
            primary: Some(true),
            ..Default::default()
        }),
    ))));
    tui.start();
    terminal.wait_for_render().await;
    assert!(terminal.all_writes_joined().contains("\x1b_Ga=T"));

    let event_count = terminal.event_count();
    tui.scroll_by(1);
    terminal.wait_for_render().await;
    tui.scroll_by(-1);
    terminal.wait_for_render().await;
    let reentry_writes = terminal.writes_since(event_count);
    assert!(reentry_writes.contains("\x1b_Ga=p,q=2"));
    assert!(!reentry_writes.contains("\x1b_Ga=T"));
    assert!(!reentry_writes.contains(&format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")));
    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn evicts_the_least_recently_visible_kitty_image_when_the_cache_is_full() {
    let _caps = CapsGuard::kitty();
    let terminal = RecordingTerminal::new(20, 1);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let first_image_id = 500;
    let image_lines: Vec<String> = (0..18)
        .map(|index| {
            let image_id = first_image_id + index;
            register_kitty_image_metadata(KittyImageMetadata {
                image_id,
                columns: 2,
                rows: 1,
                width_px: 100,
                height_px: 50,
            });
            encode_kitty(
                "AAAA",
                Some(EncodeKittyOptions {
                    columns: Some(2),
                    rows: Some(1),
                    image_id: Some(image_id),
                    move_cursor: Some(false),
                }),
            )
        })
        .collect();
    let line_count = image_lines.len();
    tui.set_layout_root(Some(Arc::new(ScrollView::new(
        Arc::new(RenderClosureComponent::new(move |_| image_lines.clone())),
        Some(ScrollViewOptions {
            primary: Some(true),
            ..Default::default()
        }),
    ))));
    tui.start();
    terminal.wait_for_render().await;
    for _ in 1..line_count {
        tui.scroll_by(1);
        terminal.wait_for_render().await;
    }
    assert!(
        terminal
            .all_writes_joined()
            .contains(&format!("\x1b_Ga=d,d=I,i={first_image_id},q=2\x1b\\"))
    );

    let event_count = terminal.event_count();
    tui.scroll_to_top();
    terminal.wait_for_render().await;
    let reentry_writes = terminal.writes_since(event_count);
    assert!(reentry_writes.contains("\x1b_Ga=T"));
    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn evicts_offscreen_kitty_images_when_decoded_raster_memory_exceeds_the_cache_quota() {
    let _caps = CapsGuard::kitty();
    let terminal = RecordingTerminal::new(20, 1);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let first_image_id = 600;
    let image_lines: Vec<String> = (0..4)
        .map(|index| {
            let image_id = first_image_id + index;
            register_kitty_image_metadata(KittyImageMetadata {
                image_id,
                columns: 2,
                rows: 1,
                width_px: 3840,
                height_px: 2160,
            });
            encode_kitty(
                "AAAA",
                Some(EncodeKittyOptions {
                    columns: Some(2),
                    rows: Some(1),
                    image_id: Some(image_id),
                    move_cursor: Some(false),
                }),
            )
        })
        .collect();
    let line_count = image_lines.len();
    tui.set_layout_root(Some(Arc::new(ScrollView::new(
        Arc::new(RenderClosureComponent::new(move |_| image_lines.clone())),
        Some(ScrollViewOptions {
            primary: Some(true),
            ..Default::default()
        }),
    ))));
    tui.start();
    terminal.wait_for_render().await;
    for _ in 1..line_count {
        tui.scroll_by(1);
        terminal.wait_for_render().await;
    }
    assert!(
        terminal
            .all_writes_joined()
            .contains(&format!("\x1b_Ga=d,d=I,i={first_image_id},q=2\x1b\\"))
    );
    tui.stop(None);
}

#[tokio::test]
async fn opens_an_osc_8_hyperlink_with_specific_or_generic_release_codes_but_not_on_drag() {
    let terminal = RecordingTerminal::new(20, 3);
    let opened_urls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let opened_urls_ref = Arc::clone(&opened_urls);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            open_url: Some(Arc::new(move |url| {
                opened_urls_ref.lock().expect("urls lock").push(url.to_string());
            })),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    let url = "https://example.com/path?q=1";
    let bel_url = "https://example.com/bel";
    let emoji_url = "https://example.com/emoji";
    let content = format!(
        "{}\n\x1b]8;;{bel_url}\x07link\x1b]8;;\x07\n{}",
        hyperlink("link", url),
        hyperlink("🙂", emoji_url)
    );
    tui.add_child(Arc::new(Text::new(Some(&content), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;2;1M");
    terminal.send_input("\x1b[<3;2;1m");
    terminal.wait_for_render().await;
    assert_eq!(*opened_urls.lock().expect("urls lock"), vec![url.to_string()]);

    terminal.send_input("\x1b[<0;2;2M");
    terminal.send_input("\x1b[<0;2;2m");
    terminal.wait_for_render().await;
    assert_eq!(
        *opened_urls.lock().expect("urls lock"),
        vec![url.to_string(), bel_url.to_string()]
    );

    terminal.send_input("\x1b[<0;2;3M");
    terminal.send_input("\x1b[<0;2;3m");
    terminal.wait_for_render().await;
    assert_eq!(
        *opened_urls.lock().expect("urls lock"),
        vec![url.to_string(), bel_url.to_string(), emoji_url.to_string()]
    );

    terminal.send_input("\x1b[<0;2;1M");
    terminal.send_input("\x1b[<32;4;1M");
    terminal.send_input("\x1b[<0;4;1m");
    terminal.wait_for_render().await;
    assert_eq!(
        *opened_urls.lock().expect("urls lock"),
        vec![url.to_string(), bel_url.to_string(), emoji_url.to_string()]
    );

    tui.stop(None);
}

#[tokio::test]
async fn selects_visible_text_with_the_mouse_and_copies_it_with_osc_52_after_a_generic_release() {
    let terminal = RecordingTerminal::new(20, 4);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("\x1b[1mal\x1b[0mpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<3;4;2m");
    terminal.wait_for_render().await;

    let expected_clipboard_sequence = format!("\x1b]52;c;{}\x07", b64::encode(b"alpha\nbeta"));
    let all_writes = terminal.all_writes_joined();
    assert!(
        all_writes.contains(&expected_clipboard_sequence),
        "clipboard sequence missing: {:?}",
        terminal.writes()
    );
    assert!(all_writes.contains("\x1b[7m"));
    assert!(
        all_writes.contains("al\x1b[0m\x1b[7mpha"),
        "selection inverse must be reapplied after a reset inside the selection"
    );
    assert!(terminal.get_viewport().iter().any(|line| line.contains("Copied!")));

    tui.stop(None);
}

#[tokio::test]
async fn uses_an_injected_copy_selection_handler_instead_of_osc_52_and_reports_success() {
    let terminal = RecordingTerminal::new(20, 4);
    let copied: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let copied_ref = Arc::clone(&copied);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            copy_selection: Some(Arc::new(move |text| {
                let copied_ref = Arc::clone(&copied_ref);
                Box::pin(async move {
                    copied_ref.lock().expect("copied lock").push(text);
                    CopySelectionResult::Bool(true)
                })
            })),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("alpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;

    assert_eq!(*copied.lock().expect("copied lock"), vec!["alpha\nbeta".to_string()]);
    assert!(
        !terminal.all_writes_joined().contains("\x1b]52;c;"),
        "must not emit OSC 52 when a copySelection handler is provided"
    );
    assert!(terminal.get_viewport().iter().any(|line| line.contains("Copied!")));

    tui.stop(None);
}

#[tokio::test]
async fn leaves_selections_visible_without_copying_when_copy_on_select_is_disabled() {
    let terminal = RecordingTerminal::new(20, 4);
    let copied: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let copied_ref = Arc::clone(&copied);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            copy_on_select: Some(false),
            copy_selection: Some(Arc::new(move |text| {
                let copied_ref = Arc::clone(&copied_ref);
                Box::pin(async move {
                    copied_ref.lock().expect("copied lock").push(text);
                    CopySelectionResult::Bool(true)
                })
            })),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("alpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;

    assert!(copied.lock().expect("copied lock").is_empty());
    assert!(tui.has_active_selection());
    assert!(terminal.all_writes_joined().contains("\x1b[7m"));
    assert!(terminal.get_viewport().iter().all(|line| !line.contains("Copied!")));

    tui.stop(None);
}

#[tokio::test]
async fn copies_an_active_selection_programmatically() {
    let terminal = RecordingTerminal::new(20, 4);
    let copied: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let copied_ref = Arc::clone(&copied);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            copy_selection: Some(Arc::new(move |text| {
                let copied_ref = Arc::clone(&copied_ref);
                Box::pin(async move {
                    copied_ref.lock().expect("copied lock").push(text);
                    CopySelectionResult::Bool(true)
                })
            })),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("alpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    assert!(!tui.has_active_selection());
    assert!(!tui.copy_active_selection_to_clipboard().await);

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;

    assert_eq!(*copied.lock().expect("copied lock"), vec!["alpha\nbeta".to_string()]);
    assert!(tui.has_active_selection());

    copied.lock().expect("copied lock").clear();
    assert!(tui.copy_active_selection_to_clipboard().await);
    terminal.wait_for_render().await;

    assert_eq!(*copied.lock().expect("copied lock"), vec!["alpha\nbeta".to_string()]);
    assert!(terminal.get_viewport().iter().any(|line| line.contains("Copied!")));

    tui.stop(None);
}

#[tokio::test]
async fn flashes_an_error_when_the_injected_copy_selection_handler_fails() {
    let terminal = RecordingTerminal::new(20, 4);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            copy_selection: Some(Arc::new(|_| Box::pin(async { CopySelectionResult::Bool(false) }))),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("alpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;

    assert!(terminal.get_viewport().iter().any(|line| line.contains("Copy failed")));
    assert!(
        !terminal.all_writes_joined().contains("\x1b]52;c;"),
        "must not emit OSC 52 when a copySelection handler is provided"
    );

    tui.stop(None);
}

#[tokio::test]
async fn flashes_a_specific_error_returned_by_the_injected_copy_selection_handler() {
    // Regression test for #9618.
    let terminal = RecordingTerminal::new(80, 4);
    let tui = TuiAltScreen::new(
        Arc::new(terminal.clone()),
        None,
        None,
        Some(TuiAltScreenOptions {
            copy_on_select: Some(false),
            copy_selection: Some(Arc::new(|_| {
                Box::pin(async {
                    CopySelectionResult::Message("Clipboard unavailable: install wl-clipboard".to_string())
                })
            })),
            ..default_tui_options()
        }),
    );
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("alpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;
    assert!(!tui.copy_active_selection_to_clipboard().await);
    terminal.wait_for_render().await;

    assert!(
        terminal
            .get_viewport()
            .iter()
            .any(|line| line.contains("Clipboard unavailable: install wl-clipboard"))
    );
    assert!(terminal.get_viewport().iter().all(|line| !line.contains("Copy failed")));

    // Error flash stays visible past the 1000ms default flash duration (uses 5000ms).
    tokio::time::sleep(Duration::from_millis(1050)).await;
    terminal.wait_for_render().await;
    assert!(
        terminal
            .get_viewport()
            .iter()
            .any(|line| line.contains("Clipboard unavailable: install wl-clipboard"))
    );
    tui.stop(None);
}

#[tokio::test]
async fn does_not_append_whitespace_to_double_click_word_highlighting() {
    let terminal = RecordingTerminal::new(20, 1);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(Some("foo  bar"), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<0;1;1m");
    terminal.send_input("\x1b[<0;3;1M");
    terminal.wait_for_render().await;

    assert!(terminal.all_writes_joined().contains("foo\x1b[27m"));
    tui.stop(None);
}

#[tokio::test]
async fn coalesces_slash_and_hyphen_separated_segments_for_double_click_word_selection() {
    for (line, needle) in [
        ("extensions/starline/fixed-editor/compositor.ts", "starline"),
        ("earendil-works/pi-tui", "works"),
    ] {
        let copied: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let copied_ref = Arc::clone(&copied);
        let terminal = RecordingTerminal::new(80, 1);
        let tui = TuiAltScreen::new(
            Arc::new(terminal.clone()),
            None,
            None,
            Some(TuiAltScreenOptions {
                copy_selection: Some(Arc::new(move |text| {
                    let copied_ref = Arc::clone(&copied_ref);
                    Box::pin(async move {
                        copied_ref.lock().expect("copied lock").push(text);
                        CopySelectionResult::Bool(true)
                    })
                })),
                ..default_tui_options()
            }),
        );
        let mut guard = StopGuard::new(tui.clone());
        tui.add_child(Arc::new(Text::new(Some(line), Some(0), Some(0), None)));
        tui.start();
        terminal.wait_for_render().await;

        let one_based_click_column = cell_col_of(line, needle).expect("needle in line") + 1;
        terminal.send_input(&format!("\x1b[<0;{one_based_click_column};1M"));
        terminal.send_input(&format!("\x1b[<0;{one_based_click_column};1m"));
        terminal.send_input(&format!("\x1b[<0;{one_based_click_column};1M"));
        terminal.send_input(&format!("\x1b[<0;{one_based_click_column};1m"));
        terminal.wait_for_render().await;

        assert_eq!(*copied.lock().expect("copied lock"), vec![line.to_string()]);
        guard.stop_now();
    }
}

#[tokio::test]
async fn highlights_a_complete_whitespace_segment_during_a_word_drag() {
    let terminal = RecordingTerminal::new(20, 1);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(Some("foo  bar"), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<0;1;1m");
    terminal.send_input("\x1b[<0;2;1M");
    terminal.send_input("\x1b[<32;4;1M");
    terminal.wait_for_render().await;

    assert!(terminal.all_writes_joined().contains("foo  \x1b[27m"));
    tui.stop(None);
}

#[tokio::test]
async fn selects_whole_words_on_double_click_extends_word_drags_and_selects_lines_on_triple_click() {
    let terminal = RecordingTerminal::new(20, 2);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("zero alpha beta\ngamma delta"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    // The second click lands on a different character in alpha.
    terminal.send_input("\x1b[<0;6;1M");
    terminal.send_input("\x1b[<0;6;1m");
    terminal.send_input("\x1b[<0;10;1M");
    terminal.send_input("\x1b[<0;10;1m");
    terminal.wait_for_render().await;
    let alpha = format!("\x1b]52;c;{}\x07", b64::encode(b"alpha"));
    assert!(terminal.all_writes_joined().contains(&alpha));

    // A double-click drag includes each word touched, rather than partial words.
    terminal.send_input("\x1b[<0;12;1M");
    terminal.send_input("\x1b[<0;12;1m");
    terminal.send_input("\x1b[<0;14;1M");
    terminal.send_input("\x1b[<32;3;2M");
    terminal.send_input("\x1b[<0;3;2m");
    terminal.wait_for_render().await;
    let words = format!("\x1b]52;c;{}\x07", b64::encode(b"beta\ngamma"));
    assert!(terminal.all_writes_joined().contains(&words));

    terminal.send_input("\x1b[<0;7;2M");
    terminal.send_input("\x1b[<0;7;2m");
    terminal.send_input("\x1b[<0;9;2M");
    terminal.send_input("\x1b[<0;9;2m");
    terminal.send_input("\x1b[<0;11;2M");
    terminal.send_input("\x1b[<0;11;2m");
    terminal.wait_for_render().await;
    let line = format!("\x1b]52;c;{}\x07", b64::encode(b"gamma delta"));
    assert!(terminal.all_writes_joined().contains(&line));

    tui.stop(None);
}

#[tokio::test]
async fn does_not_repaint_idle_or_zero_width_selections_on_focus_loss() {
    let terminal = RecordingTerminal::new(20, 4);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let mut guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("alpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    let clipboard_write_count = || terminal.writes().iter().filter(|w| w.contains("\x1b]52;c;")).count();

    let idle_write_count = terminal.write_count();
    terminal.send_input("\x1b[O");
    terminal.send_input("\x1b[I");
    terminal.wait_for_render().await;
    assert_eq!(terminal.write_count(), idle_write_count);

    // A completed click leaves a zero-width anchor, but later orphaned drag/release events must not extend it.
    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<0;1;1m");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;
    assert_eq!(clipboard_write_count(), 0);

    // Losing focus after a press without a drag cancels the press without repainting.
    terminal.send_input("\x1b[<0;1;3M");
    terminal.wait_for_render().await;
    let pressed_write_count = terminal.write_count();
    terminal.send_input("\x1b[O");
    terminal.send_input("\x1b[I");
    terminal.wait_for_render().await;
    assert_eq!(terminal.write_count(), pressed_write_count);
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;
    assert_eq!(clipboard_write_count(), 0);
    assert!(terminal.all_writes_joined().contains("\x1b[?1004h"));

    guard.stop_now();
    assert!(terminal.all_writes_joined().contains("\x1b[?1004l"));
}

#[tokio::test]
async fn clears_an_active_visible_selection_on_focus_loss_and_ignores_orphan_events() {
    let terminal = RecordingTerminal::new(20, 4);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("alpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.wait_for_render().await;
    let focus_loss_event_count = terminal.event_count();
    terminal.send_input("\x1b[O");
    terminal.send_input("\x1b[I");
    terminal.wait_for_render().await;
    let focus_loss_writes = terminal.writes_since(focus_loss_event_count);
    assert!(focus_loss_writes.contains("alpha"));
    assert!(focus_loss_writes.contains("beta"));
    assert!(!focus_loss_writes.contains("\x1b[7m"));

    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;
    assert!(!terminal.all_writes_joined().contains("\x1b]52;c;"));
    tui.stop(None);
}

#[tokio::test]
async fn retains_a_completed_visible_selection_across_focus_changes() {
    let terminal = RecordingTerminal::new(20, 4);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("alpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;
    let completed_write_count = terminal.write_count();
    terminal.send_input("\x1b[O");
    terminal.send_input("\x1b[I");
    terminal.wait_for_render().await;
    assert_eq!(terminal.write_count(), completed_write_count);

    let redraw_event_count = terminal.event_count();
    tui.render_now(Some(true));
    let redraw_writes = terminal.writes_since(redraw_event_count);
    assert!(redraw_writes.contains("alpha"));
    assert!(redraw_writes.contains("beta"));
    assert!(redraw_writes.contains("\x1b[7m"));
    tui.stop(None);
}

#[tokio::test]
async fn stacks_flash_messages_and_collapses_them_as_they_expire() {
    let terminal = VirtualTerminal::new(20, 4);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("one\ntwo\nthree\nfour"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;

    tui.flash("First", Some(80));
    tui.flash("Second", Some(500));
    terminal.wait_for_render().await;
    let mut viewport = terminal.get_viewport();
    assert!(viewport[0].ends_with(" First "));
    assert!(viewport[1].ends_with(" Second "));

    tokio::time::sleep(Duration::from_millis(100)).await;
    terminal.wait_for_render().await;
    viewport = terminal.get_viewport();
    assert!(viewport[0].ends_with(" Second "));
    assert!(!viewport.iter().any(|line| line.contains("First")));

    tui.stop(None);
}

#[tokio::test]
async fn auto_scrolls_and_extends_a_drag_selection_held_at_the_viewport_edge() {
    let terminal = RecordingTerminal::new(20, 4);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(Some(&line_items(10)), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 6);

    terminal.send_input("\x1b[<0;1;3M");
    terminal.send_input("\x1b[<32;1;1M");
    tokio::time::sleep(Duration::from_millis(130)).await;
    terminal.wait_for_render().await;

    let selection_top = tui.viewport_top();
    assert!(
        selection_top < 6,
        "expected auto-scroll above row 6, got {selection_top}"
    );
    terminal.send_input("\x1b[<0;1;1m");
    terminal.wait_for_render().await;

    let mut selected_lines: Vec<String> = (selection_top..8).map(|idx| format!("line {}", idx + 1)).collect();
    selected_lines.push("l".to_string());
    let expected_clipboard_sequence = format!("\x1b]52;c;{}\x07", b64::encode(selected_lines.join("\n").as_bytes()));
    assert!(
        terminal.all_writes_joined().contains(&expected_clipboard_sequence),
        "clipboard writes: {:?}",
        terminal
            .writes()
            .into_iter()
            .filter(|w| w.contains("\x1b]52;c;"))
            .collect::<Vec<_>>()
    );
    tui.stop(None);
}

#[tokio::test]
async fn snaps_mouse_selection_to_cjk_emoji_and_combining_grapheme_boundaries() {
    let terminal = RecordingTerminal::new(20, 2);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(Some("A界🙂e\u{0301}Z"), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;

    let wide_selection = format!("\x1b]52;c;{}\x07", b64::encode("界🙂".as_bytes()));
    terminal.send_input("\x1b[<0;3;1M");
    terminal.send_input("\x1b[<32;4;1M");
    terminal.send_input("\x1b[<0;4;1m");
    terminal.wait_for_render().await;
    assert_eq!(
        terminal.writes().iter().filter(|w| w.contains(&wide_selection)).count(),
        1
    );

    terminal.send_input("\x1b[<0;5;1M");
    terminal.send_input("\x1b[<32;2;1M");
    terminal.send_input("\x1b[<0;2;1m");
    terminal.wait_for_render().await;
    assert_eq!(
        terminal.writes().iter().filter(|w| w.contains(&wide_selection)).count(),
        2
    );

    let combining_selection = format!("\x1b]52;c;{}\x07", b64::encode("e\u{0301}Z".as_bytes()));
    terminal.send_input("\x1b[<0;6;1M");
    terminal.send_input("\x1b[<32;7;1M");
    terminal.send_input("\x1b[<0;7;1m");
    terminal.wait_for_render().await;
    assert!(terminal.all_writes_joined().contains(&combining_selection));

    tui.stop(None);
}

#[tokio::test]
async fn ignores_horizontal_trackpad_wheel_events() {
    let terminal = VirtualTerminal::new(20, 4);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(Some(&line_items(8)), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<66;1;1M");
    terminal.send_input("\x1b[<67;1;1M");
    terminal.wait_for_render().await;
    assert_eq!(tui.viewport_top(), 4);
    assert_eq!(
        trimmed_viewport(&terminal),
        vec!["line 5", "line 6", "line 7", "line 8"]
    );

    tui.stop(None);
}

#[tokio::test]
async fn dispatches_clicks_to_nested_mouse_regions_without_breaking_drag_selection() {
    let terminal = RecordingTerminal::new(20, 2);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let clicks = Arc::new(AtomicI64::new(0));
    let clicks_ref = Arc::clone(&clicks);
    tui.add_child(Arc::new(MouseRegion::new(
        Arc::new(Text::new(Some("clickable\nselectable"), Some(0), Some(0), None)),
        Arc::new(move |event| {
            if event.r#type != TuiMouseEventType::Click {
                return None;
            }
            clicks_ref.fetch_add(1, Ordering::SeqCst);
            Some(TuiMouseEventResult {
                handled: Some(true),
                ..Default::default()
            })
        }),
    )));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;2;1M");
    terminal.send_input("\x1b[<0;2;1m");
    terminal.wait_for_render().await;
    assert_eq!(clicks.load(Ordering::SeqCst), 1);

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;
    assert_eq!(clicks.load(Ordering::SeqCst), 1);
    assert!(terminal.all_writes_joined().contains("\x1b]52;c;"));
    tui.stop(None);
}

#[tokio::test]
async fn focuses_and_captures_drag_gestures_for_mouse_aware_components() {
    let terminal = VirtualTerminal::new(20, 2);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let events_ref = Arc::clone(&events);
    let component: Arc<dyn Component> = Arc::new(MouseHandlerComponent::new(
        |_| vec!["control".to_string()],
        move |event| {
            let name = match event.r#type {
                TuiMouseEventType::Press => "press",
                TuiMouseEventType::Click => "click",
                TuiMouseEventType::Drag => "drag",
                TuiMouseEventType::Release => "release",
                TuiMouseEventType::Move => "move",
                TuiMouseEventType::Wheel => "wheel",
            };
            events_ref.lock().expect("events lock").push(name.to_string());
            if event.r#type == TuiMouseEventType::Press {
                Some(TuiMouseEventResult {
                    handled: Some(true),
                    capture: Some(true),
                    focus: Some(true),
                    ..Default::default()
                })
            } else {
                Some(TuiMouseEventResult {
                    handled: Some(true),
                    ..Default::default()
                })
            }
        },
    ));
    tui.add_child(Arc::clone(&component));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;5;2M");
    terminal.send_input("\x1b[<0;5;2m");
    terminal.wait_for_render().await;

    assert_eq!(
        *events.lock().expect("events lock"),
        vec!["press".to_string(), "drag".to_string(), "release".to_string()]
    );
    let focused = tui.get_focused_component().expect("focused component");
    assert!(Arc::ptr_eq(&focused, &component));
    tui.stop(None);
}

#[tokio::test]
async fn reports_consecutive_click_counts_to_component_owned_controls() {
    let terminal = VirtualTerminal::new(20, 1);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let click_counts: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let click_counts_ref = Arc::clone(&click_counts);
    tui.add_child(Arc::new(MouseHandlerComponent::new(
        |_| vec!["control".to_string()],
        move |event| {
            if event.r#type == TuiMouseEventType::Press {
                return Some(TuiMouseEventResult {
                    handled: Some(true),
                    ..Default::default()
                });
            }
            if event.r#type == TuiMouseEventType::Click {
                click_counts_ref
                    .lock()
                    .expect("click counts lock")
                    .push(event.click_count.unwrap_or(0));
                return Some(TuiMouseEventResult {
                    handled: Some(true),
                    ..Default::default()
                });
            }
            None
        },
    )));
    tui.start();
    terminal.wait_for_render().await;

    for _ in 0..3 {
        terminal.send_input("\x1b[<0;1;1M");
        terminal.send_input("\x1b[<0;1;1m");
    }
    terminal.wait_for_render().await;
    assert_eq!(*click_counts.lock().expect("click counts lock"), vec![1, 2, 3]);
    tui.stop(None);
}

#[tokio::test]
async fn does_not_rerender_for_handled_no_op_pointer_motion() {
    let terminal = RecordingTerminal::new(20, 2);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let render_count = Arc::new(AtomicI64::new(0));
    let render_count_ref = Arc::clone(&render_count);
    tui.add_child(Arc::new(MouseHandlerComponent::new(
        move |_| {
            render_count_ref.fetch_add(1, Ordering::SeqCst);
            vec!["hover target".to_string()]
        },
        |event| {
            if event.r#type == TuiMouseEventType::Move {
                Some(TuiMouseEventResult {
                    handled: Some(true),
                    ..Default::default()
                })
            } else {
                None
            }
        },
    )));
    tui.start();
    terminal.wait_for_render().await;
    let rendered_before_motion = render_count.load(Ordering::SeqCst);
    let writes_before_motion = terminal.write_count();

    terminal.send_input("\x1b[<35;1;1M");
    terminal.wait_for_render().await;
    assert_eq!(render_count.load(Ordering::SeqCst), rendered_before_motion);
    assert_eq!(terminal.write_count(), writes_before_motion);
    tui.stop(None);
}

#[tokio::test]
async fn lets_mouse_aware_components_consume_wheel_events_before_viewport_scrolling() {
    let terminal = VirtualTerminal::new(20, 3);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let wheel_events = Arc::new(AtomicI64::new(0));
    let wheel_events_ref = Arc::clone(&wheel_events);
    tui.add_child(Arc::new(MouseRegion::new(
        Arc::new(Text::new(Some(&line_items(8)), Some(0), Some(0), None)),
        Arc::new(move |event| {
            if event.r#type != TuiMouseEventType::Wheel {
                return None;
            }
            wheel_events_ref.fetch_add(1, Ordering::SeqCst);
            Some(TuiMouseEventResult {
                handled: Some(true),
                ..Default::default()
            })
        }),
    )));
    tui.start();
    terminal.wait_for_render().await;
    let viewport_top = tui.viewport_top();

    terminal.send_input("\x1b[<64;1;1M");
    terminal.wait_for_render().await;
    assert_eq!(wheel_events.load(Ordering::SeqCst), 1);
    assert_eq!(tui.viewport_top(), viewport_top);
    tui.stop(None);
}

#[tokio::test]
async fn restores_keyboard_state_before_leaving_alt_mode_and_prints_the_full_document() {
    let terminal = RecordingTerminal::new(20, 3);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let mut guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(
        Some("first\nsecond\nthird\nfourth\nfifth\nsixth"),
        Some(0),
        Some(0),
        None,
    )));
    tui.start();
    terminal.wait_for_render().await;
    guard.stop_now();

    let events = terminal.events();
    let start_index = events
        .iter()
        .position(|event| matches!(event, RecordingEvent::Start))
        .expect("start index");
    let alt_screen_enter_index = events
        .iter()
        .position(|event| matches!(event, RecordingEvent::Write(data) if data.contains("\x1b[?1049h")))
        .expect("alt screen enter index");
    let stop_index = events
        .iter()
        .position(|event| matches!(event, RecordingEvent::Stop))
        .expect("stop index");
    let mouse_disable_index = events
        .iter()
        .position(|event| matches!(event, RecordingEvent::Write(data) if data.contains("\x1b[?1006l")))
        .expect("mouse disable index");
    let main_screen_restore_index = events
        .iter()
        .position(|event| matches!(event, RecordingEvent::Write(data) if data.contains("\x1b[?1049l")))
        .expect("main screen restore index");
    assert!(alt_screen_enter_index < start_index);
    assert!(mouse_disable_index < stop_index);
    assert!(main_screen_restore_index > stop_index);

    match &events[main_screen_restore_index] {
        RecordingEvent::Write(data) => {
            assert!(data.contains("first"));
            assert!(data.contains("second"));
            assert!(data.contains("third"));
            assert!(data.contains("fourth"));
            assert!(data.contains("fifth"));
            assert!(data.contains("sixth"));
            assert!(data.find("first").expect("first") < data.find("sixth").expect("sixth"));
        }
        other => panic!("expected Write event, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn gives_wheel_and_viewport_keys_to_a_focused_overlay() {
    let terminal = VirtualTerminal::new(20, 6);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(Some(&line_items(12)), Some(0), Some(0), None)));
    let overlay = Arc::new(InputOverlay::new());
    tui.start();
    terminal.wait_for_render().await;
    let top_before = tui.viewport_top();
    let handle = tui.show_overlay(Arc::clone(&overlay) as Arc<dyn Component>, None);
    terminal.wait_for_render().await;
    assert!(overlay.focused());

    let wheel = "\x1b[<64;10;3M";
    let keys = vec![
        "\x1b[5~".to_string(),
        "\x1b[6~".to_string(),
        "\x1bOH".to_string(),
        "\x1bOF".to_string(),
        wheel.to_string(),
    ];
    for key in &keys {
        terminal.send_input(key);
    }
    terminal.wait_for_render().await;

    assert_eq!(overlay.inputs(), keys);
    assert_eq!(tui.viewport_top(), top_before);

    handle.hide();
    terminal.wait_for_render().await;
    terminal.send_input("\x1b[5~");
    terminal.wait_for_render().await;
    assert!(tui.viewport_top() < top_before);
    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn keeps_viewport_scrolling_when_an_overlay_is_not_focused() {
    let terminal = VirtualTerminal::new(20, 6);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    let editor = Arc::new(InputOverlay::new());
    tui.add_child(Arc::new(Text::new(Some(&line_items(12)), Some(0), Some(0), None)));
    tui.set_focus(Some(Arc::clone(&editor) as Arc<dyn Component>));
    tui.start();
    terminal.wait_for_render().await;
    let top_before = tui.viewport_top();

    let hidden = tui.show_overlay(Arc::new(InputOverlay::new()), None);
    hidden.set_hidden(true);
    let non_capturing = Arc::new(InputOverlay::new());
    tui.show_overlay(
        Arc::clone(&non_capturing) as Arc<dyn Component>,
        Some(OverlayOptions {
            non_capturing: Some(true),
            ..Default::default()
        }),
    );
    let unfocused = Arc::new(InputOverlay::new());
    let unfocused_handle = tui.show_overlay(Arc::clone(&unfocused) as Arc<dyn Component>, None);
    unfocused_handle.unfocus(None);
    terminal.wait_for_render().await;
    assert!(!non_capturing.focused());
    assert!(!unfocused.focused());

    terminal.send_input("\x1b[5~");
    terminal.send_input("\x1b[<64;10;3M");
    terminal.wait_for_render().await;
    assert!(tui.viewport_top() < top_before);
    assert!(non_capturing.inputs().is_empty());
    assert!(unfocused.inputs().is_empty());
    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn keeps_viewport_scrolling_while_transcript_search_is_focused() {
    let terminal = VirtualTerminal::new(20, 6);
    let tui = TuiAltScreen::new(Arc::new(terminal.clone()), None, None, None);
    let _guard = StopGuard::new(tui.clone());
    tui.add_child(Arc::new(Text::new(Some(&line_items(12)), Some(0), Some(0), None)));
    tui.start();
    terminal.wait_for_render().await;
    let top_before = tui.viewport_top();

    terminal.send_input("\x1b[102;6u");
    terminal.wait_for_render().await;
    assert!(terminal.get_viewport().iter().any(|line| line.contains("↑ ↓")));

    terminal.send_input("\x1b[5~");
    terminal.send_input("\x1b[<64;1;4M");
    terminal.wait_for_render().await;
    assert!(tui.viewport_top() < top_before);
    assert!(terminal.get_viewport().iter().any(|line| line.contains("↑ ↓")));
    tui.stop(None);
}

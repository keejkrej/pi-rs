//! Port of packages/tui/src/tui-main-screen.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexSet;

use crate::terminal::Terminal;
use crate::terminal_colors::{TerminalColorScheme, TerminalColors};
use crate::tui::{
    Component, CursorPosition, OverlayHandle, OverlayOptions, QueryTerminalColorsOptions, TUI, TuiBase,
    TuiBaseOverrides, TuiInputListener, TuiMode, TuiMouseEvent, TuiMouseEventResult, TuiStopOptions,
};

const KITTY_SEQUENCE_PREFIX: &str = "\x1b_G";
const MAX_RENDER_WRITE_CHARS: i64 = 1024 * 1024;

/// Streams terminal output in 1 MiB chunks so a full render never forms one string large enough to exceed V8's limit.
///
/// `append()` fills the current chunk and flushes it when full. Oversized input is split at chunk boundaries, preserving
/// surrogate pairs so each write remains valid UTF-16. Callers append synchronized-output begin/end sequences themselves;
/// the final `flush()` writes any remainder, including the end sequence.
struct BoundedTerminalWriter {
    buffer: String,
    written_chars: i64,
    write: Box<dyn Fn(&str) + Send + Sync>,
}

impl BoundedTerminalWriter {
    fn new(write: Box<dyn Fn(&str) + Send + Sync>) -> Self {
        todo!("port: BoundedTerminalWriter::new")
    }

    /// Append terminal data, flushing full chunks as needed. Callers must call `flush()` after the final append.
    fn append(&mut self, value: &str) {
        todo!("port: BoundedTerminalWriter::append")
    }

    /// Write the current chunk, if any, and retain only its character count for debug output.
    fn flush(&mut self) {
        todo!("port: BoundedTerminalWriter::flush")
    }

    fn length(&self) -> i64 {
        todo!("port: BoundedTerminalWriter::length")
    }
}

struct KittyImageHeader {
    ids: Vec<i64>,
    rows: i64,
}

fn parse_kitty_image_header(line: &str) -> Option<KittyImageHeader> {
    todo!("port: parse_kitty_image_header")
}

fn extract_kitty_image_ids(line: &str) -> Vec<i64> {
    todo!("port: extract_kitty_image_ids")
}

fn extract_kitty_image_rows(line: &str) -> i64 {
    todo!("port: extract_kitty_image_rows")
}

fn is_termux_session() -> bool {
    todo!("port: is_termux_session")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TuiMainScreenRenderState {
    pub previous_lines: Vec<String>,
    pub previous_width: i64,
    pub previous_height: i64,
    pub cursor_row: i64,
    pub hardware_cursor_row: i64,
    pub max_lines_rendered: i64,
    pub previous_viewport_top: i64,
}

struct TuiMainScreenState {
    previous_lines: Vec<String>,
    previous_kitty_image_ids: IndexSet<i64>,
    previous_width: i64,
    previous_height: i64,
    cursor_row: i64,
    hardware_cursor_row: i64,
    max_lines_rendered: i64,
    previous_viewport_top: i64,
}

struct ChangedRange {
    first_changed: i64,
    last_changed: i64,
}

struct TuiMainScreenInner {
    base: TuiBase,
    state: Mutex<TuiMainScreenState>,
}

/// TUI implementation that renders into the terminal's main screen and scrollback.
///
/// PORT: `extends TuiBase` is a composed [`TuiBase`]. After [`TuiMainScreen::new`], call
/// [`TuiBase::bind_overrides`] with this screen (`Weak` inside the base breaks the cycle).
/// `mode` is `"regular"`.
#[derive(Clone)]
pub struct TuiMainScreen {
    inner: Arc<TuiMainScreenInner>,
}

impl TuiMainScreen {
    pub fn new(terminal: Arc<dyn Terminal>, show_hardware_cursor: Option<bool>, log_directory: Option<&str>) -> Self {
        todo!("port: TuiMainScreen::new")
    }

    fn base(&self) -> &TuiBase {
        &self.inner.base
    }

    pub fn mode(&self) -> TuiMode {
        TuiMode::Regular
    }

    pub fn capture_render_state(&self) -> TuiMainScreenRenderState {
        todo!("port: TuiMainScreen::capture_render_state")
    }

    pub fn restore_render_state(&self, state: TuiMainScreenRenderState) {
        todo!("port: TuiMainScreen::restore_render_state")
    }

    pub fn children(&self) -> Vec<Arc<dyn Component>> {
        self.base().children()
    }

    pub fn set_children(&self, children: Vec<Arc<dyn Component>>) {
        self.base().set_children(children);
    }

    pub fn terminal(&self) -> Arc<dyn Terminal> {
        self.base().terminal()
    }

    pub fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.base().on_debug()
    }

    pub fn set_on_debug(&self, on_debug: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.base().set_on_debug(on_debug);
    }

    pub fn full_redraws(&self) -> i64 {
        self.base().full_redraws()
    }

    pub fn add_child(&self, component: Arc<dyn Component>) {
        self.base().add_child(component);
    }

    pub fn remove_child(&self, component: &Arc<dyn Component>) {
        self.base().remove_child(component);
    }

    pub fn clear(&self) {
        self.base().clear();
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.base().render(width)
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.base().handle_mouse(event)
    }

    pub fn invalidate(&self) {
        self.base().invalidate();
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

    pub fn get_focused_component(&self) -> Option<Arc<dyn Component>> {
        self.base().get_focused_component()
    }

    pub fn set_focus(&self, component: Option<Arc<dyn Component>>) {
        self.base().set_focus(component);
    }

    pub fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
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

    pub fn stop(&self, options: Option<TuiStopOptions>) {
        self.base().stop(options);
    }

    pub fn render_now(&self, force: Option<bool>) {
        self.base().render_now(force);
    }

    pub fn request_render(&self, force: Option<bool>) {
        self.base().request_render(force);
    }

    pub fn add_input_listener(&self, listener: TuiInputListener) -> pi_js::Unsubscribe {
        self.base().add_input_listener(listener)
    }

    pub fn remove_input_listener(&self, listener: &TuiInputListener) {
        self.base().remove_input_listener(listener);
    }

    pub fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        self.base().on_terminal_color_scheme_change(listener)
    }

    pub fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        self.base().set_terminal_color_scheme_notifications(enabled);
    }

    pub async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        self.base().query_terminal_colors(options).await
    }

    fn collect_kitty_image_ids(&self, lines: &[String]) -> IndexSet<i64> {
        todo!("port: TuiMainScreen::collect_kitty_image_ids")
    }

    fn delete_kitty_images(&self, ids: &IndexSet<i64>) -> String {
        todo!("port: TuiMainScreen::delete_kitty_images")
    }

    fn get_kitty_image_reserved_rows(&self, lines: &[String], index: i64, max_index: Option<i64>) -> i64 {
        todo!("port: TuiMainScreen::get_kitty_image_reserved_rows")
    }

    fn expand_changed_range_for_kitty_images(
        &self,
        first_changed: i64,
        last_changed: i64,
        new_lines: &[String],
    ) -> ChangedRange {
        todo!("port: TuiMainScreen::expand_changed_range_for_kitty_images")
    }

    fn delete_changed_kitty_images(&self, first_changed: i64, last_changed: i64) -> String {
        todo!("port: TuiMainScreen::delete_changed_kitty_images")
    }

    /// Position the hardware cursor for IME candidate window.
    fn position_hardware_cursor(&self, cursor_pos: Option<CursorPosition>, total_lines: i64) {
        todo!("port: TuiMainScreen::position_hardware_cursor")
    }
}

impl TuiBaseOverrides for TuiMainScreen {
    fn mode(&self) -> TuiMode {
        TuiMainScreen::mode(self)
    }

    fn do_render(&self) {
        todo!("port: TuiMainScreen::do_render")
    }

    fn reset_render_state(&self) {
        todo!("port: TuiMainScreen::reset_render_state")
    }

    fn before_terminal_stop(&self, options: &TuiStopOptions) {
        todo!("port: TuiMainScreen::before_terminal_stop")
    }
}

impl Component for TuiMainScreen {
    fn render(&self, width: usize) -> Vec<String> {
        TuiMainScreen::render(self, width)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        TuiMainScreen::handle_mouse(self, event)
    }

    fn invalidate(&self) {
        TuiMainScreen::invalidate(self);
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.children())
    }
}

#[async_trait]
impl TUI for TuiMainScreen {
    fn mode(&self) -> TuiMode {
        TuiMainScreen::mode(self)
    }

    fn children(&self) -> Vec<Arc<dyn Component>> {
        TuiMainScreen::children(self)
    }

    fn set_children(&self, children: Vec<Arc<dyn Component>>) {
        TuiMainScreen::set_children(self, children);
    }

    fn terminal(&self) -> Arc<dyn Terminal> {
        TuiMainScreen::terminal(self)
    }

    fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        TuiMainScreen::on_debug(self)
    }

    fn set_on_debug(&self, on_debug: Option<Arc<dyn Fn() + Send + Sync>>) {
        TuiMainScreen::set_on_debug(self, on_debug);
    }

    fn full_redraws(&self) -> i64 {
        TuiMainScreen::full_redraws(self)
    }

    fn add_child(&self, component: Arc<dyn Component>) {
        TuiMainScreen::add_child(self, component);
    }

    fn remove_child(&self, component: &Arc<dyn Component>) {
        TuiMainScreen::remove_child(self, component);
    }

    fn clear(&self) {
        TuiMainScreen::clear(self);
    }

    fn get_show_hardware_cursor(&self) -> bool {
        TuiMainScreen::get_show_hardware_cursor(self)
    }

    fn set_show_hardware_cursor(&self, enabled: bool) {
        TuiMainScreen::set_show_hardware_cursor(self, enabled);
    }

    fn get_clear_on_shrink(&self) -> bool {
        TuiMainScreen::get_clear_on_shrink(self)
    }

    fn set_clear_on_shrink(&self, enabled: bool) {
        TuiMainScreen::set_clear_on_shrink(self, enabled);
    }

    fn set_focus(&self, component: Option<Arc<dyn Component>>) {
        TuiMainScreen::set_focus(self, component);
    }

    fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
        TuiMainScreen::show_overlay(self, component, options)
    }

    fn hide_overlay(&self) {
        TuiMainScreen::hide_overlay(self);
    }

    fn has_overlay(&self) -> bool {
        TuiMainScreen::has_overlay(self)
    }

    fn start(&self) {
        TuiMainScreen::start(self);
    }

    fn stop(&self, options: Option<TuiStopOptions>) {
        TuiMainScreen::stop(self, options);
    }

    fn render_now(&self, force: Option<bool>) {
        TuiMainScreen::render_now(self, force);
    }

    fn request_render(&self, force: Option<bool>) {
        TuiMainScreen::request_render(self, force);
    }

    fn add_input_listener(&self, listener: TuiInputListener) -> pi_js::Unsubscribe {
        TuiMainScreen::add_input_listener(self, listener)
    }

    fn remove_input_listener(&self, listener: &TuiInputListener) {
        TuiMainScreen::remove_input_listener(self, listener);
    }

    fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        TuiMainScreen::on_terminal_color_scheme_change(self, listener)
    }

    fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        TuiMainScreen::set_terminal_color_scheme_notifications(self, enabled);
    }

    async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        TuiMainScreen::query_terminal_colors(self, options).await
    }
}

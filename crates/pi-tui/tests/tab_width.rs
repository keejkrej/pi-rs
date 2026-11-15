//! Port of packages/tui/test/tab-width.test.ts

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::tui::{Component, OverlayOptions, SizeValue};
use pi_tui::tui_main_screen::TuiMainScreen;
use pi_tui::utils::{extract_segments, normalize_terminal_output, slice_with_width, visible_width};

use support::virtual_terminal::VirtualTerminal;

struct CapturingTerminal {
    inner: VirtualTerminal,
    writes: Mutex<Vec<String>>,
}

impl CapturingTerminal {
    fn new(columns: i64, rows: i64) -> Self {
        Self {
            inner: VirtualTerminal::new(columns, rows),
            writes: Mutex::new(Vec::new()),
        }
    }

    fn raw_output(&self) -> String {
        self.writes.lock().unwrap().concat()
    }

    fn clear_writes(&self) {
        self.writes.lock().unwrap().clear();
    }

    fn get_viewport(&self) -> Vec<String> {
        self.inner.get_viewport()
    }

    async fn wait_for_render(&self) {
        self.inner.wait_for_render().await;
    }
}

#[async_trait]
impl Terminal for CapturingTerminal {
    fn start(&self, on_input: TerminalInputHandler, on_resize: TerminalResizeHandler) {
        self.inner.start(on_input, on_resize);
    }

    fn stop(&self) {
        self.inner.stop();
    }

    async fn drain_input(&self, max_ms: Option<i64>, idle_ms: Option<i64>) {
        self.inner.drain_input(max_ms, idle_ms).await;
    }

    fn write(&self, data: &str) {
        self.writes.lock().unwrap().push(data.to_string());
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

struct FullViewportContent;

impl Component for FullViewportContent {
    fn render(&self, width: usize) -> Vec<String> {
        (0..3)
            .map(|i| {
                let text = format!("base {i}");
                format!("{text}{}", " ".repeat(width.saturating_sub(text.len())))
            })
            .collect()
    }

    fn invalidate(&self) {}
}

struct TabStatusOverlay;

impl Component for TabStatusOverlay {
    fn render(&self, _width: usize) -> Vec<String> {
        vec!["\tX".to_string()]
    }

    fn invalidate(&self) {}
}

#[test]
fn slice_with_width_excludes_tabs_that_cross_the_slice_boundary_when_strict_is_enabled() {
    let line = "out 192M\t.pi/skill-tests/results-ha";
    let slice = slice_with_width(line, 0, 10, Some(true));

    assert_eq!(slice.text, "out 192M");
    assert_eq!(slice.width, 8);
    assert_eq!(visible_width(&slice.text), slice.width);
}

#[test]
fn extract_segments_stops_before_a_tab_that_crosses_before_end() {
    let line = "out 192M\t.pi/skill-tests/results-ha";
    let cut = extract_segments(line, 10, 13, 10, Some(true));

    assert_eq!(cut.before, "out 192M");
    assert_eq!(cut.before_width, 8);

    let full = extract_segments(line, 11, 13, 10, Some(true));
    assert_eq!(full.before, "out 192M\t");
    assert_eq!(full.before_width, 11);
}

#[test]
fn normalize_terminal_output_keeps_tabs_inside_escape_payloads_untouched_while_expanding_visible_tabs() {
    let osc8 = "\x1b]8;;https://example.test/a\tb\x07";
    let osc0 = "\x1b]0;window\ttitle\x1b\\";
    let apc = "\x1b_payload\tdata\x1b\\";
    let line = format!("{osc8}{osc0}{apc}label\ttext");

    assert_eq!(
        normalize_terminal_output(&line),
        format!("{osc8}{osc0}{apc}label   text")
    );
}

#[tokio::test]
async fn tui_normalizes_tabs_before_sending_overlay_lines_to_terminal() {
    let terminal = Arc::new(CapturingTerminal::new(16, 3));
    let tui = TuiMainScreen::new(terminal.clone(), None, None);

    tui.add_child(Arc::new(FullViewportContent));
    tui.show_overlay(
        Arc::new(TabStatusOverlay),
        Some(OverlayOptions {
            width: Some(SizeValue::Absolute(4.0)),
            row: Some(SizeValue::Absolute(1.0)),
            col: Some(SizeValue::Absolute(4.0)),
            ..Default::default()
        }),
    );

    tui.start();
    terminal.wait_for_render().await;
    terminal.clear_writes();
    tui.request_render(Some(true));
    terminal.wait_for_render().await;

    let viewport = terminal.get_viewport();
    assert_eq!(
        viewport,
        vec![
            "base 0          ".to_string(),
            "base   X        ".to_string(),
            "base 2          ".to_string(),
        ]
    );
    assert!(!terminal.raw_output().contains('\t'));

    tui.stop(None);
}

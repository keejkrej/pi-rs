//! Port of packages/tui/test/overlay-options.test.ts

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::tui::{Component, OverlayAnchor, OverlayMargin, OverlayMarginValue, OverlayOptions, SizeValue};
use pi_tui::tui_main_screen::TuiMainScreen;

use support::virtual_terminal::VirtualTerminal;

struct StaticOverlay {
    lines: Vec<String>,
    requested_width: Mutex<Option<usize>>,
}

impl StaticOverlay {
    fn new(lines: Vec<String>) -> Self {
        Self {
            lines,
            requested_width: Mutex::new(None),
        }
    }

    fn requested_width(&self) -> Option<usize> {
        *self.requested_width.lock().unwrap()
    }
}

impl Component for StaticOverlay {
    fn render(&self, width: usize) -> Vec<String> {
        *self.requested_width.lock().unwrap() = Some(width);
        self.lines.clone()
    }

    fn invalidate(&self) {}
}

struct CursorTrackingTerminal {
    inner: VirtualTerminal,
    cursor_visible: Mutex<bool>,
}

impl CursorTrackingTerminal {
    fn new(columns: i64, rows: i64) -> Self {
        Self {
            inner: VirtualTerminal::new(columns, rows),
            cursor_visible: Mutex::new(true),
        }
    }

    fn cursor_visible(&self) -> bool {
        *self.cursor_visible.lock().unwrap()
    }
}

#[async_trait]
impl Terminal for CursorTrackingTerminal {
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
        *self.cursor_visible.lock().unwrap() = false;
        self.inner.hide_cursor();
    }

    fn show_cursor(&self) {
        *self.cursor_visible.lock().unwrap() = true;
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

struct EmptyContent;

impl Component for EmptyContent {
    fn render(&self, _width: usize) -> Vec<String> {
        Vec::new()
    }

    fn invalidate(&self) {}
}

struct StyledContent;

impl Component for StyledContent {
    fn render(&self, width: usize) -> Vec<String> {
        let styled_line = format!("\x1b[1m\x1b[38;2;255;0;0m{}\x1b[0m", "X".repeat(width));
        vec![styled_line.clone(), styled_line.clone(), styled_line]
    }

    fn invalidate(&self) {}
}

struct HyperlinkContent;

impl Component for HyperlinkContent {
    fn render(&self, width: usize) -> Vec<String> {
        let link = "\x1b]8;;file:///path/to/file.ts\x07file.ts\x1b]8;;\x07";
        let line = format!("See {link} for details {}", "X".repeat(width.saturating_sub(30)));
        vec![line.clone(), line.clone(), line]
    }

    fn invalidate(&self) {}
}

async fn render_and_flush(tui: &TuiMainScreen, terminal: &VirtualTerminal) {
    tui.request_render(Some(true));
    tokio::task::yield_now().await;
    terminal.wait_for_render().await;
}

mod width_overflow_protection {
    use super::*;

    #[tokio::test]
    async fn should_truncate_overlay_lines_that_exceed_declared_width() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["X".repeat(100)]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                width: Some(SizeValue::Absolute(20.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert_eq!(viewport.len(), 24);
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_handle_overlay_with_complex_ansi_sequences_without_crashing() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let complex_line = format!(
            "\x1b[48;2;40;50;40m \x1b[38;2;128;128;128mSome styled content\x1b[39m\x1b[49m\x1b]8;;http://example.com\x07link\x1b]8;;\x07{}",
            " more content ".repeat(10)
        );
        let overlay = Arc::new(StaticOverlay::new(vec![
            complex_line.clone(),
            complex_line.clone(),
            complex_line,
        ]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                width: Some(SizeValue::Absolute(60.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(!viewport.is_empty());
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_handle_overlay_composited_on_styled_base_content() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["OVERLAY".to_string()]));

        tui.add_child(Arc::new(StyledContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                width: Some(SizeValue::Absolute(20.0)),
                anchor: Some(OverlayAnchor::Center),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(
            viewport.iter().any(|line| line.contains("OVERLAY")),
            "Overlay should be visible"
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_handle_wide_characters_at_overlay_boundary() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let wide_char_line = "中文日本語한글テスト漢字".to_string();
        let overlay = Arc::new(StaticOverlay::new(vec![wide_char_line]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                width: Some(SizeValue::Absolute(15.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(!viewport.is_empty());
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_handle_overlay_positioned_at_terminal_edge() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["X".repeat(50)]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                col: Some(SizeValue::Absolute(60.0)),
                width: Some(SizeValue::Absolute(20.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(!viewport.is_empty());
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_handle_overlay_on_base_content_with_osc_sequences() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["OVERLAY-TEXT".to_string()]));

        tui.add_child(Arc::new(HyperlinkContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::Center),
                width: Some(SizeValue::Absolute(20.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(!viewport.is_empty());
        tui.stop(None);
    }
}

mod width_percentage {
    use super::*;

    #[tokio::test]
    async fn should_render_overlay_at_percentage_of_terminal_width() {
        let terminal = Arc::new(VirtualTerminal::new(100, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["test".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                width: Some(SizeValue::Percent("50%".to_string())),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        assert_eq!(overlay.requested_width(), Some(50));
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_respect_min_width_when_width_percent_results_in_smaller_width() {
        let terminal = Arc::new(VirtualTerminal::new(100, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["test".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                width: Some(SizeValue::Percent("10%".to_string())),
                min_width: Some(30),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        assert_eq!(overlay.requested_width(), Some(30));
        tui.stop(None);
    }
}

mod anchor_positioning {
    use super::*;

    #[tokio::test]
    async fn should_position_overlay_at_top_left() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["TOP-LEFT".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopLeft),
                width: Some(SizeValue::Absolute(10.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(
            viewport[0].starts_with("TOP-LEFT"),
            "Expected TOP-LEFT at start, got: {}",
            viewport[0]
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_position_overlay_at_bottom_right() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["BTM-RIGHT".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::BottomRight),
                width: Some(SizeValue::Absolute(10.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        let last_row = &viewport[23];
        assert!(
            last_row.contains("BTM-RIGHT"),
            "Expected BTM-RIGHT on last row, got: {last_row}"
        );
        assert!(
            last_row.trim_end().ends_with("BTM-RIGHT"),
            "Expected BTM-RIGHT at end, got: {last_row}"
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_position_overlay_at_top_center() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["CENTERED".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopCenter),
                width: Some(SizeValue::Absolute(10.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        let first_row = &viewport[0];
        assert!(
            first_row.contains("CENTERED"),
            "Expected CENTERED on first row, got: {first_row}"
        );
        let col_index = first_row.find("CENTERED").unwrap();
        assert!((30..=40).contains(&col_index), "Expected centered, got col {col_index}");
        tui.stop(None);
    }
}

mod margin {
    use super::*;

    #[tokio::test]
    async fn should_clamp_negative_margins_to_zero() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["NEG-MARGIN".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopLeft),
                width: Some(SizeValue::Absolute(12.0)),
                margin: Some(OverlayMarginValue::Sides(OverlayMargin {
                    top: Some(-5),
                    left: Some(-10),
                    right: Some(0),
                    bottom: Some(0),
                })),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(
            viewport[0].starts_with("NEG-MARGIN"),
            "Expected NEG-MARGIN at start of row 0, got: {}",
            viewport[0]
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_respect_margin_as_number() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["MARGIN".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopLeft),
                width: Some(SizeValue::Absolute(10.0)),
                margin: Some(OverlayMarginValue::All(5)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(!viewport[0].contains("MARGIN"), "Should not be on row 0");
        assert!(!viewport[4].contains("MARGIN"), "Should not be on row 4");
        assert!(
            viewport[5].contains("MARGIN"),
            "Expected MARGIN on row 5, got: {}",
            viewport[5]
        );
        assert_eq!(viewport[5].find("MARGIN"), Some(5));
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_respect_margin_object() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["MARGIN".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopLeft),
                width: Some(SizeValue::Absolute(10.0)),
                margin: Some(OverlayMarginValue::Sides(OverlayMargin {
                    top: Some(2),
                    left: Some(3),
                    right: Some(0),
                    bottom: Some(0),
                })),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(
            viewport[2].contains("MARGIN"),
            "Expected MARGIN on row 2, got: {}",
            viewport[2]
        );
        assert_eq!(viewport[2].find("MARGIN"), Some(3));
        tui.stop(None);
    }
}

mod offset {
    use super::*;

    #[tokio::test]
    async fn should_apply_offset_x_and_offset_y_from_anchor_position() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["OFFSET".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopLeft),
                width: Some(SizeValue::Absolute(10.0)),
                offset_x: Some(10),
                offset_y: Some(5),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(
            viewport[5].contains("OFFSET"),
            "Expected OFFSET on row 5, got: {}",
            viewport[5]
        );
        assert_eq!(viewport[5].find("OFFSET"), Some(10));
        tui.stop(None);
    }
}

mod percentage_positioning {
    use super::*;

    #[tokio::test]
    async fn should_position_with_row_percent_and_col_percent() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["PCT".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                width: Some(SizeValue::Absolute(10.0)),
                row: Some(SizeValue::Percent("50%".to_string())),
                col: Some(SizeValue::Percent("50%".to_string())),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        let found_row = viewport.iter().position(|line| line.contains("PCT"));
        assert!(
            matches!(found_row, Some(r) if (10..=13).contains(&r)),
            "Expected centered row, got {found_row:?}"
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn row_percent_0_should_position_at_top() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["TOP".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                width: Some(SizeValue::Absolute(10.0)),
                row: Some(SizeValue::Percent("0%".to_string())),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(
            viewport[0].contains("TOP"),
            "Expected TOP on row 0, got: {}",
            viewport[0]
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn row_percent_100_should_position_at_bottom() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["BOTTOM".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                width: Some(SizeValue::Absolute(10.0)),
                row: Some(SizeValue::Percent("100%".to_string())),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(
            viewport[23].contains("BOTTOM"),
            "Expected BOTTOM on last row, got: {}",
            viewport[23]
        );
        tui.stop(None);
    }
}

mod max_height {
    use super::*;

    #[tokio::test]
    async fn should_truncate_overlay_to_max_height() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new((1..=5).map(|i| format!("Line {i}")).collect()));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                max_height: Some(SizeValue::Absolute(3.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let content = terminal.get_viewport().join("\n");
        assert!(content.contains("Line 1"));
        assert!(content.contains("Line 2"));
        assert!(content.contains("Line 3"));
        assert!(!content.contains("Line 4"));
        assert!(!content.contains("Line 5"));
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_truncate_overlay_to_max_height_percent() {
        let terminal = Arc::new(VirtualTerminal::new(80, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new((1..=10).map(|i| format!("L{i}")).collect()));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                max_height: Some(SizeValue::Percent("50%".to_string())),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let content = terminal.get_viewport().join("\n");
        assert!(content.contains("L1"));
        assert!(content.contains("L5"));
        assert!(!content.contains("L6"));
        tui.stop(None);
    }
}

mod absolute_positioning {
    use super::*;

    #[tokio::test]
    async fn row_and_col_should_override_anchor() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = Arc::new(StaticOverlay::new(vec!["ABSOLUTE".to_string()]));

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::BottomRight),
                row: Some(SizeValue::Absolute(3.0)),
                col: Some(SizeValue::Absolute(5.0)),
                width: Some(SizeValue::Absolute(10.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(
            viewport[3].contains("ABSOLUTE"),
            "Expected ABSOLUTE on row 3, got: {}",
            viewport[3]
        );
        assert_eq!(viewport[3].find("ABSOLUTE"), Some(5));
        tui.stop(None);
    }
}

mod stacked_overlays {
    use super::*;

    #[tokio::test]
    async fn should_render_multiple_overlays_with_later_ones_on_top() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            Arc::new(StaticOverlay::new(vec!["FIRST-OVERLAY".to_string()])),
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopLeft),
                width: Some(SizeValue::Absolute(20.0)),
                ..Default::default()
            }),
        );
        tui.show_overlay(
            Arc::new(StaticOverlay::new(vec!["SECOND".to_string()])),
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopLeft),
                width: Some(SizeValue::Absolute(10.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(
            viewport[0].contains("SECOND"),
            "Expected SECOND on row 0, got: {}",
            viewport[0]
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_handle_overlays_at_different_positions_without_interference() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            Arc::new(StaticOverlay::new(vec!["TOP-LEFT".to_string()])),
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopLeft),
                width: Some(SizeValue::Absolute(15.0)),
                ..Default::default()
            }),
        );
        tui.show_overlay(
            Arc::new(StaticOverlay::new(vec!["BTM-RIGHT".to_string()])),
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::BottomRight),
                width: Some(SizeValue::Absolute(15.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        let viewport = terminal.get_viewport();
        assert!(
            viewport[0].contains("TOP-LEFT"),
            "Expected TOP-LEFT on row 0, got: {}",
            viewport[0]
        );
        assert!(
            viewport[23].contains("BTM-RIGHT"),
            "Expected BTM-RIGHT on row 23, got: {}",
            viewport[23]
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn should_properly_hide_overlays_in_stack_order() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);

        tui.add_child(Arc::new(EmptyContent));
        tui.show_overlay(
            Arc::new(StaticOverlay::new(vec!["FIRST".to_string()])),
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopLeft),
                width: Some(SizeValue::Absolute(10.0)),
                ..Default::default()
            }),
        );
        tui.show_overlay(
            Arc::new(StaticOverlay::new(vec!["SECOND".to_string()])),
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopLeft),
                width: Some(SizeValue::Absolute(10.0)),
                ..Default::default()
            }),
        );
        tui.start();
        render_and_flush(&tui, &terminal).await;

        assert!(
            terminal.get_viewport()[0].contains("SECOND"),
            "SECOND should be visible initially"
        );

        tui.hide_overlay();
        render_and_flush(&tui, &terminal).await;

        assert!(
            terminal.get_viewport()[0].contains("FIRST"),
            "FIRST should be visible after hiding SECOND"
        );
        tui.stop(None);
    }
}

mod hiding_after_stop {
    use super::*;

    #[test]
    fn hide_overlay_leaves_the_cursor_visible() {
        let terminal = Arc::new(CursorTrackingTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        tui.start();
        tui.show_overlay(
            Arc::new(StaticOverlay::new(vec!["OVERLAY".to_string()])),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );

        tui.stop(None);
        tui.hide_overlay();

        assert!(terminal.cursor_visible());
    }

    #[test]
    fn overlay_handle_hide_leaves_the_cursor_visible() {
        let terminal = Arc::new(CursorTrackingTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        tui.start();
        let handle = tui.show_overlay(
            Arc::new(StaticOverlay::new(vec!["OVERLAY".to_string()])),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );

        tui.stop(None);
        handle.hide();

        assert!(terminal.cursor_visible());
    }
}

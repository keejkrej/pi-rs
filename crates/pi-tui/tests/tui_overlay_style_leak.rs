//! Port of packages/tui/test/tui-overlay-style-leak.test.ts

mod support;

use std::sync::Arc;

use pi_tui::tui::{Component, OverlayOptions, SizeValue};
use pi_tui::tui_main_screen::TuiMainScreen;

use support::virtual_terminal::VirtualTerminal;

struct StaticLines {
    lines: Vec<String>,
}

impl Component for StaticLines {
    fn render(&self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }

    fn invalidate(&self) {}
}

async fn render_and_flush(tui: &TuiMainScreen, terminal: &VirtualTerminal) {
    tui.request_render(Some(true));
    tokio::task::yield_now().await;
    terminal.wait_for_render().await;
}

#[tokio::test]
async fn does_not_leak_sgr_italic_from_line_to_line_without_overlay() {
    let width = 20usize;
    let terminal = Arc::new(VirtualTerminal::new(width as i64, 10));
    let tui = TuiMainScreen::new(terminal.clone(), None, None);

    let base_line = format!("\x1b[3m{}\x1b[23m", "X".repeat(width));
    tui.add_child(Arc::new(StaticLines {
        lines: vec![base_line, "INPUT".to_string()],
    }));

    tui.start();
    render_and_flush(&tui, &terminal).await;

    let cell = terminal.get_cell(1, 0).expect("cell at row 1, col 0");
    assert_eq!(cell.is_italic(), 0);
    tui.stop(None);
}

#[tokio::test]
async fn does_not_leak_sgr_italic_when_compositing_an_overlay() {
    let width = 20usize;
    let terminal = Arc::new(VirtualTerminal::new(width as i64, 10));
    let tui = TuiMainScreen::new(terminal.clone(), None, None);

    let base_line = format!("\x1b[3m{}\x1b[23m", "X".repeat(width));
    tui.add_child(Arc::new(StaticLines {
        lines: vec![base_line, "INPUT".to_string()],
    }));

    let overlay = Arc::new(StaticLines {
        lines: vec!["OVR".to_string()],
    });
    tui.show_overlay(
        overlay,
        Some(OverlayOptions {
            row: Some(SizeValue::Absolute(0.0)),
            col: Some(SizeValue::Absolute(5.0)),
            width: Some(SizeValue::Absolute(3.0)),
            ..Default::default()
        }),
    );

    tui.start();
    render_and_flush(&tui, &terminal).await;

    let cell = terminal.get_cell(1, 0).expect("cell at row 1, col 0");
    assert_eq!(cell.is_italic(), 0);
    tui.stop(None);
}

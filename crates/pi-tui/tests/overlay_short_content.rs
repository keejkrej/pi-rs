//! Port of packages/tui/test/overlay-short-content.test.ts

mod support;

use std::sync::Arc;

use pi_tui::tui::Component;
use pi_tui::tui_main_screen::TuiMainScreen;

use support::virtual_terminal::VirtualTerminal;

struct SimpleContent;

impl Component for SimpleContent {
    fn render(&self, _width: usize) -> Vec<String> {
        vec!["Line 1".to_string(), "Line 2".to_string(), "Line 3".to_string()]
    }

    fn invalidate(&self) {}
}

struct SimpleOverlay;

impl Component for SimpleOverlay {
    fn render(&self, _width: usize) -> Vec<String> {
        vec![
            "OVERLAY_TOP".to_string(),
            "OVERLAY_MID".to_string(),
            "OVERLAY_BOT".to_string(),
        ]
    }

    fn invalidate(&self) {}
}

#[tokio::test]
async fn renders_overlay_at_row_0_when_content_is_shorter_than_terminal_height() {
    let terminal = Arc::new(VirtualTerminal::new(80, 24));
    let tui = TuiMainScreen::new(terminal.clone(), None, None);

    tui.add_child(Arc::new(SimpleContent));
    tui.show_overlay(Arc::new(SimpleOverlay), None);

    tui.start();
    terminal.wait_for_render().await;

    let viewport = terminal.get_viewport();
    let has_overlay = viewport.iter().any(|line| line.contains("OVERLAY"));
    assert!(has_overlay, "expected overlay text in viewport: {viewport:?}");

    tui.stop(None);
}

//! Port of packages/tui/test/tui-shrink.test.ts

mod support;

use std::sync::Arc;

use pi_tui::tui::Component;
use pi_tui::tui_main_screen::TuiMainScreen;

use support::virtual_terminal::VirtualTerminal;

struct LinesComponent {
    lines: Vec<String>,
}

impl Component for LinesComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }

    fn invalidate(&self) {}
}

#[tokio::test]
async fn clears_all_rows_even_if_virtual_terminal_content_changed_before_clear() {
    let terminal = Arc::new(VirtualTerminal::new(40, 10));
    let tui = TuiMainScreen::new(terminal.clone(), None, None);

    tui.add_child(Arc::new(LinesComponent {
        lines: vec!["first".to_string(), "second".to_string(), "third".to_string()],
    }));

    tui.start();
    terminal.wait_for_render().await;

    let rendered = terminal.get_viewport().join("\n");
    assert!(rendered.contains("first"), "expected 'first' in viewport: {rendered}");
    assert!(rendered.contains("second"), "expected 'second' in viewport: {rendered}");
    assert!(rendered.contains("third"), "expected 'third' in viewport: {rendered}");

    tui.clear();
    tui.request_render(None);
    terminal.wait_for_render().await;

    let after_clear = terminal.get_viewport().join("\n");
    assert!(
        !after_clear.contains("first"),
        "unexpected 'first' after clear: {after_clear}"
    );
    assert!(
        !after_clear.contains("second"),
        "unexpected 'second' after clear: {after_clear}"
    );
    assert!(
        !after_clear.contains("third"),
        "unexpected 'third' after clear: {after_clear}"
    );

    tui.stop(None);
}

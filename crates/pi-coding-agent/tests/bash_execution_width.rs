//! Port of packages/coding-agent/test/bash-execution-width.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use pi_coding_agent::modes::interactive::components::bash_execution::BashExecutionComponent;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{TUI, TuiMainScreen, visible_width};
use serial_test::serial;

struct StubTerminal {
    columns: AtomicI64,
}

impl StubTerminal {
    fn new(columns: i64) -> Self {
        Self {
            columns: AtomicI64::new(columns),
        }
    }
}

#[async_trait]
impl Terminal for StubTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        self.columns.load(Ordering::Relaxed)
    }
    fn rows(&self) -> i64 {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&self, _lines: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _title: &str) {}
    fn set_progress(&self, _active: bool) {}
}

fn create_tui_stub(columns: i64) -> Arc<dyn TUI + Send + Sync> {
    Arc::new(TuiMainScreen::new(
        Arc::new(StubTerminal::new(columns)),
        Some(false),
        None,
    ))
}

#[test]
#[serial]
fn collapsed_preview_lines_respect_render_time_width_not_construction_time_width() {
    init_theme(None, Some(false));
    let wide_width: i64 = 200;
    let narrow_width: usize = 80;

    let stub = create_tui_stub(wide_width);
    let component = BashExecutionComponent::new("pwd", stub, None, None);

    let long_line = "x".repeat(150);
    component.append_output(&format!("{long_line}\n{long_line}\n"));
    component.set_complete(Some(0), false, None, None);

    let lines = component.render(narrow_width);
    for (i, line) in lines.iter().enumerate() {
        let w = visible_width(line);
        assert!(w <= narrow_width as i64, "Line {i} visible_width={w} > {narrow_width}");
    }
}

#[test]
#[serial]
fn recomputes_lines_when_width_changes_between_renders() {
    init_theme(None, Some(false));
    let stub = create_tui_stub(200);
    let component = BashExecutionComponent::new("echo hello", stub, None, None);

    let long_line = "abcdefghij".repeat(20);
    component.append_output(&format!("{long_line}\n"));
    component.set_complete(Some(0), false, None, None);

    let lines_200 = component.render(200);
    for line in &lines_200 {
        assert!(visible_width(line) <= 200);
    }

    let lines_60 = component.render(60);
    for (i, line) in lines_60.iter().enumerate() {
        let w = visible_width(line);
        assert!(w <= 60, "Line {i} visible_width={w} > 60");
    }
}

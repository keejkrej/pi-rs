//! Port of packages/coding-agent/test/status-indicator.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use pi_agent_core::ThinkingLevel;
use pi_coding_agent::core::keybindings::KeybindingsManager as AppKeybindingsManager;
use pi_coding_agent::modes::interactive::components::custom_editor::{CustomEditor, CustomEditorOptions};
use pi_coding_agent::modes::interactive::components::status_indicator::{
    CompactionStatusReason, IdleStatus, StatusIndicator,
};
use pi_coding_agent::modes::interactive::theme::theme::{ThemeColor, get_editor_theme, init_theme, theme};
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::tui::{
    OverlayHandle, OverlayOptions, QueryTerminalColorsOptions, TUI, TuiInputListener, TuiMode, TuiStopOptions,
};
use pi_tui::{
    Component, KeybindingsManager, TUI_KEYBINDINGS, Terminal, TerminalColorScheme, TerminalColors, TuiMouseEvent,
    TuiMouseEventResult, set_keybindings, strip_terminal_sequences, visible_width,
};
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn app_keybindings() -> AppKeybindingsManager {
    AppKeybindingsManager::new(None, None)
}

fn setup() -> TestGuard {
    init_theme(Some("dark"), None);
    set_keybindings(KeybindingsManager::new(
        pi_coding_agent::core::keybindings::KEYBINDINGS.clone(),
        None,
    ));
    TestGuard
}

struct MockTerminal {
    rows: i64,
}

#[async_trait]
impl Terminal for MockTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        self.rows
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

struct MockTui {
    terminal: Arc<dyn Terminal>,
    render_calls: AtomicUsize,
}

impl MockTui {
    fn new(rows: i64) -> Arc<Self> {
        Arc::new(Self {
            terminal: Arc::new(MockTerminal { rows }),
            render_calls: AtomicUsize::new(0),
        })
    }

    fn request_render_count(&self) -> usize {
        self.render_calls.load(Ordering::SeqCst)
    }
}

impl Component for MockTui {
    fn render(&self, _width: usize) -> Vec<String> {
        Vec::new()
    }
    fn handle_input(&self, _data: &str) {}
    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }
    fn wants_key_release(&self) -> bool {
        false
    }
    fn invalidate(&self) {}
}

#[async_trait]
impl TUI for MockTui {
    fn mode(&self) -> TuiMode {
        TuiMode::Regular
    }
    fn children(&self) -> Vec<Arc<dyn Component>> {
        Vec::new()
    }
    fn set_children(&self, _children: Vec<Arc<dyn Component>>) {}
    fn terminal(&self) -> Arc<dyn Terminal> {
        Arc::clone(&self.terminal)
    }
    fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        None
    }
    fn set_on_debug(&self, _on_debug: Option<Arc<dyn Fn() + Send + Sync>>) {}
    fn full_redraws(&self) -> i64 {
        0
    }
    fn add_child(&self, _component: Arc<dyn Component>) {}
    fn remove_child(&self, _component: &Arc<dyn Component>) {}
    fn clear(&self) {}
    fn get_show_hardware_cursor(&self) -> bool {
        false
    }
    fn set_show_hardware_cursor(&self, _enabled: bool) {}
    fn get_clear_on_shrink(&self) -> bool {
        false
    }
    fn set_clear_on_shrink(&self, _enabled: bool) {}
    fn set_focus(&self, _component: Option<Arc<dyn Component>>) {}
    fn show_overlay(&self, _component: Arc<dyn Component>, _options: Option<OverlayOptions>) -> OverlayHandle {
        unreachable!("not used in status_indicator tests")
    }
    fn hide_overlay(&self) {}
    fn has_overlay(&self) -> bool {
        false
    }
    fn start(&self) {}
    fn stop(&self, _options: Option<TuiStopOptions>) {}
    fn render_now(&self, _force: Option<bool>) {}
    fn request_render(&self, _force: Option<bool>) {
        self.render_calls.fetch_add(1, Ordering::SeqCst);
    }
    fn add_input_listener(&self, _listener: TuiInputListener) -> pi_js::Unsubscribe {
        pi_js::Unsubscribe::new(|| {})
    }
    fn remove_input_listener(&self, _listener: &TuiInputListener) {}
    fn on_terminal_color_scheme_change(
        &self,
        _listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        pi_js::Unsubscribe::new(|| {})
    }
    fn set_terminal_color_scheme_notifications(&self, _enabled: bool) {}
    async fn query_terminal_colors(&self, _options: QueryTerminalColorsOptions) -> TerminalColors {
        TerminalColors::default()
    }
}

#[test]
#[serial]
fn keeps_idle_status_at_the_same_height_as_standalone_status_indicators() {
    let idle = IdleStatus::new();
    let lines = idle.render(20);
    assert_eq!(lines, vec![" ".repeat(20), " ".repeat(20)]);
}

#[test]
#[serial]
fn keeps_the_top_border_unchanged_unless_the_editor_opts_in() {
    let _g = setup();
    let tui = MockTui::new(10);
    let editor = CustomEditor::new(tui.clone(), get_editor_theme(), app_keybindings(), None);
    let indicator = StatusIndicator::working(tui, "Working", None, None);
    editor.set_working_status_indicator(Some(indicator.clone()));

    let top = strip_terminal_sequences(&editor.render(20)[0]);
    assert_eq!(top, "─".repeat(20));

    let standalone_line = &indicator.render(20)[1];
    let t = theme();
    assert!(standalone_line.contains(&t.get_fg_ansi(ThemeColor::Accent)));
    assert!(standalone_line.contains(&t.get_fg_ansi(ThemeColor::Muted)));
    indicator.dispose();
}

#[test]
#[serial]
fn embeds_the_working_indicator_when_the_editor_opts_in() {
    let _g = setup();
    let tui = MockTui::new(10);
    let editor = CustomEditor::new(
        tui.clone(),
        get_editor_theme(),
        app_keybindings(),
        Some(CustomEditorOptions {
            embed_working_status: Some(true),
            ..Default::default()
        }),
    );
    assert!(editor.embed_working_status());

    let t = theme();
    editor.set_border_color(t.get_thinking_border_color(ThinkingLevel::High));
    let editor_c = editor.clone();
    let indicator = StatusIndicator::working(
        tui,
        "Working",
        None,
        Some(Arc::new(move |text: &str| (editor_c.border_color())(text))),
    );
    editor.set_working_status_indicator(Some(indicator.clone()));

    let top_border = editor.render(20)[0].clone();
    assert_eq!(strip_terminal_sequences(&top_border), "── ⠋ Working ───────");
    assert_eq!(visible_width(&top_border), 20);
    assert_eq!(top_border.split(&t.get_fg_ansi(ThemeColor::ThinkingHigh)).count(), 5);
    indicator.dispose();
}

#[tokio::test(start_paused = true)]
#[serial]
async fn embeds_compaction_summary_and_retry_labels_within_the_border_width() {
    let _g = setup();
    let tui = MockTui::new(10);
    let editor = CustomEditor::new(
        tui.clone(),
        get_editor_theme(),
        app_keybindings(),
        Some(CustomEditorOptions {
            embed_working_status: Some(true),
            ..Default::default()
        }),
    );

    let indicators = vec![
        StatusIndicator::compaction(tui.clone(), CompactionStatusReason::Manual, None),
        StatusIndicator::compaction(tui.clone(), CompactionStatusReason::Threshold, None),
        StatusIndicator::compaction(tui.clone(), CompactionStatusReason::Overflow, None),
        StatusIndicator::branch_summary(tui.clone()),
        StatusIndicator::retry(tui.clone(), 1, 3, 3000),
    ];

    for indicator in &indicators {
        editor.set_working_status_indicator(Some(indicator.clone()));
        let label = strip_terminal_sequences(&indicator.render(120)[1]).trim().to_string();
        assert!(strip_terminal_sequences(&editor.render(120)[0]).contains(&format!("── {label} ")));
        for width in [1, 4, 10, 20, 80, 120] {
            assert_eq!(visible_width(&editor.render(width)[0]), width as i64);
        }
    }

    tokio::time::advance(Duration::from_millis(1000)).await;
    tokio::task::yield_now().await;
    assert!(strip_terminal_sequences(&editor.render(120)[0]).contains("Retrying (1/3) in 2s"));

    editor.set_working_status_indicator(None);
    assert_eq!(strip_terminal_sequences(&editor.render(120)[0]), "─".repeat(120));

    for indicator in indicators {
        indicator.dispose();
    }
}

#[tokio::test(start_paused = true)]
#[serial]
async fn disposes_retry_countdown_updates() {
    let _g = setup();
    let tui = MockTui::new(10);
    let indicator = StatusIndicator::retry(tui.clone(), 1, 3, 1000);
    let calls_before_dispose = tui.request_render_count();

    indicator.dispose();
    tokio::time::advance(Duration::from_millis(2000)).await;
    tokio::task::yield_now().await;

    assert_eq!(tui.request_render_count(), calls_before_dispose);
}

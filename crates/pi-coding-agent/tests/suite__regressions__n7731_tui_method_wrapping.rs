//! Port of packages/coding-agent/test/suite/regressions/7731-tui-method-wrapping.test.ts

// #7731

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::modes::interactive::tui_renderer::create_interactive_tui_reference;
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::tui::{
    OverlayHandle, OverlayOptions, QueryTerminalColorsOptions, TUI, TuiInputListener, TuiMode, TuiStopOptions,
};
use pi_tui::{Component, Terminal, TerminalColorScheme, TerminalColors, TuiMouseEvent, TuiMouseEventResult};

struct DummyTerminal;

#[async_trait]
impl Terminal for DummyTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        80
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

struct MockRenderer {
    terminal: Arc<dyn Terminal>,
    request_render_calls: Arc<AtomicUsize>,
    render_prefix: String,
}

impl MockRenderer {
    fn new(render_prefix: &str) -> Arc<Self> {
        Arc::new(Self {
            terminal: Arc::new(DummyTerminal),
            request_render_calls: Arc::new(AtomicUsize::new(0)),
            render_prefix: render_prefix.to_string(),
        })
    }
}

impl Component for MockRenderer {
    fn render(&self, width: usize) -> Vec<String> {
        vec![format!("{}{width}", self.render_prefix)]
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
impl TUI for MockRenderer {
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
        unreachable!()
    }
    fn hide_overlay(&self) {}
    fn has_overlay(&self) -> bool {
        false
    }
    fn start(&self) {}
    fn stop(&self, _options: Option<TuiStopOptions>) {}
    fn render_now(&self, _force: Option<bool>) {}
    fn request_render(&self, _force: Option<bool>) {
        self.request_render_calls.fetch_add(1, Ordering::SeqCst);
    }
    fn add_input_listener(&self, _listener: TuiInputListener) -> pi_js::Unsubscribe {
        pi_js::Unsubscribe::noop()
    }
    fn remove_input_listener(&self, _listener: &TuiInputListener) {}
    fn on_terminal_color_scheme_change(
        &self,
        _listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        pi_js::Unsubscribe::noop()
    }
    fn set_terminal_color_scheme_notifications(&self, _enabled: bool) {}
    async fn query_terminal_colors(&self, _options: QueryTerminalColorsOptions) -> TerminalColors {
        TerminalColors::default()
    }
}

mod tui_method_wrapping {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn calls_the_method_captured_before_a_replacement() {
        let renderer = MockRenderer::new("width: ");
        let renderer_slot: Arc<Mutex<Arc<dyn TUI + Send + Sync>>> =
            Arc::new(Mutex::new(Arc::clone(&renderer) as Arc<dyn TUI + Send + Sync>));
        let slot_for_ref = Arc::clone(&renderer_slot);
        let tui = create_interactive_tui_reference(Arc::new(move || Arc::clone(&slot_for_ref.lock().unwrap())));

        // PORT: Rust structs do not allow mutating trait methods in place (`tui.render = ...`),
        // so wrapping captures a cloned `InteractiveTuiReference` handle and delegates to `render`.
        let original_tui = tui.clone();
        let wrapped_render = move |width: usize| original_tui.render(width);

        assert_eq!(wrapped_render(80), vec!["width: 80".to_string()]);
    }

    #[test]
    fn routes_a_captured_method_to_a_replacement_renderer() {
        let regular = MockRenderer::new("regular: ");
        let fullscreen = MockRenderer::new("fullscreen: ");
        let renderer_slot: Arc<Mutex<Arc<dyn TUI + Send + Sync>>> =
            Arc::new(Mutex::new(Arc::clone(&regular) as Arc<dyn TUI + Send + Sync>));
        let slot_for_ref = Arc::clone(&renderer_slot);
        let tui = create_interactive_tui_reference(Arc::new(move || Arc::clone(&slot_for_ref.lock().unwrap())));

        let captured = tui.clone();
        let request_render = move || captured.request_render(None);

        request_render();
        *renderer_slot.lock().unwrap() = Arc::clone(&fullscreen) as Arc<dyn TUI + Send + Sync>;
        request_render();

        assert_eq!(regular.request_render_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fullscreen.request_render_calls.load(Ordering::SeqCst), 1);
    }
}

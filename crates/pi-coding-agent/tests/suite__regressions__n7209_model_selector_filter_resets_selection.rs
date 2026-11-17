//! Port of packages/coding-agent/test/suite/regressions/7209-model-selector-filter-resets-selection.test.ts

// #7209

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_ai::Model;
use pi_coding_agent::modes::interactive::components::model_selector::{ModelSelectorComponent, ScopedModelItem};
use pi_coding_agent::modes::interactive::model_catalog_refresh::{ModelCatalogRefreshResult, ModelCatalogRuntime};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::tui::{
    OverlayHandle, OverlayOptions, QueryTerminalColorsOptions, TUI, TuiInputListener, TuiMode, TuiStopOptions,
};
use pi_tui::{
    Component, KeybindingsManager, TUI_KEYBINDINGS, Terminal, TerminalColorScheme, TerminalColors, TuiMouseEvent,
    TuiMouseEventResult, set_keybindings, strip_terminal_sequences,
};
use serde_json::json;
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn setup() -> TestGuard {
    init_theme(Some("dark"), None);
    set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    TestGuard
}

struct NoopTerminal;

#[async_trait]
impl Terminal for NoopTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        40
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

struct FakeTui {
    terminal: Arc<dyn Terminal>,
}

impl FakeTui {
    fn new() -> Arc<dyn TUI + Send + Sync> {
        Arc::new(Self {
            terminal: Arc::new(NoopTerminal),
        })
    }
}

impl Component for FakeTui {
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
impl TUI for FakeTui {
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
    fn request_render(&self, _force: Option<bool>) {}
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

struct FakeRuntime {
    models: Mutex<Vec<Model>>,
}

impl FakeRuntime {
    fn new(models: Vec<Model>) -> Arc<Self> {
        Arc::new(Self {
            models: Mutex::new(models),
        })
    }
}

impl ModelCatalogRuntime for FakeRuntime {
    fn runtime_ptr_key(&self) -> usize {
        self as *const Self as usize
    }

    fn refresh(&self, _signal: AbortSignal) -> BoxFuture<pi_js::Result<ModelCatalogRefreshResult>> {
        Box::pin(async { Ok(ModelCatalogRefreshResult::default()) })
    }

    fn get_available_snapshot(&self) -> Vec<Model> {
        self.models.lock().unwrap().clone()
    }

    fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.models
            .lock()
            .unwrap()
            .iter()
            .find(|m| m.provider.as_str() == provider && m.id == id)
            .cloned()
    }

    fn get_error(&self) -> Option<String> {
        None
    }
}

fn make_model(id: &str, name: &str, reasoning: bool) -> Model {
    serde_json::from_value(json!({
        "id": id,
        "name": name,
        "api": "faux",
        "provider": "faux",
        "baseUrl": "http://localhost:0",
        "reasoning": reasoning,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 128_000,
        "maxTokens": 16_384,
    }))
    .expect("valid test model")
}

/// Return the model id of the highlighted (→) row in the rendered selector.
fn selected_model_id(rendered: &str) -> Option<String> {
    let line = rendered.lines().find(|l| l.starts_with("→ "))?;
    let rest = line.trim_start_matches('→').trim_start();
    let rest = rest.trim_start_matches('✓').trim_start();
    rest.split(' ')
        .next()
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

async fn wait_for_refreshed(selector: &ModelSelectorComponent) {
    for _ in 0..500 {
        let rendered = strip_terminal_sequences(&selector.render(120).join("\n"));
        if rendered.contains("Model catalogs refreshed.") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("timed out waiting for Model catalogs refreshed.");
}

mod model_selector_filter_resets_selection {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    #[serial]
    async fn moves_selection_to_the_first_row_in_the_all_tab_when_typing_a_query() {
        let _g = setup();
        let alpha1 = make_model("alpha-1", "Alpha One", true);
        let alpha2 = make_model("alpha-2", "Alpha Two", true);
        let alpha3 = make_model("alpha-3", "Alpha Three", true);
        let beta1 = make_model("beta-1", "Beta One", true);
        let runtime = FakeRuntime::new(vec![alpha1.clone(), alpha2, alpha3, beta1]);

        let selector = ModelSelectorComponent::new(
            FakeTui::new(),
            Some(alpha1),
            runtime,
            Vec::new(),
            Arc::new(|_| {}),
            Arc::new(|| {}),
            None,
            None,
            None,
        );

        wait_for_refreshed(&selector).await;

        // Current model (alpha-1) is sorted first, so selection starts on row 0.
        assert_eq!(
            selected_model_id(&strip_terminal_sequences(&selector.render(120).join("\n"))).as_deref(),
            Some("alpha-1")
        );

        // Move selection down two rows to alpha-3.
        selector.handle_input("\x1b[B");
        selector.handle_input("\x1b[B");
        assert_eq!(
            selected_model_id(&strip_terminal_sequences(&selector.render(120).join("\n"))).as_deref(),
            Some("alpha-3")
        );

        // Type a query that matches the three alpha models. The selection must
        // move back to the top row (alpha-1), not stay clamped at index 2.
        for ch in "alpha".chars() {
            selector.handle_input(&ch.to_string());
        }

        let rendered = strip_terminal_sequences(&selector.render(120).join("\n"));
        assert_eq!(selected_model_id(&rendered).as_deref(), Some("alpha-1"));
        // Sanity: the filter actually narrowed the list.
        assert!(!rendered.contains("beta-1"));

        selector.dispose();
    }

    #[tokio::test]
    #[serial]
    async fn moves_selection_to_the_first_row_in_the_scoped_tab_when_typing_a_query() {
        let _g = setup();
        let alpha1 = make_model("alpha-1", "Alpha One", true);
        let alpha2 = make_model("alpha-2", "Alpha Two", true);
        let alpha3 = make_model("alpha-3", "Alpha Three", true);
        let runtime = FakeRuntime::new(vec![alpha1.clone(), alpha2.clone(), alpha3.clone()]);

        // Scoped list is intentionally not in current-model-first order; the
        // current model (alpha-1) sits at index 2.
        let selector = ModelSelectorComponent::new(
            FakeTui::new(),
            Some(alpha1.clone()),
            runtime,
            vec![
                ScopedModelItem {
                    model: alpha2,
                    thinking_level: None,
                },
                ScopedModelItem {
                    model: alpha3,
                    thinking_level: None,
                },
                ScopedModelItem {
                    model: alpha1,
                    thinking_level: None,
                },
            ],
            Arc::new(|_| {}),
            Arc::new(|| {}),
            None,
            None,
            None,
        );

        wait_for_refreshed(&selector).await;

        // Selection starts on the current model (alpha-1), which is row 2 here.
        assert_eq!(
            selected_model_id(&strip_terminal_sequences(&selector.render(120).join("\n"))).as_deref(),
            Some("alpha-1")
        );

        // Type a query matching all three scoped models. Selection must move to
        // the top row (alpha-2), not stay clamped at index 2 (alpha-1).
        for ch in "alpha".chars() {
            selector.handle_input(&ch.to_string());
        }

        assert_eq!(
            selected_model_id(&strip_terminal_sequences(&selector.render(120).join("\n"))).as_deref(),
            Some("alpha-2")
        );

        selector.dispose();
    }
}

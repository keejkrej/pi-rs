//! Port of packages/coding-agent/test/model-selector.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::Model;
use pi_coding_agent::core::keybindings::KEYBINDINGS;
use pi_coding_agent::modes::interactive::components::model_selector::ModelSelectorComponent;
use pi_coding_agent::modes::interactive::model_catalog_refresh::{ModelCatalogRefreshResult, ModelCatalogRuntime};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;
use pi_tui::keybindings::KeyIdOrList;
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::tui::{
    OverlayHandle, OverlayOptions, QueryTerminalColorsOptions, TUI, TuiInputListener, TuiMode, TuiStopOptions,
};
use pi_tui::{
    Component, KeybindingsConfig, KeybindingsManager, TUI_KEYBINDINGS, Terminal, TerminalColorScheme, TerminalColors,
    TuiMouseEvent, TuiMouseEventResult, set_keybindings, strip_terminal_sequences,
};
use serde_json::json;
use serial_test::serial;
use tokio::sync::oneshot;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn app_keybindings(user_bindings: Option<KeybindingsConfig>) -> KeybindingsManager {
    KeybindingsManager::new(KEYBINDINGS.clone(), user_bindings)
}

fn setup(user_bindings: Option<KeybindingsConfig>) -> TestGuard {
    init_theme(Some("dark"), None);
    set_keybindings(app_keybindings(user_bindings));
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

fn make_model(id: &str, name: Option<&str>, provider: &str, reasoning: bool) -> Model {
    serde_json::from_value(json!({
        "id": id,
        "name": name.unwrap_or(id),
        "api": "openai-responses",
        "provider": provider,
        "baseUrl": "https://api.example.com/v1",
        "reasoning": reasoning,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 200_000,
        "maxTokens": 16_384,
    }))
    .expect("test model parses")
}

struct FakeRuntime {
    models: Mutex<Vec<Model>>,
    refresh_rx: Mutex<Option<oneshot::Receiver<ModelCatalogRefreshResult>>>,
    fixed_result: Option<ModelCatalogRefreshResult>,
}

impl FakeRuntime {
    fn new(models: Vec<Model>, fixed_result: ModelCatalogRefreshResult) -> Arc<Self> {
        Arc::new(Self {
            models: Mutex::new(models),
            refresh_rx: Mutex::new(None),
            fixed_result: Some(fixed_result),
        })
    }

    fn with_pending_refresh(models: Vec<Model>) -> (Arc<Self>, oneshot::Sender<ModelCatalogRefreshResult>) {
        let (tx, rx) = oneshot::channel();
        let rt = Arc::new(Self {
            models: Mutex::new(models),
            refresh_rx: Mutex::new(Some(rx)),
            fixed_result: None,
        });
        (rt, tx)
    }

    fn set_models(&self, models: Vec<Model>) {
        *self.models.lock().unwrap() = models;
    }
}

impl ModelCatalogRuntime for FakeRuntime {
    fn runtime_ptr_key(&self) -> usize {
        self as *const Self as usize
    }

    fn refresh(&self, _signal: AbortSignal) -> BoxFuture<pi_js::Result<ModelCatalogRefreshResult>> {
        let rx = self.refresh_rx.lock().unwrap().take();
        let fixed = self.fixed_result.clone();
        Box::pin(async move {
            if let Some(rx) = rx {
                Ok(rx.await.unwrap_or_default())
            } else {
                Ok(fixed.unwrap_or_default())
            }
        })
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

fn selected_model_id(selector: &ModelSelectorComponent) -> Option<String> {
    let rendered = strip_terminal_sequences(&selector.render(120).join("\n"));
    let row = rendered.lines().find(|line| line.starts_with('→'))?;
    let trimmed = row
        .trim_start_matches('→')
        .trim_start()
        .trim_start_matches('✓')
        .trim_start();
    trimmed.split(' ').next().map(ToString::to_string)
}

#[tokio::test]
#[serial]
async fn keeps_the_current_model_marked_while_browsing() {
    let _g = setup(None);
    let m1 = make_model("current-model", Some("Current Model"), "openai", true);
    let m2 = make_model("browsed-model", Some("Browsed Model"), "openai", true);
    let rt = FakeRuntime::new(vec![m1.clone(), m2], ModelCatalogRefreshResult::default());

    let selector = ModelSelectorComponent::new(
        FakeTui::new(),
        Some(m1),
        rt,
        Vec::new(),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    let get_model_row = |id: &str| -> Option<String> {
        strip_terminal_sequences(&selector.render(120).join("\n"))
            .lines()
            .find(|line| line.contains(&format!("{id} ")))
            .map(|l| l.trim_end().to_string())
    };

    assert_eq!(
        get_model_row("current-model").as_deref(),
        Some("→ ✓ current-model [openai]")
    );
    selector.handle_input("\x1b[B");
    assert_eq!(
        get_model_row("current-model").as_deref(),
        Some("  ✓ current-model [openai]")
    );
    assert_eq!(
        get_model_row("browsed-model").as_deref(),
        Some("→   browsed-model [openai]")
    );
    selector.dispose();
}

#[tokio::test]
#[serial]
async fn shows_model_names_that_differ_from_the_id() {
    let _g = setup(None);
    let m1 = make_model("minimax/minimax-m3", Some("MiniMax M3 [parasail]"), "openai", false);
    let m2 = make_model("claude-sonnet-4-5", Some("Claude Sonnet 4.5"), "openai", false);
    let rt = FakeRuntime::new(vec![m1, m2], ModelCatalogRefreshResult::default());

    let selector = ModelSelectorComponent::new(
        FakeTui::new(),
        None,
        rt,
        Vec::new(),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    let rows: Vec<String> = strip_terminal_sequences(&selector.render(120).join("\n"))
        .lines()
        .map(|l| l.chars().skip(4).collect::<String>().trim_end().to_string())
        .collect();
    assert!(rows.contains(&"minimax/minimax-m3 (MiniMax M3 [parasail]) [openai]".to_string()));
    assert!(rows.contains(&"claude-sonnet-4-5 [openai]".to_string()));
    selector.dispose();
}

#[tokio::test]
#[serial]
async fn moves_by_one_page_with_remapped_selection_page_keys() {
    let mut custom = KeybindingsConfig::new();
    custom.insert(
        "tui.select.pageUp".to_string(),
        Some(KeyIdOrList::One("alt+v".to_string())),
    );
    custom.insert(
        "tui.select.pageDown".to_string(),
        Some(KeyIdOrList::One("ctrl+v".to_string())),
    );
    let _g = setup(Some(custom));

    let models: Vec<Model> = (0..12)
        .map(|i| make_model(&format!("model-{i:02}"), None, "openai", false))
        .collect();
    let rt = FakeRuntime::new(models, ModelCatalogRefreshResult::default());
    let picked: Arc<Mutex<Option<Model>>> = Arc::new(Mutex::new(None));
    let picked_c = Arc::clone(&picked);

    let selector = ModelSelectorComponent::new(
        FakeTui::new(),
        None,
        rt,
        Vec::new(),
        Arc::new(move |m| {
            *picked_c.lock().unwrap() = Some(m);
        }),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    selector.handle_input("\x16");
    selector.handle_input("\x16");
    selector.handle_input("\x1bv");
    selector.handle_input("\r");

    assert_eq!(picked.lock().unwrap().as_ref().map(|m| m.id.as_str()), Some("model-01"));
    selector.dispose();
}

#[tokio::test]
#[serial]
async fn uses_the_configured_save_binding() {
    let mut custom = KeybindingsConfig::new();
    custom.insert(
        "app.models.save".to_string(),
        Some(KeyIdOrList::One("ctrl+r".to_string())),
    );
    let _g = setup(Some(custom));

    let current = make_model("gpt-4o", None, "openai", false);
    let rt = FakeRuntime::new(vec![current.clone()], ModelCatalogRefreshResult::default());
    let saved: Arc<Mutex<Option<Model>>> = Arc::new(Mutex::new(None));
    let saved_c = Arc::clone(&saved);

    let selector = ModelSelectorComponent::new(
        FakeTui::new(),
        Some(current.clone()),
        rt,
        Vec::new(),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        Some(Arc::new(move |m| {
            *saved_c.lock().unwrap() = Some(m);
        })),
        None,
    );

    let rendered = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(rendered.contains("Ctrl+R to set as default"), "rendered={rendered}");
    selector.handle_input("\x13");
    assert!(saved.lock().unwrap().is_none());
    selector.handle_input("\x12");
    assert_eq!(saved.lock().unwrap().as_ref().map(|m| m.id.as_str()), Some("gpt-4o"));
    selector.dispose();
}

#[tokio::test]
#[serial]
async fn lists_every_catalog_that_failed_to_refresh() {
    let _g = setup(None);
    let current = make_model("gpt-4o", None, "openai", false);
    let mut errors = IndexMap::new();
    errors.insert("openai".to_string(), "unavailable".to_string());
    errors.insert("anthropic".to_string(), "unavailable".to_string());

    let rt = FakeRuntime::new(
        vec![current.clone()],
        ModelCatalogRefreshResult { aborted: false, errors },
    );

    let selector = ModelSelectorComponent::new(
        FakeTui::new(),
        Some(current),
        rt,
        Vec::new(),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    for _ in 0..20 {
        tokio::task::yield_now().await;
        let rendered = strip_terminal_sequences(&selector.render(120).join("\n"));
        if rendered.contains("Could not refresh 2 model catalogs (openai, anthropic); showing cached models.") {
            selector.dispose();
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let rendered = strip_terminal_sequences(&selector.render(120).join("\n"));
    panic!("Expected multi-catalog error in rendered selector: {rendered}");
}

async fn finish_pending_refresh(selector: &ModelSelectorComponent, tx: oneshot::Sender<ModelCatalogRefreshResult>) {
    let _ = tx.send(ModelCatalogRefreshResult::default());
    for _ in 0..40 {
        tokio::task::yield_now().await;
        let rendered = strip_terminal_sequences(&selector.render(120).join("\n"));
        if rendered.contains("Model catalogs refreshed.") {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let rendered = strip_terminal_sequences(&selector.render(120).join("\n"));
    panic!("Refresh status did not appear: {rendered}");
}

#[tokio::test]
#[serial]
async fn keeps_the_selection_on_the_browsed_model_after_refresh() {
    let _g = setup(None);
    let m1 = make_model("alpha-1", None, "openai", false);
    let m2 = make_model("alpha-2", None, "openai", false);
    let m3 = make_model("alpha-3", None, "openai", false);
    let m4 = make_model("beta-1", None, "openai", false);
    let (rt, tx) = FakeRuntime::with_pending_refresh(vec![m1.clone(), m2, m3, m4]);

    let selector = ModelSelectorComponent::new(
        FakeTui::new(),
        Some(m1),
        rt,
        Vec::new(),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    selector.handle_input("\x1b[B");
    selector.handle_input("\x1b[B");
    finish_pending_refresh(&selector, tx).await;

    assert_eq!(selected_model_id(&selector).as_deref(), Some("alpha-3"));
    selector.dispose();
}

#[tokio::test]
#[serial]
async fn keeps_the_selection_while_a_search_query_is_active_after_refresh() {
    let _g = setup(None);
    let m1 = make_model("alpha-1", None, "openai", false);
    let m2 = make_model("alpha-2", None, "openai", false);
    let m3 = make_model("alpha-3", None, "openai", false);
    let m4 = make_model("beta-1", None, "openai", false);
    let (rt, tx) = FakeRuntime::with_pending_refresh(vec![m1.clone(), m2, m3, m4]);

    let selector = ModelSelectorComponent::new(
        FakeTui::new(),
        Some(m1),
        rt,
        Vec::new(),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    for ch in "alpha".chars() {
        selector.handle_input(&ch.to_string());
    }
    selector.handle_input("\x1b[B");
    let browsed = selected_model_id(&selector);

    finish_pending_refresh(&selector, tx).await;

    assert_eq!(selected_model_id(&selector), browsed);
    assert_ne!(browsed.as_deref(), Some("alpha-1"));
    selector.dispose();
}

#[tokio::test]
#[serial]
async fn follows_the_selected_model_when_new_rows_shift_its_position() {
    let _g = setup(None);
    let m1 = make_model("alpha-1", None, "openai", false);
    let m2 = make_model("alpha-2", None, "openai", false);
    let m3 = make_model("alpha-3", None, "openai", false);
    let m4 = make_model("beta-1", None, "openai", false);
    let added = make_model("added-model", None, "openai", false);
    let (rt, tx) = FakeRuntime::with_pending_refresh(vec![m1.clone(), m2.clone(), m3.clone(), m4.clone()]);

    let selector = ModelSelectorComponent::new(
        FakeTui::new(),
        Some(m1.clone()),
        Arc::clone(&rt) as Arc<dyn ModelCatalogRuntime>,
        Vec::new(),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    selector.handle_input("\x1b[B");
    rt.set_models(vec![added, m1, m2, m3, m4]);

    finish_pending_refresh(&selector, tx).await;

    assert_eq!(selected_model_id(&selector).as_deref(), Some("alpha-2"));
    selector.handle_input("\x1b[A");
    selector.handle_input("\x1b[A");
    assert_eq!(selected_model_id(&selector).as_deref(), Some("alpha-1"));
    selector.dispose();
}

#[tokio::test]
#[serial]
async fn falls_back_to_the_current_model_when_the_selected_model_disappears() {
    let _g = setup(None);
    let m1 = make_model("alpha-1", None, "openai", false);
    let m2 = make_model("alpha-2", None, "openai", false);
    let m3 = make_model("alpha-3", None, "openai", false);
    let m4 = make_model("beta-1", None, "openai", false);
    let (rt, tx) = FakeRuntime::with_pending_refresh(vec![m1.clone(), m2, m3.clone(), m4.clone()]);

    let selector = ModelSelectorComponent::new(
        FakeTui::new(),
        Some(m1.clone()),
        Arc::clone(&rt) as Arc<dyn ModelCatalogRuntime>,
        Vec::new(),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    selector.handle_input("\x1b[B");
    rt.set_models(vec![m1, m3, m4]);

    finish_pending_refresh(&selector, tx).await;

    assert_eq!(selected_model_id(&selector).as_deref(), Some("alpha-1"));
    selector.dispose();
}

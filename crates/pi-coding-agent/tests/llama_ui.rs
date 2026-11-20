//! Port of packages/coding-agent/test/llama-ui.test.ts

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_coding_agent::core::extensions::types::{
    AutocompleteProviderFactory, BuildSystemPromptOptions, CancelledResult, CompactOptions, ContextUsage,
    CustomMessageContent, CustomMessageDraft, CustomUiFactory, EditorFactory, ExecuteToolOptions,
    ExtensionCommandContext, ExtensionContext, ExtensionContextBackend, ExtensionCustomOptions, ExtensionMode,
    ExtensionUIContext, ExtensionUIDialogOptions, ExtensionWidgetOptions, FooterFactory, ForkOptions, HeaderFactory,
    ModelRegistry, NavigateTreeOptions, NotifyType, ReadonlySessionManager, ScopedModel, SendMessageOptions,
    SendUserMessageOptions, SetThemeResult, SwitchSessionOptions, TerminalInputHandler, Theme, ThemeInfo,
    WidgetFactory, WorkingIndicatorOptions,
};
use pi_coding_agent::core::keybindings::KEYBINDINGS;
use pi_coding_agent::extensions::llama::huggingface::HuggingFaceModel;
use pi_coding_agent::extensions::llama::ui::{HfSearchFn, LlamaUi, show_llama_ui};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_coding_agent::utils::ansi::strip_ansi;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::terminal::{Terminal, TerminalInputHandler as TermIn, TerminalResizeHandler};
use pi_tui::{Component, KeybindingsManager, TUI, TUI_KEYBINDINGS, TuiMainScreen, set_keybindings};
use serde_json::Value;
use serial_test::serial;
use tokio::sync::oneshot;

const DOWN: &str = "\x1b[B";
const ENTER: &str = "\r";

struct FakeTerminal;

#[async_trait]
impl Terminal for FakeTerminal {
    fn start(&self, _on_input: TermIn, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
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

struct MockCustomUi {
    tui: Arc<dyn TUI>,
    view: Mutex<Option<Arc<dyn Component>>>,
    on_view: Mutex<Option<oneshot::Sender<Arc<dyn Component>>>>,
}

#[async_trait]
impl ExtensionUIContext for MockCustomUi {
    async fn select(&self, _: &str, _: Vec<String>, _: Option<ExtensionUIDialogOptions>) -> Result<Option<String>> {
        Ok(None)
    }
    async fn confirm(&self, _: &str, _: &str, _: Option<ExtensionUIDialogOptions>) -> Result<bool> {
        Ok(false)
    }
    async fn input(&self, _: &str, _: Option<&str>, _: Option<ExtensionUIDialogOptions>) -> Result<Option<String>> {
        Ok(None)
    }
    fn notify(&self, _: &str, _: Option<NotifyType>) {}
    fn on_terminal_input(&self, _: TerminalInputHandler) -> Unsubscribe {
        Unsubscribe::noop()
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_working_message(&self, _: Option<&str>) {}
    fn set_working_visible(&self, _: bool) {}
    fn set_working_indicator(&self, _: Option<WorkingIndicatorOptions>) {}
    fn set_hidden_thinking_label(&self, _: Option<&str>) {}
    fn set_widget_lines(&self, _: &str, _: Option<Vec<String>>, _: Option<ExtensionWidgetOptions>) {}
    fn set_widget_factory(&self, _: &str, _: Option<WidgetFactory>, _: Option<ExtensionWidgetOptions>) {}
    fn set_footer(&self, _: Option<FooterFactory>) {}
    fn set_header(&self, _: Option<HeaderFactory>) {}
    fn set_title(&self, _: &str) {}
    async fn custom(&self, factory: CustomUiFactory, _: Option<ExtensionCustomOptions>) -> Result<Value> {
        let (done_tx, done_rx) = oneshot::channel::<Value>();
        let done_slot = Arc::new(Mutex::new(Some(done_tx)));
        let done: pi_coding_agent::core::extensions::types::DoneFn = Arc::new(move |val| {
            if let Some(tx) = done_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = tx.send(val);
            }
        });
        let comp = factory(
            Arc::clone(&self.tui),
            Theme::default(),
            KeybindingsManager::new(KEYBINDINGS.clone(), None),
            done,
        )
        .await?;
        *self.view.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&comp));
        if let Some(tx) = self.on_view.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(comp);
        }
        let _ = done_rx.await;
        Ok(Value::Null)
    }
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    async fn editor(&self, _: &str, _: Option<&str>) -> Result<Option<String>> {
        Ok(None)
    }
    fn add_autocomplete_provider(&self, _: AutocompleteProviderFactory) {}
    fn set_editor_component(&self, _: Option<EditorFactory>) {}
    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }
    fn get_theme(&self, _: &str) -> Option<Theme> {
        None
    }
    fn set_theme_by_name(&self, _: &str) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn set_theme(&self, _: Theme) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn get_tools_expanded(&self) -> bool {
        false
    }
    fn set_tools_expanded(&self, _: bool) {}
}

struct MockBackend {
    ui: Arc<dyn ExtensionUIContext>,
}

impl ExtensionContextBackend for MockBackend {
    fn assert_active(&self) -> Result<()> {
        Ok(())
    }
    fn ui(&self) -> Arc<dyn ExtensionUIContext> {
        Arc::clone(&self.ui)
    }
    fn mode(&self) -> ExtensionMode {
        ExtensionMode::Tui
    }
    fn has_ui(&self) -> bool {
        true
    }
    fn cwd(&self) -> String {
        "/tmp".to_string()
    }
    fn session_manager(&self) -> ReadonlySessionManager {
        Default::default()
    }
    fn model_registry(&self) -> ModelRegistry {
        Default::default()
    }
    fn model(&self) -> Option<pi_ai::Model> {
        None
    }
    fn scoped_models(&self) -> Vec<ScopedModel> {
        Vec::new()
    }
    fn thinking_level(&self) -> Option<pi_agent_core::types::ThinkingLevel> {
        None
    }
    fn is_idle(&self) -> bool {
        true
    }
    fn is_project_trusted(&self) -> bool {
        true
    }
    fn signal(&self) -> Option<AbortSignal> {
        None
    }
    fn abort(&self) {}
    fn has_pending_messages(&self) -> bool {
        false
    }
    fn shutdown(&self) {}
    fn get_context_usage(&self) -> Option<ContextUsage> {
        None
    }
    fn compact(&self, _: Option<CompactOptions>) {}
    fn get_system_prompt(&self) -> String {
        String::new()
    }
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        Default::default()
    }
    fn wait_for_idle(&self) -> BoxFuture<Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn new_session(
        &self,
        _: Option<pi_coding_agent::core::extensions::types::NewSessionOptions>,
    ) -> BoxFuture<Result<CancelledResult>> {
        Box::pin(async { Ok(CancelledResult { cancelled: false }) })
    }
    fn fork(&self, _: &str, _: Option<ForkOptions>) -> BoxFuture<Result<CancelledResult>> {
        Box::pin(async { Ok(CancelledResult { cancelled: false }) })
    }
    fn navigate_tree(&self, _: &str, _: Option<NavigateTreeOptions>) -> BoxFuture<Result<CancelledResult>> {
        Box::pin(async { Ok(CancelledResult { cancelled: false }) })
    }
    fn switch_session(&self, _: &str, _: Option<SwitchSessionOptions>) -> BoxFuture<Result<CancelledResult>> {
        Box::pin(async { Ok(CancelledResult { cancelled: false }) })
    }
    fn reload(&self) -> BoxFuture<Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn callable_tools(&self) -> Vec<pi_agent_core::AgentTool> {
        Vec::new()
    }
    fn execute_tool(
        &self,
        _: &str,
        _: &str,
        _: Value,
        _: ExecuteToolOptions,
    ) -> BoxFuture<pi_agent_core::AgentToolCallOutcome> {
        Box::pin(async {
            pi_agent_core::AgentToolCallOutcome {
                tool_call: pi_agent_core::AgentToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: IndexMap::new(),
                },
                result: pi_agent_core::AgentToolResult::text(""),
                is_error: false,
                duration_ms: None,
            }
        })
    }
    fn send_message(&self, _: CustomMessageDraft, _: Option<SendMessageOptions>) {}
    fn send_user_message(&self, _: CustomMessageContent, _: Option<SendUserMessageOptions>) {}
}

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn hf_model(id: &str) -> HuggingFaceModel {
    HuggingFaceModel {
        id: id.to_string(),
        downloads: 1000,
    }
}

fn selected_id(view: &dyn Component) -> Option<String> {
    let rendered = strip_ansi(&view.render(120).join("\n"));
    let row = rendered.lines().find(|line| line.contains("→ "))?;
    row.split("→ ").nth(1)?.split(' ').next().map(str::to_string)
}

async fn open_search() -> (
    TestGuard,
    Arc<dyn Component>,
    Arc<Mutex<IndexMap<String, VecDeque<oneshot::Sender<Result<Vec<HuggingFaceModel>>>>>>>,
    oneshot::Receiver<Option<String>>,
    tokio::task::JoinHandle<()>,
) {
    init_theme(Some("dark"), Some(false));
    set_keybindings(KeybindingsManager::new(KEYBINDINGS.clone(), None));
    let tui: Arc<dyn TUI> = Arc::new(TuiMainScreen::new(Arc::new(FakeTerminal), None, None));
    let (view_tx, view_rx) = oneshot::channel();
    let mock_ui = Arc::new(MockCustomUi {
        tui,
        view: Mutex::new(None),
        on_view: Mutex::new(Some(view_tx)),
    });
    let ctx = ExtensionCommandContext {
        base: ExtensionContext::from_backend(Arc::new(MockBackend {
            ui: mock_ui as Arc<dyn ExtensionUIContext>,
        })),
    };

    let pending: Arc<Mutex<IndexMap<String, VecDeque<oneshot::Sender<Result<Vec<HuggingFaceModel>>>>>>> =
        Arc::new(Mutex::new(IndexMap::new()));
    let pending_c = Arc::clone(&pending);
    let search_fn: HfSearchFn = Arc::new(move |query, _signal| {
        let pending_c = Arc::clone(&pending_c);
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            pending_c
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(query)
                .or_default()
                .push_back(tx);
            rx.await.unwrap_or_else(|_| Ok(Vec::new()))
        })
    });

    let (sel_tx, sel_rx) = oneshot::channel();
    let ui_task = tokio::spawn(async move {
        let _ = show_llama_ui(&ctx, move |ui: Arc<dyn LlamaUi>| {
            Box::pin(async move {
                let chosen = ui.search_models(search_fn).await;
                let _ = sel_tx.send(chosen);
                Ok(())
            })
        })
        .await;
    });

    let view = view_rx.await.expect("llama view shown");
    tokio::task::yield_now().await;
    (TestGuard, view, pending, sel_rx, ui_task)
}

async fn type_and_debounce(view: &dyn Component, text: &str) {
    for ch in text.chars() {
        let s = ch.to_string();
        view.handle_input(&s);
    }
    tokio::time::advance(Duration::from_millis(500)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

async fn finish_search(
    pending: &Arc<Mutex<IndexMap<String, VecDeque<oneshot::Sender<Result<Vec<HuggingFaceModel>>>>>>>,
    query: &str,
    results: Vec<HuggingFaceModel>,
) {
    for _ in 0..16 {
        let tx_opt = pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(query)
            .and_then(|q| q.pop_front());
        if let Some(tx) = tx_opt {
            let _ = tx.send(Ok(results));
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("no search pending for {query}");
}

#[tokio::test(start_paused = true)]
#[serial]
async fn keeps_browsed_model_selected_when_next_search_lands() {
    let first_results = vec![
        hf_model("qwen/a-GGUF"),
        hf_model("qwen/b-GGUF"),
        hf_model("qwen/c-GGUF"),
    ];
    let (_guard, view, pending, sel_rx, ui_task) = open_search().await;
    type_and_debounce(&*view, "qw").await;
    finish_search(&pending, "qw", first_results.clone()).await;
    type_and_debounce(&*view, "e").await;
    view.handle_input(DOWN);
    view.handle_input(DOWN);
    assert_eq!(selected_id(&*view).as_deref(), Some("qwen/c-GGUF"));

    let mut second_results = vec![hf_model("qwen/new-GGUF")];
    second_results.extend(first_results);
    finish_search(&pending, "qwe", second_results).await;

    assert_eq!(selected_id(&*view).as_deref(), Some("qwen/c-GGUF"));
    view.handle_input(ENTER);
    assert_eq!(sel_rx.await.ok().flatten().as_deref(), Some("qwen/c-GGUF"));
    let _ = ui_task.await;
}

#[tokio::test(start_paused = true)]
#[serial]
async fn selects_best_match_when_user_did_not_browse() {
    let first_results = vec![
        hf_model("qwen/a-GGUF"),
        hf_model("qwen/b-GGUF"),
        hf_model("qwen/c-GGUF"),
    ];
    let (_guard, view, pending, _sel_rx, ui_task) = open_search().await;
    type_and_debounce(&*view, "qw").await;
    finish_search(&pending, "qw", first_results.clone()).await;
    type_and_debounce(&*view, "e").await;

    let mut second_results = vec![hf_model("qwen/new-GGUF")];
    second_results.extend(first_results);
    finish_search(&pending, "qwe", second_results).await;

    assert_eq!(selected_id(&*view).as_deref(), Some("qwen/new-GGUF"));
    view.handle_input("\x1b");
    let _ = ui_task.await;
}

#[tokio::test(start_paused = true)]
#[serial]
async fn selects_best_match_when_browsed_model_is_missing_from_new_results() {
    let first_results = vec![
        hf_model("qwen/a-GGUF"),
        hf_model("qwen/b-GGUF"),
        hf_model("qwen/c-GGUF"),
    ];
    let (_guard, view, pending, _sel_rx, ui_task) = open_search().await;
    type_and_debounce(&*view, "qw").await;
    finish_search(&pending, "qw", first_results).await;
    type_and_debounce(&*view, "e").await;
    view.handle_input(DOWN);

    finish_search(
        &pending,
        "qwe",
        vec![hf_model("qwen/new-GGUF"), hf_model("qwen/a-GGUF")],
    )
    .await;

    assert_eq!(selected_id(&*view).as_deref(), Some("qwen/new-GGUF"));
    view.handle_input("\x1b");
    let _ = ui_task.await;
}

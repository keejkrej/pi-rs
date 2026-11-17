//! Port of packages/coding-agent/test/theme-controller.test.ts

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::modes::interactive::theme::theme::{
    ThemeColor, ThemeToken, init_theme, set_terminal_color_scheme, set_terminal_colors, theme,
};
use pi_coding_agent::modes::interactive::theme::theme_controller::{
    InteractiveThemeController, InteractiveThemeControllerOptions, ThemeResult, ThemeSettingsSource,
};
use pi_tui::tui::{
    OverlayHandle, OverlayOptions, QueryTerminalColorsOptions, TUI, TuiInputListener, TuiMode, TuiStopOptions,
};
use pi_tui::{Component, RgbColor, Terminal, TerminalColorScheme, TerminalColors};
use pretty_assertions::assert_eq;
use serial_test::serial;

fn dark_colors() -> TerminalColors {
    TerminalColors {
        foreground: Some(RgbColor {
            r: 248.0,
            g: 248.0,
            b: 242.0,
        }),
        background: Some(RgbColor {
            r: 40.0,
            g: 42.0,
            b: 54.0,
        }),
        palette: None,
    }
}

fn light_colors() -> TerminalColors {
    TerminalColors {
        foreground: Some(RgbColor {
            r: 30.0,
            g: 30.0,
            b: 30.0,
        }),
        background: Some(RgbColor {
            r: 250.0,
            g: 250.0,
            b: 250.0,
        }),
        palette: None,
    }
}

type QueryFuture = Pin<Box<dyn std::future::Future<Output = TerminalColors> + Send>>;
type QueryImpl = Arc<dyn Fn(QueryTerminalColorsOptions) -> QueryFuture + Send + Sync>;

struct MockSettingsManager {
    theme_setting: Mutex<Option<String>>,
    set_theme_calls: Mutex<Vec<String>>,
    flush_calls: Mutex<usize>,
}

impl MockSettingsManager {
    fn in_memory(initial: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            theme_setting: Mutex::new(initial.map(str::to_string)),
            set_theme_calls: Mutex::new(Vec::new()),
            flush_calls: Mutex::new(0),
        })
    }

    fn apply_override_theme(&self, theme: &str) {
        *self.theme_setting.lock().unwrap() = Some(theme.to_string());
    }

    #[allow(dead_code)]
    fn set_theme(&self, theme: &str) {
        self.set_theme_calls.lock().unwrap().push(theme.to_string());
        *self.theme_setting.lock().unwrap() = Some(theme.to_string());
    }

    #[allow(dead_code)]
    fn flush(&self) {
        *self.flush_calls.lock().unwrap() += 1;
    }
}

impl ThemeSettingsSource for MockSettingsManager {
    fn get_theme_setting(&self) -> Option<String> {
        self.theme_setting.lock().unwrap().clone()
    }
}

struct MockTerminal;

#[async_trait]
impl Terminal for MockTerminal {
    fn start(&self, _: Arc<dyn Fn(&str) + Send + Sync>, _: Arc<dyn Fn() + Send + Sync>) {}
    fn stop(&self) {}
    async fn drain_input(&self, _: Option<i64>, _: Option<i64>) {}
    fn write(&self, _: &str) {}
    fn columns(&self) -> i64 {
        80
    }
    fn rows(&self) -> i64 {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&self, _: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _: &str) {}
    fn set_progress(&self, _: bool) {}
}

struct MockUiState {
    request_render_calls: usize,
    query_calls: usize,
    notifications_history: Vec<bool>,
    scheme_listener: Option<Arc<dyn Fn(TerminalColorScheme) + Send + Sync>>,
    query_impl: QueryImpl,
}

struct MockUi {
    state: Mutex<MockUiState>,
}

impl MockUi {
    fn new() -> Arc<Self> {
        let default_impl: QueryImpl = Arc::new(|_| Box::pin(async { TerminalColors::default() }));
        Arc::new(Self {
            state: Mutex::new(MockUiState {
                request_render_calls: 0,
                query_calls: 0,
                notifications_history: Vec::new(),
                scheme_listener: None,
                query_impl: default_impl,
            }),
        })
    }

    fn set_query_resolved(&self, colors: TerminalColors) {
        let impl_fn: QueryImpl = Arc::new(move |_| {
            let c = colors.clone();
            Box::pin(async move { c })
        });
        self.state.lock().unwrap().query_impl = impl_fn;
    }

    fn set_query_impl(&self, impl_fn: QueryImpl) {
        self.state.lock().unwrap().query_impl = impl_fn;
    }

    fn emit_terminal_color_scheme(&self, scheme: TerminalColorScheme) {
        let listener = self.state.lock().unwrap().scheme_listener.clone();
        if let Some(listener) = listener {
            listener(scheme);
        }
    }
}

impl Component for MockUi {
    fn render(&self, _: usize) -> Vec<String> {
        Vec::new()
    }
    fn invalidate(&self) {}
}

#[async_trait]
impl TUI for MockUi {
    fn mode(&self) -> TuiMode {
        TuiMode::Regular
    }
    fn children(&self) -> Vec<Arc<dyn Component>> {
        Vec::new()
    }
    fn set_children(&self, _: Vec<Arc<dyn Component>>) {}
    fn terminal(&self) -> Arc<dyn Terminal> {
        Arc::new(MockTerminal)
    }
    fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        None
    }
    fn set_on_debug(&self, _: Option<Arc<dyn Fn() + Send + Sync>>) {}
    fn full_redraws(&self) -> i64 {
        0
    }
    fn add_child(&self, _: Arc<dyn Component>) {}
    fn remove_child(&self, _: &Arc<dyn Component>) {}
    fn clear(&self) {}
    fn get_show_hardware_cursor(&self) -> bool {
        false
    }
    fn set_show_hardware_cursor(&self, _: bool) {}
    fn get_clear_on_shrink(&self) -> bool {
        false
    }
    fn set_clear_on_shrink(&self, _: bool) {}
    fn set_focus(&self, _: Option<Arc<dyn Component>>) {}
    fn show_overlay(&self, _: Arc<dyn Component>, _: Option<OverlayOptions>) -> OverlayHandle {
        unreachable!("not used in theme_controller tests")
    }
    fn hide_overlay(&self) {}
    fn has_overlay(&self) -> bool {
        false
    }
    fn start(&self) {}
    fn stop(&self, _: Option<TuiStopOptions>) {}
    fn render_now(&self, _: Option<bool>) {}
    fn request_render(&self, _: Option<bool>) {
        self.state.lock().unwrap().request_render_calls += 1;
    }
    fn add_input_listener(&self, _: TuiInputListener) -> pi_js::Unsubscribe {
        pi_js::Unsubscribe::noop()
    }
    fn remove_input_listener(&self, _: &TuiInputListener) {}
    fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        let mut st = self.state.lock().unwrap();
        st.scheme_listener = Some(listener);
        // SAFETY/test-only: cannot easily hold Weak inside static without Arc, clone self isn't possible from &self;
        // use shared counter instead.
        drop(st);
        pi_js::Unsubscribe::noop()
    }
    fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        self.state.lock().unwrap().notifications_history.push(enabled);
    }
    async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        let fut = {
            let mut st = self.state.lock().unwrap();
            st.query_calls += 1;
            (st.query_impl)(options)
        };
        fut.await
    }
}

/// Wrapper allowing unsubscribe call tracking on `MockUi`.
struct TrackedMockUi {
    inner: Arc<MockUi>,
    unsubscribe_calls: Arc<Mutex<usize>>,
}

impl TrackedMockUi {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MockUi::new(),
            unsubscribe_calls: Arc::new(Mutex::new(0)),
        })
    }
}

impl Component for TrackedMockUi {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }
    fn invalidate(&self) {
        self.inner.invalidate();
    }
}

#[async_trait]
impl TUI for TrackedMockUi {
    fn mode(&self) -> TuiMode {
        self.inner.mode()
    }
    fn children(&self) -> Vec<Arc<dyn Component>> {
        self.inner.children()
    }
    fn set_children(&self, c: Vec<Arc<dyn Component>>) {
        self.inner.set_children(c);
    }
    fn terminal(&self) -> Arc<dyn Terminal> {
        self.inner.terminal()
    }
    fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.inner.on_debug()
    }
    fn set_on_debug(&self, d: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.set_on_debug(d);
    }
    fn full_redraws(&self) -> i64 {
        self.inner.full_redraws()
    }
    fn add_child(&self, c: Arc<dyn Component>) {
        self.inner.add_child(c);
    }
    fn remove_child(&self, c: &Arc<dyn Component>) {
        self.inner.remove_child(c);
    }
    fn clear(&self) {
        self.inner.clear();
    }
    fn get_show_hardware_cursor(&self) -> bool {
        self.inner.get_show_hardware_cursor()
    }
    fn set_show_hardware_cursor(&self, enabled: bool) {
        self.inner.set_show_hardware_cursor(enabled);
    }
    fn get_clear_on_shrink(&self) -> bool {
        self.inner.get_clear_on_shrink()
    }
    fn set_clear_on_shrink(&self, enabled: bool) {
        self.inner.set_clear_on_shrink(enabled);
    }
    fn set_focus(&self, c: Option<Arc<dyn Component>>) {
        self.inner.set_focus(c);
    }
    fn show_overlay(&self, c: Arc<dyn Component>, o: Option<OverlayOptions>) -> OverlayHandle {
        self.inner.show_overlay(c, o)
    }
    fn hide_overlay(&self) {
        self.inner.hide_overlay();
    }
    fn has_overlay(&self) -> bool {
        self.inner.has_overlay()
    }
    fn start(&self) {
        self.inner.start();
    }
    fn stop(&self, o: Option<TuiStopOptions>) {
        self.inner.stop(o);
    }
    fn render_now(&self, f: Option<bool>) {
        self.inner.render_now(f);
    }
    fn request_render(&self, f: Option<bool>) {
        self.inner.request_render(f);
    }
    fn add_input_listener(&self, l: TuiInputListener) -> pi_js::Unsubscribe {
        self.inner.add_input_listener(l)
    }
    fn remove_input_listener(&self, l: &TuiInputListener) {
        self.inner.remove_input_listener(l);
    }
    fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        self.inner.state.lock().unwrap().scheme_listener = Some(listener);
        let counter = Arc::clone(&self.unsubscribe_calls);
        pi_js::Unsubscribe::new(move || {
            *counter.lock().unwrap() += 1;
        })
    }
    fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        self.inner.set_terminal_color_scheme_notifications(enabled);
    }
    async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        self.inner.query_terminal_colors(options).await
    }
}

struct ThemeResetGuard {
    prev_colorfgbg: Option<String>,
}

impl ThemeResetGuard {
    fn new() -> Self {
        Self {
            prev_colorfgbg: pi_js::env::var("COLORFGBG"),
        }
    }
}

impl Drop for ThemeResetGuard {
    fn drop(&mut self) {
        set_terminal_colors(TerminalColors::default());
        set_terminal_color_scheme(None);
        init_theme(Some("dark"), None);
        match &self.prev_colorfgbg {
            Some(v) => pi_js::env::set_var("COLORFGBG", v),
            None => pi_js::env::remove_var("COLORFGBG"),
        }
    }
}

fn create_controller(
    ui: Arc<TrackedMockUi>,
    get_settings_manager: Arc<dyn Fn() -> Arc<dyn ThemeSettingsSource> + Send + Sync>,
    initial_theme_setting: Option<&str>,
) -> InteractiveThemeController {
    InteractiveThemeController::new(
        ui,
        InteractiveThemeControllerOptions {
            get_settings_manager,
            show_error: Arc::new(|_| {}),
            on_changed: Arc::new(|| {}),
            initial_theme_setting: initial_theme_setting.map(str::to_string),
        },
    )
}

async fn flush() {
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
#[serial]
async fn uses_the_initial_theme_without_persisting_it() {
    let _guard = ThemeResetGuard::new();
    let ui = TrackedMockUi::new();
    let manager = MockSettingsManager::in_memory(Some("dark"));
    let mgr_for_cb = Arc::clone(&manager);
    let controller = create_controller(
        Arc::clone(&ui),
        Arc::new(move || Arc::clone(&mgr_for_cb) as Arc<dyn ThemeSettingsSource>),
        Some("light"),
    );

    assert_eq!(theme().name(), Some("light"));
    assert_eq!(controller.get_theme_selection().as_deref(), Some("light"));
    controller.apply_from_settings();
    flush().await;

    assert_eq!(ui.inner.state.lock().unwrap().query_calls, 1);
    assert!(manager.set_theme_calls.lock().unwrap().is_empty());
    assert_eq!(*manager.flush_calls.lock().unwrap(), 0);
}

#[tokio::test]
#[serial]
async fn applies_the_theme_immediately_and_lets_startup_wait_for_the_colors() {
    let _guard = ThemeResetGuard::new();
    let ui = TrackedMockUi::new();
    let (tx, rx) = tokio::sync::oneshot::channel::<TerminalColors>();
    let rx_slot = Arc::new(Mutex::new(VecDeque::from([rx])));
    ui.inner.set_query_impl(Arc::new(move |_| {
        let rx = rx_slot.lock().unwrap().pop_front().unwrap();
        Box::pin(async move { rx.await.unwrap_or_default() })
    }));

    let manager = MockSettingsManager::in_memory(None);
    let controller = create_controller(
        Arc::clone(&ui),
        Arc::new(move || Arc::clone(&manager) as Arc<dyn ThemeSettingsSource>),
        None,
    );
    controller.apply_from_settings();

    // Grayscale until the terminal answers.
    assert_eq!(theme().name(), Some("system"));
    assert_eq!(theme().get_fg_ansi(ThemeColor::Error), "\x1b[39m");

    tx.send(dark_colors()).unwrap();
    controller.wait_for_terminal_colors().await;
    assert!(theme().get_fg_ansi(ThemeColor::Error).starts_with("\x1b[38;"));
}

#[tokio::test]
#[serial]
async fn falls_back_to_palette_indices_then_applies_colors_that_arrive_after_the_timeout() {
    let _guard = ThemeResetGuard::new();
    let ui = TrackedMockUi::new();
    let late_reply_slot: Arc<Mutex<Option<Arc<dyn Fn(TerminalColors) + Send + Sync>>>> = Arc::new(Mutex::new(None));
    let late_clone = Arc::clone(&late_reply_slot);
    ui.inner.set_query_impl(Arc::new(move |options| {
        *late_clone.lock().unwrap() = options.on_late_reply;
        Box::pin(async { TerminalColors::default() })
    }));

    let manager = MockSettingsManager::in_memory(None);
    let controller = create_controller(
        Arc::clone(&ui),
        Arc::new(move || Arc::clone(&manager) as Arc<dyn ThemeSettingsSource>),
        None,
    );
    controller.apply_from_settings();
    flush().await;
    assert_eq!(theme().get_fg_ansi(ThemeColor::Error), "\x1b[38;5;1m");

    let late_reply = late_reply_slot.lock().unwrap().take().unwrap();
    late_reply(dark_colors());
    let err_color = theme().color(ThemeToken::Fg(ThemeColor::Error)).unwrap();
    assert!(matches!(err_color, pi_tui::Color::Rgb(_)));
}

#[tokio::test]
#[serial]
async fn re_queries_the_colors_on_appearance_changes_and_lets_them_decide() {
    let _guard = ThemeResetGuard::new();
    let ui = TrackedMockUi::new();
    ui.inner.set_query_resolved(light_colors());

    let manager = MockSettingsManager::in_memory(None);
    let controller = create_controller(
        Arc::clone(&ui),
        Arc::new(move || Arc::clone(&manager) as Arc<dyn ThemeSettingsSource>),
        Some("light/dark"),
    );
    controller.apply_from_settings();
    assert_eq!(ui.inner.state.lock().unwrap().notifications_history, vec![true]);
    flush().await;
    assert_eq!(theme().name(), Some("light"));

    ui.inner.set_query_resolved(dark_colors());
    // The report says light, but the terminal renders dark.
    ui.inner.emit_terminal_color_scheme(TerminalColorScheme::Light);
    flush().await;
    assert_eq!(theme().name(), Some("dark"));
}

#[tokio::test]
#[serial]
async fn uses_the_reported_scheme_for_the_system_theme_when_the_terminal_reports_no_colors() {
    let _guard = ThemeResetGuard::new();
    pi_js::env::set_var("COLORFGBG", "");
    let ui = TrackedMockUi::new();
    let manager = MockSettingsManager::in_memory(None);
    let controller = create_controller(
        Arc::clone(&ui),
        Arc::new(move || Arc::clone(&manager) as Arc<dyn ThemeSettingsSource>),
        None,
    );
    controller.apply_from_settings();
    flush().await;
    assert_eq!(
        theme().appearance(),
        pi_coding_agent::modes::interactive::theme::theme::ThemeAppearance::Dark
    );

    ui.inner.emit_terminal_color_scheme(TerminalColorScheme::Light);
    assert_eq!(
        theme().appearance(),
        pi_coding_agent::modes::interactive::theme::theme::ThemeAppearance::Light
    );
    assert_eq!(
        controller.get_terminal_theme(),
        pi_coding_agent::modes::interactive::theme::theme::ThemeAppearance::Light
    );
}

#[tokio::test]
#[serial]
async fn re_renders_only_when_the_reported_colors_change() {
    let _guard = ThemeResetGuard::new();
    let ui = TrackedMockUi::new();
    let manager = MockSettingsManager::in_memory(Some("dark"));
    let controller = create_controller(
        Arc::clone(&ui),
        Arc::new(move || Arc::clone(&manager) as Arc<dyn ThemeSettingsSource>),
        None,
    );

    for colors in [dark_colors(), TerminalColors::default(), dark_colors()] {
        ui.inner.set_query_resolved(colors);
        controller.apply_from_settings();
        flush().await;
    }

    assert_eq!(ui.inner.state.lock().unwrap().request_render_calls, 1);
}

#[tokio::test]
#[serial]
async fn disables_terminal_appearance_updates_when_disposed() {
    let _guard = ThemeResetGuard::new();
    let ui = TrackedMockUi::new();
    let manager = MockSettingsManager::in_memory(Some("light/dark"));
    let controller = create_controller(
        Arc::clone(&ui),
        Arc::new(move || Arc::clone(&manager) as Arc<dyn ThemeSettingsSource>),
        None,
    );
    controller.apply_from_settings();
    flush().await;

    controller.dispose();

    assert_eq!(
        ui.inner.state.lock().unwrap().notifications_history.last().copied(),
        Some(false)
    );
    assert_eq!(*ui.unsubscribe_calls.lock().unwrap(), 1);
}

#[tokio::test]
#[serial]
async fn lets_an_explicit_selection_replace_the_initial_theme() {
    let _guard = ThemeResetGuard::new();
    let ui = TrackedMockUi::new();
    let first_manager = MockSettingsManager::in_memory(Some("dark"));
    let second_manager = MockSettingsManager::in_memory(Some("light"));
    let current_manager: Arc<Mutex<Arc<MockSettingsManager>>> = Arc::new(Mutex::new(first_manager));
    let mgr_ref = Arc::clone(&current_manager);
    let controller = create_controller(
        Arc::clone(&ui),
        Arc::new(move || Arc::clone(&*mgr_ref.lock().unwrap()) as Arc<dyn ThemeSettingsSource>),
        Some("light"),
    );
    controller.apply_from_settings();

    assert_eq!(
        controller.set_theme_name("dark", false),
        ThemeResult {
            success: true,
            error: None
        }
    );
    *current_manager.lock().unwrap() = second_manager;
    controller.apply_from_settings();
    flush().await;

    assert_eq!(controller.get_theme_selection().as_deref(), Some("dark"));
    assert_eq!(theme().name(), Some("dark"));
}

#[tokio::test]
#[serial]
async fn reloads_theme_settings_when_no_initial_theme_was_supplied() {
    let _guard = ThemeResetGuard::new();
    let ui = TrackedMockUi::new();
    let first_manager = MockSettingsManager::in_memory(Some("dark"));
    let second_manager = MockSettingsManager::in_memory(Some("light"));
    let current_manager: Arc<Mutex<Arc<MockSettingsManager>>> = Arc::new(Mutex::new(Arc::clone(&first_manager)));
    let mgr_ref = Arc::clone(&current_manager);
    let controller = create_controller(
        Arc::clone(&ui),
        Arc::new(move || Arc::clone(&*mgr_ref.lock().unwrap()) as Arc<dyn ThemeSettingsSource>),
        None,
    );
    controller.apply_from_settings();

    first_manager.apply_override_theme("light");
    controller.apply_from_settings();
    assert_eq!(theme().name(), Some("light"));

    second_manager.apply_override_theme("dark");
    *current_manager.lock().unwrap() = second_manager;
    controller.apply_from_settings();
    assert_eq!(theme().name(), Some("dark"));
}

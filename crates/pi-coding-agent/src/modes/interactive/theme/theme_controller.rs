//! Port of packages/coding-agent/src/modes/interactive/theme/theme-controller.ts

use std::sync::{Arc, Mutex};

use pi_tui::tui::QueryTerminalColorsOptions;
use pi_tui::{RgbColor, TUI, TerminalColorScheme, TerminalColors};
use tokio::sync::watch;

use super::theme::{
    SYSTEM_THEME_NAME, SetThemeResult, TerminalTheme, Theme, get_terminal_theme, init_theme,
    mark_terminal_colors_pending, parse_auto_theme_setting, resolve_theme_setting, set_terminal_color_scheme,
    set_terminal_colors, set_theme, set_theme_instance,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeResult {
    pub success: bool,
    pub error: Option<String>,
}

impl From<SetThemeResult> for ThemeResult {
    fn from(result: SetThemeResult) -> Self {
        Self {
            success: result.success,
            error: result.error,
        }
    }
}

/// Settings subset needed by [`InteractiveThemeController`].
pub trait ThemeSettingsSource: Send + Sync {
    fn get_theme_setting(&self) -> Option<String>;
}

impl ThemeSettingsSource for crate::core::settings_manager::SettingsManager {
    fn get_theme_setting(&self) -> Option<String> {
        Self::get_theme(self)
    }
}

/// How long the system theme stays grayscale before falling back to palette indices. Terminals answer
/// the trailing DA1 request right after the color replies, so this only matters for terminals that
/// answer neither. Replies arriving later still apply.
const TERMINAL_QUERY_TIMEOUT_MS: i64 = 100;

/// Query the terminal's colors and pass them to `apply` when the query completes or times out, and again
/// if the terminal answers after the timeout. A failed query applies no colors. Settles after the first apply.
pub async fn request_terminal_colors(ui: &(dyn TUI + Send + Sync), apply: Arc<dyn Fn(TerminalColors) + Send + Sync>) {
    let colors = ui
        .query_terminal_colors(QueryTerminalColorsOptions {
            timeout_ms: TERMINAL_QUERY_TIMEOUT_MS,
            on_late_reply: Some(Arc::clone(&apply)),
        })
        .await;
    apply(colors);
}

fn same_rgb(a: Option<RgbColor>, b: Option<RgbColor>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.r == b.r && a.g == b.g && a.b == b.b,
        _ => false,
    }
}

fn same_terminal_colors(a: &TerminalColors, b: &TerminalColors) -> bool {
    if !same_rgb(a.foreground, b.foreground) || !same_rgb(a.background, b.background) {
        return false;
    }
    match (&a.palette, &b.palette) {
        (None, None) => true,
        (Some(pa), Some(pb)) => {
            pa.len() == pb.len() && pa.iter().zip(pb.iter()).all(|(&ca, &cb)| same_rgb(Some(ca), Some(cb)))
        }
        _ => false,
    }
}

fn scheme_to_terminal_theme(scheme: TerminalColorScheme) -> TerminalTheme {
    match scheme {
        TerminalColorScheme::Dark => TerminalTheme::Dark,
        TerminalColorScheme::Light => TerminalTheme::Light,
    }
}

pub struct InteractiveThemeControllerOptions {
    pub get_settings_manager: Arc<dyn Fn() -> Arc<dyn ThemeSettingsSource> + Send + Sync>,
    pub show_error: Arc<dyn Fn(&str) + Send + Sync>,
    pub on_changed: Arc<dyn Fn() + Send + Sync>,
    pub initial_theme_setting: Option<String>,
}

struct ThemeControllerState {
    current_theme_setting: Option<String>,
    /// Last reported colors; a query that times out keeps them instead of erasing them.
    terminal_colors: Option<TerminalColors>,
    active_theme_name: Option<String>,
    auto_sync_enabled: bool,
    terminal_color_scheme_unsubscribe: Option<pi_js::Unsubscribe>,
    /// Signals when the latest color query completed or timed out, and its colors applied.
    terminal_color_query_rx: watch::Receiver<bool>,
}

struct InteractiveThemeControllerInner {
    ui: Arc<dyn TUI + Send + Sync>,
    get_settings_manager: Arc<dyn Fn() -> Arc<dyn ThemeSettingsSource> + Send + Sync>,
    show_error: Arc<dyn Fn(&str) + Send + Sync>,
    on_changed: Arc<dyn Fn() + Send + Sync>,
    state: Mutex<ThemeControllerState>,
}

/// Applies the theme setting and keeps it in sync with the terminal. The theme applies immediately, and the
/// terminal's colors update it when they arrive; the system theme renders in grayscale until then. Callers
/// that bake theme colors into content can wait for the colors with [`Self::wait_for_terminal_colors`].
#[derive(Clone)]
pub struct InteractiveThemeController {
    inner: Arc<InteractiveThemeControllerInner>,
}

impl InteractiveThemeController {
    pub fn new(ui: Arc<dyn TUI + Send + Sync>, options: InteractiveThemeControllerOptions) -> Self {
        let (_tx, rx) = watch::channel(true);
        let controller = Self {
            inner: Arc::new(InteractiveThemeControllerInner {
                ui,
                get_settings_manager: options.get_settings_manager,
                show_error: options.show_error,
                on_changed: options.on_changed,
                state: Mutex::new(ThemeControllerState {
                    current_theme_setting: options.initial_theme_setting,
                    terminal_colors: None,
                    active_theme_name: None,
                    auto_sync_enabled: false,
                    terminal_color_scheme_unsubscribe: None,
                    terminal_color_query_rx: rx,
                }),
            }),
        };

        let active_theme_name = controller.resolve_theme_name();
        {
            let mut state = controller.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.active_theme_name = Some(active_theme_name.clone());
        }
        // The system theme starts in grayscale; color follows once the terminal reports its colors.
        mark_terminal_colors_pending();
        init_theme(Some(&active_theme_name), Some(true));
        controller.bind_terminal_color_scheme_listener();
        controller
    }

    pub fn rebind_tui(&self) {
        let prev = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.terminal_color_scheme_unsubscribe.take()
        };
        if let Some(unsub) = prev {
            unsub.call();
        }
        self.bind_terminal_color_scheme_listener();
        let enabled = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .auto_sync_enabled;
        self.inner.ui.set_terminal_color_scheme_notifications(enabled);
    }

    /// Apply the theme setting now and query the terminal's colors, which update the theme when they arrive.
    /// Theme pairs and the system theme follow terminal appearance changes.
    pub fn apply_from_settings(&self) {
        let theme_setting = self.get_theme_setting();
        let theme_name = self.resolve_theme_name_with_setting(theme_setting.as_deref());
        let auto_sync = parse_auto_theme_setting(theme_setting.as_deref()).is_some() || theme_name == SYSTEM_THEME_NAME;
        self.set_auto_sync(auto_sync);
        self.apply_theme_name(&theme_name, theme_setting.is_some());
        self.query_terminal_colors();
    }

    /// Wait until the latest color query completed or timed out. Content that bakes theme colors into
    /// strings, such as the startup header, should be built after this. Terminals answer the DA1 request
    /// right after the color replies, so this only takes the full timeout when a terminal answers nothing.
    pub async fn wait_for_terminal_colors(&self) {
        let mut rx = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.terminal_color_query_rx.clone()
        };
        let _ = rx.wait_for(|&done| done).await;
    }

    pub fn get_theme_selection(&self) -> Option<String> {
        let (current_setting, active_name) = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.current_theme_setting.clone(), state.active_theme_name.clone())
        };
        if let Some(setting) = current_setting {
            return Some(setting);
        }
        let from_settings = (self.inner.get_settings_manager)().get_theme_setting();
        from_settings.or(active_name)
    }

    pub fn set_theme_name(&self, theme_name: &str, show_error: bool) -> ThemeResult {
        self.set_auto_sync(theme_name == SYSTEM_THEME_NAME);
        let result = self.apply_theme_name(theme_name, show_error);
        if result.success {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.current_theme_setting = Some(theme_name.to_string());
        }
        result
    }

    pub fn set_theme_setting(&self, theme_setting: &str) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.current_theme_setting = Some(theme_setting.to_string());
        }
        self.apply_from_settings();
    }

    pub fn set_theme_instance(&self, theme_instance: Theme) -> ThemeResult {
        self.set_auto_sync(false);
        set_theme_instance(theme_instance);
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.active_theme_name = Some("<in-memory>".to_string());
        }
        self.notify_changed();
        ThemeResult {
            success: true,
            error: None,
        }
    }

    pub fn preview(&self, theme_setting_or_name: &str) {
        let active_name = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.active_theme_name.clone()
        };
        let Some(theme_name) = resolve_theme_setting(Some(theme_setting_or_name), get_terminal_theme()).or(active_name)
        else {
            return;
        };
        if set_theme(&theme_name, Some(true)).success {
            self.inner.ui.invalidate();
            self.inner.ui.request_render(None);
        }
    }

    pub fn disable_auto_sync(&self) {
        self.set_auto_sync(false);
    }

    pub fn dispose(&self) {
        self.set_auto_sync(false);
        let unsub = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.terminal_color_scheme_unsubscribe.take()
        };
        if let Some(unsub) = unsub {
            unsub.call();
        }
    }

    pub fn get_terminal_theme(&self) -> TerminalTheme {
        get_terminal_theme()
    }

    fn get_theme_setting(&self) -> Option<String> {
        let current = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.current_theme_setting.clone()
        };
        current.or_else(|| (self.inner.get_settings_manager)().get_theme_setting())
    }

    fn resolve_theme_name(&self) -> String {
        let theme_setting = self.get_theme_setting();
        self.resolve_theme_name_with_setting(theme_setting.as_deref())
    }

    /// The theme for the current setting and terminal appearance. Without a setting, pi uses the system theme.
    fn resolve_theme_name_with_setting(&self, theme_setting: Option<&str>) -> String {
        resolve_theme_setting(theme_setting, get_terminal_theme()).unwrap_or_else(|| SYSTEM_THEME_NAME.to_string())
    }

    fn apply_theme_name(&self, theme_name: &str, show_error: bool) -> ThemeResult {
        let result: ThemeResult = set_theme(theme_name, Some(true)).into();
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.active_theme_name = Some(if result.success {
                theme_name.to_string()
            } else {
                SYSTEM_THEME_NAME.to_string()
            });
        }
        self.notify_changed();
        if !result.success && show_error {
            let error_msg = result.error.as_deref().unwrap_or("Unknown error");
            (self.inner.show_error)(&format!(
                "Failed to load theme \"{theme_name}\": {error_msg}\nFell back to the system theme."
            ));
        }
        result
    }

    /// Query the terminal's colors without waiting for them; `wait_for_terminal_colors()` waits for this query.
    fn query_terminal_colors(&self) {
        let (tx, rx) = watch::channel(false);
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.terminal_color_query_rx = rx;
        }
        let this = self.clone();
        let ui = Arc::clone(&self.inner.ui);
        let apply: Arc<dyn Fn(TerminalColors) + Send + Sync> = Arc::new(move |colors| {
            this.apply_terminal_colors(colors);
        });
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                request_terminal_colors(ui.as_ref(), apply).await;
                let _ = tx.send(true);
            });
        } else {
            apply(TerminalColors::default());
            let _ = tx.send(true);
        }
    }

    /// Record reported colors: themes use the default colors for tokens set to "", the system theme is
    /// generated from all of them, and light/dark detection uses them. Re-renders only when they changed.
    fn apply_terminal_colors(&self, reported: TerminalColors) {
        let next = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let previous = state.terminal_colors.as_ref();
            let next = TerminalColors {
                foreground: reported.foreground.or(previous.and_then(|p| p.foreground)),
                background: reported.background.or(previous.and_then(|p| p.background)),
                palette: reported.palette.or_else(|| previous.and_then(|p| p.palette.clone())),
            };
            // Re-rendering rebuilds every component, so skip it when nothing changed (including timeouts).
            if previous.is_some_and(|prev| same_terminal_colors(prev, &next)) {
                return;
            }
            state.terminal_colors = Some(next.clone());
            next
        };
        set_terminal_colors(next);
        self.reapply_for_terminal();
        self.inner.ui.invalidate();
        self.inner.ui.request_render(None);
    }

    /// Re-apply the setting after the terminal's colors or appearance changed: regenerate the system theme,
    /// or switch the theme of a pair. Themes set through extensions or previews are left alone.
    fn reapply_for_terminal(&self) {
        let active_theme_name = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.active_theme_name.clone()
        };
        if active_theme_name.as_deref() == Some("<in-memory>") {
            return;
        }
        let theme_name = self.resolve_theme_name();
        if theme_name == SYSTEM_THEME_NAME || Some(theme_name.as_str()) != active_theme_name.as_deref() {
            self.apply_theme_name(&theme_name, false);
        }
    }

    fn set_auto_sync(&self, enabled: bool) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.auto_sync_enabled == enabled {
                return;
            }
            state.auto_sync_enabled = enabled;
        }
        self.inner.ui.set_terminal_color_scheme_notifications(enabled);
    }

    fn bind_terminal_color_scheme_listener(&self) {
        let this = self.clone();
        let unsub = self.inner.ui.on_terminal_color_scheme_change(Arc::new(move |scheme| {
            this.apply_terminal_color_scheme_change(scheme_to_terminal_theme(scheme));
        }));
        let prev = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.terminal_color_scheme_unsubscribe.replace(unsub)
        };
        if let Some(prev_unsub) = prev {
            prev_unsub.call();
        }
    }

    /// The terminal reported a light/dark switch. Its colors changed too, so query them again: they decide
    /// the appearance. The reported scheme only matters for terminals that do not report their background.
    fn apply_terminal_color_scheme_change(&self, terminal_theme: TerminalTheme) {
        let auto_sync = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.auto_sync_enabled
        };
        if !auto_sync {
            return;
        }
        let previous = get_terminal_theme();
        set_terminal_color_scheme(Some(terminal_theme));
        if get_terminal_theme() != previous {
            self.reapply_for_terminal();
        }
        self.query_terminal_colors();
    }

    fn notify_changed(&self) {
        self.inner.ui.invalidate();
        (self.inner.on_changed)();
    }
}

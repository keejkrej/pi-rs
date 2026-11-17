//! Port of packages/coding-agent/src/modes/interactive/components/oauth-selector.ts

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexSet;
use pi_ai::auth::types::{ApiKeyAuth, AuthCheck, AuthType, OAuthAuth};
use pi_js::time::Interval;
use pi_tui::{
    Component, Container, Focusable, Input, Spacer, TUI, TruncatedText, TuiMouseEvent, TuiMouseEventResult,
    fuzzy_filter, get_keybindings,
};
use regex::Regex;

use super::super::theme::theme::{ThemeColor, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::get_select_page_index;

pub(crate) mod provider_brand {
    use std::cmp::Ordering;
    use std::sync::{Arc, LazyLock};

    use pi_js::intl::locale_compare;
    use pi_js::time::{Interval, set_interval};
    use pi_tui::TUI;
    use pi_tui::colors::{Color, ColorMixSpace, ParseColorValue, foreground_ansi, mix_colors, parse_color};

    use super::super::super::theme::theme::{ThemeAppearance, theme};

    pub(crate) const RADIUS_PROVIDER_ID: &str = "radius";
    pub(crate) const PREFERRED_PROVIDER_IDS: &[&str] = &[RADIUS_PROVIDER_ID, "openrouter"];

    pub(crate) fn is_preferred_provider(provider_id: &str) -> bool {
        PREFERRED_PROVIDER_IDS.contains(&provider_id)
    }

    #[allow(dead_code)]
    fn preferred_rank(provider_id: &str) -> usize {
        PREFERRED_PROVIDER_IDS
            .iter()
            .position(|&id| id == provider_id)
            .unwrap_or(PREFERRED_PROVIDER_IDS.len())
    }

    #[allow(dead_code)]
    pub(crate) fn compare_providers(a_id: &str, a_name: &str, b_id: &str, b_name: &str) -> Ordering {
        preferred_rank(a_id)
            .cmp(&preferred_rank(b_id))
            .then_with(|| locale_compare(a_name, b_name))
    }

    pub(crate) type ProviderBrandStyle = fn(&str, f64) -> String;

    pub(crate) fn get_provider_brand_style(provider_id: &str) -> Option<ProviderBrandStyle> {
        if provider_id == RADIUS_PROVIDER_ID {
            return Some(radius_shimmer);
        }
        if provider_id == "openrouter" {
            return Some(open_router_glow);
        }
        None
    }

    static RADIUS_COLORS: LazyLock<[Color; 4]> = LazyLock::new(|| {
        [
            parse_color(ParseColorValue::Text("#4d9abf")).expect("radius color 0 parses"),
            parse_color(ParseColorValue::Text("#83ccd2")).expect("radius color 1 parses"),
            parse_color(ParseColorValue::Text("#f1be57")).expect("radius color 2 parses"),
            parse_color(ParseColorValue::Text("#f09082")).expect("radius color 3 parses"),
        ]
    });

    const CHARS_PER_COLOR: f64 = 4.0;
    const CHARS_PER_SECOND: f64 = 10.0;

    pub(crate) fn radius_shimmer(text: &str, elapsed_ms: f64) -> String {
        let mode = theme().get_color_mode();
        let cycle = (RADIUS_COLORS.len() as f64) * CHARS_PER_COLOR;
        let offset = (elapsed_ms / 1000.0) * CHARS_PER_SECOND;
        let mut result = String::new();
        for (index, ch) in text.chars().enumerate() {
            let position = ((((index as f64) - offset) % cycle) + cycle) % cycle;
            let band = ((position / CHARS_PER_COLOR).floor() as usize) % RADIUS_COLORS.len();
            let t = (position / CHARS_PER_COLOR - band as f64).clamp(0.0, 1.0);
            let amount = (t * t * (3.0 - 2.0 * t)).clamp(0.0, 1.0);
            let from = RADIUS_COLORS[band];
            let to = RADIUS_COLORS[(band + 1) % RADIUS_COLORS.len()];
            let mixed = mix_colors(from, to, amount, Some(ColorMixSpace::Srgb)).unwrap_or(from);
            result.push_str(&foreground_ansi(mixed, mode));
            result.push(ch);
        }
        result.push_str("\x1b[39m");
        result
    }

    static OPENROUTER_DARK: LazyLock<(Color, Color)> = LazyLock::new(|| {
        (
            parse_color(ParseColorValue::Text("#c6f243")).expect("openrouter dark base parses"),
            parse_color(ParseColorValue::Text("#efffb0")).expect("openrouter dark peak parses"),
        )
    });
    static OPENROUTER_LIGHT: LazyLock<(Color, Color)> = LazyLock::new(|| {
        (
            parse_color(ParseColorValue::Text("#5d32e1")).expect("openrouter light base parses"),
            parse_color(ParseColorValue::Text("#8f6bff")).expect("openrouter light peak parses"),
        )
    });
    const GLOW_PERIOD_MS: f64 = 2400.0;

    pub(crate) fn open_router_glow(text: &str, elapsed_ms: f64) -> String {
        let t = theme();
        let (base, peak) = match t.appearance() {
            ThemeAppearance::Dark => *OPENROUTER_DARK,
            ThemeAppearance::Light => *OPENROUTER_LIGHT,
        };
        let amount = ((1.0 - ((2.0 * std::f64::consts::PI * elapsed_ms) / GLOW_PERIOD_MS).cos()) / 2.0).clamp(0.0, 1.0);
        let mixed = mix_colors(base, peak, amount, Some(ColorMixSpace::Oklch)).unwrap_or(base);
        format!("{}{text}\x1b[39m", foreground_ansi(mixed, t.get_color_mode()))
    }

    const ANIMATION_FRAME_MS: u64 = 50;

    pub(crate) fn start_brand_animation(
        tui: Arc<dyn TUI + Send + Sync>,
        is_animating: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Interval {
        let interval = set_interval(ANIMATION_FRAME_MS, move || {
            if is_animating() {
                tui.request_render(None);
            }
        });
        interval.unref();
        interval
    }
}

const MAX_VISIBLE: usize = 8;

static ENV_VAR_LIST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][A-Z0-9_]*(?:, [A-Z][A-Z0-9_]*)*$").expect("ENV_VAR_LIST_RE compiles"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthSelectorMode {
    Login,
    Logout,
}

#[derive(Clone)]
pub enum AuthSelectorMethod {
    ApiKey(Arc<dyn ApiKeyAuth>),
    OAuth(Arc<dyn OAuthAuth>),
}

impl AuthSelectorMethod {
    pub fn name(&self) -> &str {
        match self {
            Self::ApiKey(a) => a.name(),
            Self::OAuth(o) => o.name(),
        }
    }
}

#[derive(Clone)]
pub struct AuthSelectorProvider {
    pub id: String,
    pub name: String,
    pub auth_type: AuthType,
    pub method: Option<AuthSelectorMethod>,
    pub status: Option<AuthCheck>,
    pub subscription: Option<bool>,
}

pub fn format_auth_selector_provider_type(auth_type: AuthType, subscription: Option<bool>) -> &'static str {
    if auth_type == AuthType::ApiKey {
        return "API key";
    }
    if subscription == Some(false) {
        "account"
    } else {
        "subscription"
    }
}

/// Themed suffix describing whether and how a login option is configured, for example `" ✓ configured"`.
pub fn format_auth_selector_provider_status(provider: &AuthSelectorProvider) -> String {
    let t = theme();
    let Some(ref status) = provider.status else {
        return t.fg(ThemeColor::Muted, " • not configured");
    };
    if status.auth_type != provider.auth_type {
        let label = format!(
            "{} configured",
            format_auth_selector_provider_type(status.auth_type, provider.subscription)
        );
        return format!(
            "{}{}",
            t.fg(ThemeColor::Muted, " • "),
            t.fg(ThemeColor::Warning, &label)
        );
    }
    let Some(ref src) = status.source else {
        return t.fg(ThemeColor::Success, " ✓ configured");
    };
    if src.is_empty() || src == "OAuth" || src == "stored credential" {
        return t.fg(ThemeColor::Success, " ✓ configured");
    }
    let formatted_source = if ENV_VAR_LIST_RE.is_match(src) {
        format!("env: {src}")
    } else {
        src.clone()
    };
    t.fg(ThemeColor::Success, &format!(" ✓ {formatted_source}"))
}

struct OAuthSelectorState {
    filtered_providers: Vec<AuthSelectorProvider>,
    selected_index: usize,
    focused: bool,
}

struct OAuthSelectorInner {
    container: Container,
    search_input: Input,
    list_container: Container,
    all_providers: Vec<AuthSelectorProvider>,
    mode: OAuthSelectorMode,
    on_select: Arc<dyn Fn(&str, AuthType) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    show_auth_type_labels: bool,
    animation_start: tokio::time::Instant,
    stop_animation: Mutex<Option<Interval>>,
    state: Mutex<OAuthSelectorState>,
}

impl Drop for OAuthSelectorInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.stop_animation.lock()
            && let Some(interval) = guard.take()
        {
            interval.clear();
        }
    }
}

/// Component that renders an auth provider selector. With `tui`, the selected row of a preferred partner provider
/// shows its animated brand style.
#[derive(Clone)]
pub struct OAuthSelectorComponent {
    inner: Arc<OAuthSelectorInner>,
}

impl OAuthSelectorComponent {
    pub fn new(
        mode: OAuthSelectorMode,
        providers: Vec<AuthSelectorProvider>,
        on_select: Arc<dyn Fn(&str, AuthType) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        initial_search_input: Option<&str>,
        tui: Option<Arc<dyn TUI + Send + Sync>>,
    ) -> Self {
        let auth_types: IndexSet<u8> = providers
            .iter()
            .map(|p| match p.auth_type {
                AuthType::ApiKey => 0,
                AuthType::OAuth => 1,
            })
            .collect();
        let show_auth_type_labels = auth_types.len() > 1;

        let t = theme();
        let container = Container::new();
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let title = match mode {
            OAuthSelectorMode::Login => "Select provider to configure:",
            OAuthSelectorMode::Logout => "Select provider to logout:",
        };
        container.add_child(Arc::new(TruncatedText::new(
            &t.fg(ThemeColor::Accent, &t.bold(title)),
            Some(1),
            Some(0),
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let search_input = Input::new(None);
        if let Some(init) = initial_search_input
            && !init.is_empty()
        {
            search_input.set_value(init);
        }
        container.add_child(Arc::new(search_input.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let list_container = Container::new();
        container.add_child(Arc::new(list_container.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::default()));

        let comp = Self {
            inner: Arc::new(OAuthSelectorInner {
                container,
                search_input,
                list_container,
                all_providers: providers.clone(),
                mode,
                on_select,
                on_cancel,
                show_auth_type_labels,
                animation_start: tokio::time::Instant::now(),
                stop_animation: Mutex::new(None),
                state: Mutex::new(OAuthSelectorState {
                    filtered_providers: providers,
                    selected_index: 0,
                    focused: false,
                }),
            }),
        };

        let weak = Arc::downgrade(&comp.inner);
        comp.inner.search_input.set_on_submit(Some(Arc::new(move |_| {
            if let Some(inner) = weak.upgrade() {
                let selected = {
                    let state = inner.state.lock().unwrap();
                    state
                        .filtered_providers
                        .get(state.selected_index)
                        .map(|p| (p.id.clone(), p.auth_type))
                };
                if let Some((id, auth_type)) = selected {
                    (inner.on_select)(&id, auth_type);
                }
            }
        })));

        comp.filter_providers(initial_search_input.unwrap_or(""));

        if let Some(ui) = tui {
            let comp_for_anim = comp.clone();
            let interval = provider_brand::start_brand_animation(
                ui,
                Arc::new(move || comp_for_anim.get_selected_brand_style().is_some()),
            );
            *comp.inner.stop_animation.lock().unwrap() = Some(interval);
        }

        comp
    }

    /// Stops the brand animation.
    pub fn dispose(&self) {
        let interval = self.inner.stop_animation.lock().unwrap().take();
        if let Some(interval) = interval {
            interval.clear();
        }
    }

    fn has_animation(&self) -> bool {
        self.inner.stop_animation.lock().unwrap().is_some()
    }

    fn get_selected_brand_style(&self) -> Option<provider_brand::ProviderBrandStyle> {
        let state = self.inner.state.lock().unwrap();
        state
            .filtered_providers
            .get(state.selected_index)
            .and_then(|p| provider_brand::get_provider_brand_style(&p.id))
    }

    fn filter_providers(&self, query: &str) {
        let filtered = if query.is_empty() {
            self.inner.all_providers.clone()
        } else {
            fuzzy_filter(
                &self.inner.all_providers,
                query,
                Arc::new(|p: &AuthSelectorProvider| {
                    let auth_str = match p.auth_type {
                        AuthType::ApiKey => "api_key",
                        AuthType::OAuth => "oauth",
                    };
                    let method_name = p.method.as_ref().map(|m| m.name()).unwrap_or("");
                    format!("{} {} {} {}", p.name, p.id, auth_str, method_name)
                }),
            )
        };
        {
            let mut state = self.inner.state.lock().unwrap();
            let max_idx = filtered.len().saturating_sub(1);
            state.filtered_providers = filtered;
            state.selected_index = state.selected_index.min(max_idx);
        }
        self.update_list();
    }

    fn update_list(&self) {
        self.inner.list_container.clear();
        let (filtered, selected_index) = {
            let state = self.inner.state.lock().unwrap();
            (state.filtered_providers.clone(), state.selected_index)
        };
        let t = theme();

        let len = filtered.len();
        let max_start = len.saturating_sub(MAX_VISIBLE);
        let start_index = selected_index.saturating_sub(MAX_VISIBLE / 2).min(max_start);
        let end_index = (start_index + MAX_VISIBLE).min(len);

        let has_anim = self.has_animation();
        for (i, provider) in filtered.iter().enumerate().take(end_index).skip(start_index) {
            let is_selected = i == selected_index;
            let status_indicator = format_auth_selector_provider_status(provider);
            // U+2726 is single-width in every terminal, unlike star emoji or ambiguous-width symbols such as U+2605.
            let partner_marker = if provider_brand::is_preferred_provider(&provider.id) {
                t.fg(ThemeColor::Muted, " ✦")
            } else {
                String::new()
            };
            let auth_type_label = if self.inner.show_auth_type_labels {
                t.fg(
                    ThemeColor::Muted,
                    &format!(
                        " [{}]",
                        format_auth_selector_provider_type(provider.auth_type, provider.subscription)
                    ),
                )
            } else {
                String::new()
            };
            let line = if is_selected {
                let prefix = t.fg(ThemeColor::Accent, "→ ");
                let brand_style = if has_anim {
                    provider_brand::get_provider_brand_style(&provider.id)
                } else {
                    None
                };
                let text = match brand_style {
                    Some(style_fn) => {
                        let elapsed_ms = self.inner.animation_start.elapsed().as_secs_f64() * 1000.0;
                        style_fn(&provider.name, elapsed_ms)
                    }
                    None => t.fg(ThemeColor::Accent, &provider.name),
                };
                format!("{prefix}{text}{partner_marker}{auth_type_label}{status_indicator}")
            } else {
                format!(
                    "  {}{}{}{}",
                    t.fg(ThemeColor::Text, &provider.name),
                    partner_marker,
                    auth_type_label,
                    status_indicator
                )
            };
            self.inner
                .list_container
                .add_child(Arc::new(TruncatedText::new(&line, Some(1), Some(0))));
        }

        if start_index > 0 || end_index < len {
            let scroll = t.fg(ThemeColor::Muted, &format!("  ({}/{})", selected_index + 1, len));
            self.inner
                .list_container
                .add_child(Arc::new(TruncatedText::new(&scroll, Some(1), Some(0))));
        }

        if len == 0 {
            let msg = if self.inner.all_providers.is_empty() {
                match self.inner.mode {
                    OAuthSelectorMode::Login => "No providers available",
                    OAuthSelectorMode::Logout => "No providers logged in. Use /login first.",
                }
            } else {
                "No matching providers"
            };
            self.inner.list_container.add_child(Arc::new(TruncatedText::new(
                &t.fg(ThemeColor::Muted, &format!("  {msg}")),
                Some(1),
                Some(0),
            )));
        }
    }

    pub fn focused(&self) -> bool {
        self.inner.state.lock().unwrap().focused
    }

    pub fn set_focused(&self, value: bool) {
        self.inner.state.lock().unwrap().focused = value;
        self.inner.search_input.set_focused(value);
    }

    pub fn handle_input(&self, key_data: &str) {
        let kb = get_keybindings();
        let (len, selected_index) = {
            let state = self.inner.state.lock().unwrap();
            (state.filtered_providers.len(), state.selected_index)
        };
        let page_index = get_select_page_index(&kb, key_data, selected_index, len, MAX_VISIBLE);

        if kb.matches(key_data, "tui.select.up") {
            if len == 0 {
                return;
            }
            {
                let mut state = self.inner.state.lock().unwrap();
                state.selected_index = state.selected_index.saturating_sub(1);
            }
            self.update_list();
        } else if kb.matches(key_data, "tui.select.down") {
            if len == 0 {
                return;
            }
            {
                let mut state = self.inner.state.lock().unwrap();
                state.selected_index = (state.selected_index + 1).min(len - 1);
            }
            self.update_list();
        } else if let Some(next_idx) = page_index {
            if len == 0 {
                return;
            }
            self.inner.state.lock().unwrap().selected_index = next_idx;
            self.update_list();
        } else if kb.matches(key_data, "tui.select.confirm") {
            let selected = {
                let state = self.inner.state.lock().unwrap();
                state
                    .filtered_providers
                    .get(state.selected_index)
                    .map(|p| (p.id.clone(), p.auth_type))
            };
            if let Some((id, auth_type)) = selected {
                (self.inner.on_select)(&id, auth_type);
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.inner.on_cancel)();
        } else {
            self.inner.search_input.handle_input(key_data);
            let query = self.inner.search_input.get_value();
            self.filter_providers(&query);
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        if self.has_animation() && self.get_selected_brand_style().is_some() {
            self.update_list();
        }
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Focusable for OAuthSelectorComponent {
    fn focused(&self) -> bool {
        OAuthSelectorComponent::focused(self)
    }

    fn set_focused(&self, focused: bool) {
        OAuthSelectorComponent::set_focused(self, focused)
    }
}

impl Component for OAuthSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        OAuthSelectorComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        OAuthSelectorComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        OAuthSelectorComponent::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}

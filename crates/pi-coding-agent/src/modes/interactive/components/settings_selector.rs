//! Port of packages/coding-agent/src/modes/interactive/components/settings-selector.ts

use std::sync::{Arc, Mutex};

pub use super::super::session_share::SessionShareDestination;
use crate::core::http_dispatcher::{HTTP_IDLE_TIMEOUT_CHOICES, format_http_idle_timeout_ms};
use crate::core::settings_manager::{CACHE_WARMING_MODES, QuietStartup as CoreQuietStartup, QuietStartupHeader};
pub use crate::core::settings_manager::{
    CacheWarmingMode, DefaultProjectTrust, DoubleEscapeAction, FullscreenExitOutput, QueueModeSetting as SteeringMode,
    TreeFilterMode, TuiMode, WarningSettings,
};
use indexmap::{IndexMap, IndexSet};
use pi_ai::{Model, ModelThinkingLevel, Transport, get_supported_thinking_levels};
use pi_js::intl::locale_compare;
use pi_tui::components::settings_list::SettingsListOptions;
use pi_tui::{
    Component, Container, ScrollViewScrollbar, SelectItem, SelectListLayoutOptions, SettingItem, SettingsList, Spacer,
    Text, TuiMouseEvent, TuiMouseEventResult, WheelScrollLines, get_capabilities,
};
use serde::{Deserialize, Serialize};

use super::super::theme::theme::{
    SYSTEM_THEME_NAME, TerminalTheme, ThemeColor, get_settings_list_theme, parse_auto_theme_setting, theme,
};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::key_display_text;
use super::mermaid::MermaidRenderingMode;
use super::settings_submenu::{SelectSubmenu, StepText, SteppedSubmenu, SteppedSubmenuOptions, SteppedSubmenuStep};

pub(crate) fn cache_warming_mode_as_str(mode: CacheWarmingMode) -> &'static str {
    match mode {
        CacheWarmingMode::Off => "off",
        CacheWarmingMode::Streaming => "streaming",
        CacheWarmingMode::Idle => "idle",
    }
}

fn parse_cache_warming_mode(s: &str) -> Option<CacheWarmingMode> {
    match s {
        "off" => Some(CacheWarmingMode::Off),
        "streaming" => Some(CacheWarmingMode::Streaming),
        "idle" => Some(CacheWarmingMode::Idle),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuietStartup {
    True,
    #[default]
    False,
    Header,
}

impl QuietStartup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::True => "true",
            Self::False => "false",
            Self::Header => "header",
        }
    }
}

impl From<CoreQuietStartup> for QuietStartup {
    fn from(value: CoreQuietStartup) -> Self {
        match value {
            CoreQuietStartup::Bool(true) => Self::True,
            CoreQuietStartup::Bool(false) => Self::False,
            CoreQuietStartup::Header(_) => Self::Header,
        }
    }
}

impl From<QuietStartup> for CoreQuietStartup {
    fn from(value: QuietStartup) -> Self {
        match value {
            QuietStartup::True => Self::Bool(true),
            QuietStartup::False => Self::Bool(false),
            QuietStartup::Header => Self::Header(QuietStartupHeader::Header),
        }
    }
}

pub(crate) fn tui_mode_as_str(mode: TuiMode) -> &'static str {
    match mode {
        TuiMode::Regular => "regular",
        TuiMode::Fullscreen => "fullscreen",
    }
}

fn fullscreen_exit_output_as_str(output: FullscreenExitOutput) -> &'static str {
    match output {
        FullscreenExitOutput::Transcript => "transcript",
        FullscreenExitOutput::ResumeHint => "resume-hint",
    }
}

const MODEL_PICKER_LAYOUT: SelectListLayoutOptions = SelectListLayoutOptions {
    min_primary_column_width: Some(12),
    max_primary_column_width: Some(46),
    truncate_primary: None,
};

fn thinking_level_as_str(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::XHigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

fn parse_thinking_level(s: &str) -> Option<ModelThinkingLevel> {
    match s {
        "off" => Some(ModelThinkingLevel::Off),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::XHigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

fn thinking_description(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "No reasoning",
        ModelThinkingLevel::Minimal => "Very brief reasoning (~1k tokens)",
        ModelThinkingLevel::Low => "Light reasoning (~2k tokens)",
        ModelThinkingLevel::Medium => "Moderate reasoning (~8k tokens)",
        ModelThinkingLevel::High => "Deep reasoning (~16k tokens)",
        ModelThinkingLevel::XHigh => "Extra-high reasoning (~32k tokens)",
        ModelThinkingLevel::Max => "Maximum reasoning",
    }
}

fn default_project_trust_label(trust: DefaultProjectTrust) -> &'static str {
    match trust {
        DefaultProjectTrust::Ask => "Ask",
        DefaultProjectTrust::Always => "Always trust",
        DefaultProjectTrust::Never => "Never trust",
    }
}

fn parse_default_project_trust_label(label: &str) -> Option<DefaultProjectTrust> {
    match label {
        "Ask" => Some(DefaultProjectTrust::Ask),
        "Always trust" => Some(DefaultProjectTrust::Always),
        "Never trust" => Some(DefaultProjectTrust::Never),
        _ => None,
    }
}

fn scrollbar_as_str(s: ScrollViewScrollbar) -> &'static str {
    match s {
        ScrollViewScrollbar::Auto => "auto",
        ScrollViewScrollbar::Always => "always",
        ScrollViewScrollbar::Hidden => "hidden",
    }
}

fn parse_scrollbar(s: &str) -> Option<ScrollViewScrollbar> {
    match s {
        "auto" => Some(ScrollViewScrollbar::Auto),
        "always" => Some(ScrollViewScrollbar::Always),
        "hidden" => Some(ScrollViewScrollbar::Hidden),
        _ => None,
    }
}

fn wheel_scroll_as_str(w: WheelScrollLines) -> String {
    match w {
        WheelScrollLines::Auto(_) => "auto".to_string(),
        WheelScrollLines::Lines(n) => pi_js::num::to_js_string(n),
    }
}

fn steering_mode_as_str(mode: SteeringMode) -> &'static str {
    match mode {
        SteeringMode::All => "all",
        SteeringMode::OneAtATime => "one-at-a-time",
    }
}

fn parse_steering_mode(s: &str) -> Option<SteeringMode> {
    match s {
        "all" => Some(SteeringMode::All),
        "one-at-a-time" => Some(SteeringMode::OneAtATime),
        _ => None,
    }
}

fn double_escape_action_as_str(action: DoubleEscapeAction) -> &'static str {
    match action {
        DoubleEscapeAction::Fork => "fork",
        DoubleEscapeAction::Tree => "tree",
        DoubleEscapeAction::None => "none",
    }
}

fn parse_double_escape_action(s: &str) -> Option<DoubleEscapeAction> {
    match s {
        "fork" => Some(DoubleEscapeAction::Fork),
        "tree" => Some(DoubleEscapeAction::Tree),
        "none" => Some(DoubleEscapeAction::None),
        _ => None,
    }
}

fn tree_filter_mode_as_str(mode: TreeFilterMode) -> &'static str {
    match mode {
        TreeFilterMode::Default => "default",
        TreeFilterMode::NoTools => "no-tools",
        TreeFilterMode::UserOnly => "user-only",
        TreeFilterMode::LabeledOnly => "labeled-only",
        TreeFilterMode::All => "all",
    }
}

fn parse_tree_filter_mode(s: &str) -> Option<TreeFilterMode> {
    match s {
        "default" => Some(TreeFilterMode::Default),
        "no-tools" => Some(TreeFilterMode::NoTools),
        "user-only" => Some(TreeFilterMode::UserOnly),
        "labeled-only" => Some(TreeFilterMode::LabeledOnly),
        "all" => Some(TreeFilterMode::All),
        _ => None,
    }
}

#[derive(Clone)]
pub struct SettingsConfig {
    pub auto_compact: bool,
    pub default_model: String,
    pub current_model: Option<Model>,
    pub available_default_models: Vec<Model>,
    pub show_images: bool,
    pub image_width_cells: i64,
    pub auto_resize_images: bool,
    pub block_images: bool,
    pub enable_skill_commands: bool,
    pub steering_mode: SteeringMode,
    pub follow_up_mode: SteeringMode,
    pub transport: Transport,
    pub session_share_destination: SessionShareDestination,
    pub http_idle_timeout_ms: u64,
    pub cache_warming_mode: CacheWarmingMode,
    pub thinking_level: ModelThinkingLevel,
    pub available_thinking_levels: Vec<ModelThinkingLevel>,
    pub model_thinking_levels: IndexMap<String, ModelThinkingLevel>,
    pub current_theme: String,
    pub terminal_theme: TerminalTheme,
    pub available_themes: Vec<String>,
    pub hide_thinking_block: bool,
    pub mermaid_rendering_mode: MermaidRenderingMode,
    pub show_cache_miss_notices: bool,
    pub collapse_changelog: bool,
    pub enable_install_telemetry: bool,
    pub double_escape_action: DoubleEscapeAction,
    pub tree_filter_mode: TreeFilterMode,
    pub show_hardware_cursor: bool,
    pub editor_padding_x: i64,
    pub output_pad: i64,
    pub autocomplete_max_visible: i64,
    pub quiet_startup: QuietStartup,
    pub default_project_trust: DefaultProjectTrust,
    pub clear_on_shrink: bool,
    pub show_terminal_progress: bool,
    pub tui_mode: TuiMode,
    pub fullscreen_exit_output: FullscreenExitOutput,
    pub fullscreen_scrollbar: ScrollViewScrollbar,
    pub fullscreen_copy_on_select: bool,
    pub fullscreen_wheel_scroll_lines: WheelScrollLines,
    pub warnings: WarningSettings,
}

impl Default for SettingsConfig {
    fn default() -> Self {
        Self {
            auto_compact: true,
            default_model: "not set".to_string(),
            current_model: None,
            available_default_models: Vec::new(),
            show_images: true,
            image_width_cells: 80,
            auto_resize_images: true,
            block_images: false,
            enable_skill_commands: true,
            steering_mode: SteeringMode::OneAtATime,
            follow_up_mode: SteeringMode::OneAtATime,
            transport: Transport::Sse,
            session_share_destination: SessionShareDestination::Auto,
            http_idle_timeout_ms: 300_000,
            cache_warming_mode: CacheWarmingMode::Off,
            thinking_level: ModelThinkingLevel::Medium,
            available_thinking_levels: Vec::new(),
            model_thinking_levels: IndexMap::new(),
            current_theme: "dark".to_string(),
            terminal_theme: TerminalTheme::Dark,
            available_themes: Vec::new(),
            hide_thinking_block: false,
            mermaid_rendering_mode: MermaidRenderingMode::Streaming,
            show_cache_miss_notices: true,
            collapse_changelog: false,
            enable_install_telemetry: false,
            double_escape_action: DoubleEscapeAction::Tree,
            tree_filter_mode: TreeFilterMode::Default,
            show_hardware_cursor: false,
            editor_padding_x: 0,
            output_pad: 1,
            autocomplete_max_visible: 5,
            quiet_startup: QuietStartup::False,
            default_project_trust: DefaultProjectTrust::Ask,
            clear_on_shrink: false,
            show_terminal_progress: true,
            tui_mode: TuiMode::Regular,
            fullscreen_exit_output: FullscreenExitOutput::Transcript,
            fullscreen_scrollbar: ScrollViewScrollbar::Auto,
            fullscreen_copy_on_select: true,
            fullscreen_wheel_scroll_lines: WheelScrollLines::AUTO,
            warnings: WarningSettings::default(),
        }
    }
}

#[derive(Clone, Default)]
pub struct SettingsCallbacks {
    pub on_auto_compact_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_show_images_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_image_width_cells_change: Option<Arc<dyn Fn(i64) + Send + Sync>>,
    pub on_auto_resize_images_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_block_images_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_enable_skill_commands_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_steering_mode_change: Option<Arc<dyn Fn(SteeringMode) + Send + Sync>>,
    pub on_follow_up_mode_change: Option<Arc<dyn Fn(SteeringMode) + Send + Sync>>,
    pub on_transport_change: Option<Arc<dyn Fn(Transport) + Send + Sync>>,
    pub on_session_share_destination_change: Option<Arc<dyn Fn(SessionShareDestination) + Send + Sync>>,
    pub on_http_idle_timeout_ms_change: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    pub on_cache_warming_mode_change: Option<Arc<dyn Fn(CacheWarmingMode) + Send + Sync>>,
    pub on_model_thinking_level_change: Option<Arc<dyn Fn(&str, &str, ModelThinkingLevel) + Send + Sync>>,
    pub on_model_thinking_level_remove: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    pub on_theme_change: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_theme_preview: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_hide_thinking_block_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_mermaid_rendering_mode_change: Option<Arc<dyn Fn(MermaidRenderingMode) + Send + Sync>>,
    pub on_show_cache_miss_notices_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_collapse_changelog_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_enable_install_telemetry_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_double_escape_action_change: Option<Arc<dyn Fn(DoubleEscapeAction) + Send + Sync>>,
    pub on_tree_filter_mode_change: Option<Arc<dyn Fn(TreeFilterMode) + Send + Sync>>,
    pub on_show_hardware_cursor_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_editor_padding_x_change: Option<Arc<dyn Fn(i64) + Send + Sync>>,
    pub on_output_pad_change: Option<Arc<dyn Fn(i64) + Send + Sync>>,
    pub on_autocomplete_max_visible_change: Option<Arc<dyn Fn(i64) + Send + Sync>>,
    pub on_quiet_startup_change: Option<Arc<dyn Fn(QuietStartup) + Send + Sync>>,
    pub on_default_project_trust_change: Option<Arc<dyn Fn(DefaultProjectTrust) + Send + Sync>>,
    pub on_clear_on_shrink_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_show_terminal_progress_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_tui_mode_change: Option<Arc<dyn Fn(TuiMode) + Send + Sync>>,
    pub on_fullscreen_exit_output_change: Option<Arc<dyn Fn(FullscreenExitOutput) + Send + Sync>>,
    pub on_fullscreen_scrollbar_change: Option<Arc<dyn Fn(ScrollViewScrollbar) + Send + Sync>>,
    pub on_fullscreen_copy_on_select_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    pub on_fullscreen_wheel_scroll_lines_change: Option<Arc<dyn Fn(WheelScrollLines) + Send + Sync>>,
    pub on_warnings_change: Option<Arc<dyn Fn(WarningSettings) + Send + Sync>>,
    pub on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct WarningSettingsSubmenuInner {
    container: Container,
    settings_list: SettingsList,
}

#[derive(Clone)]
struct WarningSettingsSubmenu {
    inner: Arc<WarningSettingsSubmenuInner>,
}

impl WarningSettingsSubmenu {
    fn new(
        initial: WarningSettings,
        on_change: Arc<dyn Fn(WarningSettings) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let state = Arc::new(Mutex::new(initial.clone()));
        let t = theme();
        let container = Container::new();
        container.add_child(Arc::new(Text::new(
            Some(&t.bold(&t.fg(ThemeColor::Accent, "Warnings"))),
            Some(0),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Muted, "Enable or disable individual warnings")),
            Some(0),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let items = vec![SettingItem {
            id: "anthropic-extra-usage".to_string(),
            label: "Anthropic extra usage".to_string(),
            description: Some("Warn when Anthropic subscription auth may use paid extra usage".to_string()),
            current_value: if initial.anthropic_extra_usage.unwrap_or(true) {
                "true"
            } else {
                "false"
            }
            .to_string(),
            values: Some(vec!["true".to_string(), "false".to_string()]),
            submenu: None,
        }];

        let state_c = Arc::clone(&state);
        let settings_list = SettingsList::new(
            items.clone(),
            items.len().min(10) as i64,
            get_settings_list_theme(),
            Arc::new(move |id: &str, new_value: &str| {
                if id == "anthropic-extra-usage" {
                    let updated = {
                        let mut guard = state_c.lock().unwrap();
                        guard.anthropic_extra_usage = Some(new_value == "true");
                        guard.clone()
                    };
                    on_change(updated);
                }
            }),
            on_cancel,
            None,
        );
        container.add_child(Arc::new(settings_list.clone()));
        Self {
            inner: Arc::new(WarningSettingsSubmenuInner {
                container,
                settings_list,
            }),
        }
    }
}

impl Component for WarningSettingsSubmenu {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }
    fn handle_input(&self, data: &str) {
        self.inner.settings_list.handle_input(data);
    }
    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }
    fn wants_key_release(&self) -> bool {
        false
    }
    fn invalidate(&self) {
        self.inner.container.invalidate();
    }
    fn has_handle_input(&self) -> bool {
        true
    }
    fn is_container_handle_mouse(&self) -> bool {
        true
    }
    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}

const CLEAR_OVERRIDE_VALUE: &str = "__clear__";
const AUTOMATIC_THEME_VALUE: &str = "/";

fn model_setting_key(model: &Model) -> String {
    format!("{}/{}", model.provider, model.id)
}

fn model_display_label(model: &Model) -> String {
    format!("{} [{}]", model.id, model.provider)
}

fn model_thinking_overrides_summary(overrides: &IndexMap<String, ModelThinkingLevel>) -> String {
    if overrides.is_empty() {
        "none".to_string()
    } else {
        format!("{} configured", overrides.len())
    }
}

fn model_item_label(model: &Model) -> String {
    format!(
        "{} {}",
        model.id,
        theme().fg(ThemeColor::Muted, &format!("[{}]", model.provider))
    )
}

fn theme_items(available_themes: &[String], current_theme: &str) -> Vec<SelectItem> {
    available_themes
        .iter()
        .map(|name| SelectItem {
            value: name.clone(),
            label: format!("{}{name}", if name == current_theme { "✓ " } else { "  " }),
            description: (name == SYSTEM_THEME_NAME).then(|| "Theme created from your terminal's colors".to_string()),
        })
        .collect()
}

fn single_mode_theme_items(available_themes: &[String], current_theme: &str) -> Vec<SelectItem> {
    let mut items = theme_items(available_themes, current_theme);
    let system_item = items
        .iter()
        .position(|i| i.value == SYSTEM_THEME_NAME)
        .map(|idx| items.remove(idx));
    let mut out = Vec::new();
    if let Some(sys) = system_item {
        out.push(sys);
    }
    out.push(SelectItem {
        value: AUTOMATIC_THEME_VALUE.to_string(),
        label: "  automatic".to_string(),
        description: Some("Use separate themes for light and dark terminal appearance".to_string()),
    });
    out.extend(items);
    out
}

fn preferred_theme(available_themes: &[String], preferred: Option<&str>, fallback: &str) -> String {
    if let Some(p) = preferred
        && available_themes.iter().any(|t| t == p)
    {
        return p.to_string();
    }
    if available_themes.iter().any(|t| t == fallback) {
        return fallback.to_string();
    }
    available_themes
        .first()
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeSubmenuMode {
    Single,
    Automatic,
}

struct ThemeSubmenuState {
    mode: ThemeSubmenuMode,
    light_theme: String,
    dark_theme: String,
    single_theme: String,
    input_component: Option<Arc<dyn Component>>,
}

struct ThemeSubmenuInner {
    container: Container,
    terminal_theme: TerminalTheme,
    available_themes: Vec<String>,
    original_theme_setting: String,
    on_preview: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    on_done: Arc<dyn Fn(Option<String>) + Send + Sync>,
    state: Mutex<ThemeSubmenuState>,
}

#[derive(Clone)]
struct ThemeSubmenu {
    inner: Arc<ThemeSubmenuInner>,
}

impl ThemeSubmenu {
    fn new(
        current_theme_setting: &str,
        terminal_theme: TerminalTheme,
        available_themes: Vec<String>,
        on_preview: Option<Arc<dyn Fn(&str) + Send + Sync>>,
        on_done: Arc<dyn Fn(Option<String>) + Send + Sync>,
    ) -> Self {
        let auto_theme = parse_auto_theme_setting(Some(current_theme_setting));
        let default_light = preferred_theme(&available_themes, Some("light"), SYSTEM_THEME_NAME);
        let default_dark = preferred_theme(&available_themes, Some("dark"), SYSTEM_THEME_NAME);
        let (light_theme, dark_theme) = match auto_theme {
            Some((ref light, ref dark)) => (
                preferred_theme(&available_themes, Some(light), &default_light),
                preferred_theme(&available_themes, Some(dark), &default_dark),
            ),
            None => (default_light, default_dark),
        };
        let fixed_theme = (!current_theme_setting.contains('/')).then_some(current_theme_setting);
        let active_auto = if terminal_theme == TerminalTheme::Light {
            &light_theme
        } else {
            &dark_theme
        };
        let single_theme = preferred_theme(
            &available_themes,
            fixed_theme.or(auto_theme.as_ref().map(|_| active_auto.as_str())),
            SYSTEM_THEME_NAME,
        );
        let mode = if auto_theme.is_some() {
            ThemeSubmenuMode::Automatic
        } else {
            ThemeSubmenuMode::Single
        };

        let comp = Self {
            inner: Arc::new(ThemeSubmenuInner {
                container: Container::new(),
                terminal_theme,
                available_themes,
                original_theme_setting: current_theme_setting.to_string(),
                on_preview,
                on_done,
                state: Mutex::new(ThemeSubmenuState {
                    mode,
                    light_theme,
                    dark_theme,
                    single_theme,
                    input_component: None,
                }),
            }),
        };

        if mode == ThemeSubmenuMode::Automatic {
            comp.show_automatic_menu();
        } else {
            comp.show_single_menu();
        }
        comp
    }

    fn set_content(&self, render_comp: Arc<dyn Component>, input_comp: Arc<dyn Component>) {
        self.inner.container.clear();
        self.inner.container.add_child(render_comp);
        self.inner.state.lock().unwrap().input_component = Some(input_comp);
    }

    fn get_automatic_theme_setting(&self) -> String {
        let s = self.inner.state.lock().unwrap();
        format!("{}/{}", s.light_theme, s.dark_theme)
    }

    fn get_theme_setting(&self) -> String {
        let s = self.inner.state.lock().unwrap();
        match s.mode {
            ThemeSubmenuMode::Automatic => format!("{}/{}", s.light_theme, s.dark_theme),
            ThemeSubmenuMode::Single => s.single_theme.clone(),
        }
    }

    fn get_active_automatic_theme(&self) -> String {
        let s = self.inner.state.lock().unwrap();
        if self.inner.terminal_theme == TerminalTheme::Light {
            s.light_theme.clone()
        } else {
            s.dark_theme.clone()
        }
    }

    fn preview(&self, value: &str) {
        if let Some(ref cb) = self.inner.on_preview {
            cb(value);
        }
    }

    fn apply(&self, theme_setting: &str) {
        (self.inner.on_done)(Some(theme_setting.to_string()));
    }

    fn cancel(&self) {
        self.preview(&self.inner.original_theme_setting);
        (self.inner.on_done)(None);
    }

    fn show_single_menu(&self) {
        let single_theme = {
            let mut s = self.inner.state.lock().unwrap();
            s.mode = ThemeSubmenuMode::Single;
            s.single_theme.clone()
        };
        let items = single_mode_theme_items(&self.inner.available_themes, &single_theme);
        let self_sel = self.clone();
        let self_can = self.clone();
        let self_chg = self.clone();
        let menu = Arc::new(SelectSubmenu::new(
            "Theme",
            "Select a theme, or choose automatic to follow terminal appearance.",
            items,
            &single_theme,
            Arc::new(move |value: &str| {
                if value == AUTOMATIC_THEME_VALUE {
                    self_sel.inner.state.lock().unwrap().mode = ThemeSubmenuMode::Automatic;
                    let setting = self_sel.get_theme_setting();
                    self_sel.preview(&setting);
                    self_sel.show_automatic_menu();
                    return;
                }
                self_sel.inner.state.lock().unwrap().single_theme = value.to_string();
                self_sel.apply(value);
            }),
            Arc::new(move || self_can.cancel()),
            Some(Arc::new(move |value: &str| {
                if value == AUTOMATIC_THEME_VALUE {
                    self_chg.preview(&self_chg.get_automatic_theme_setting());
                } else {
                    self_chg.preview(value);
                }
            })),
            None,
        ));
        self.set_content(menu.clone(), menu);
    }

    fn create_theme_select(
        &self,
        title: &str,
        description: &str,
        current_value: &str,
        done: Arc<dyn Fn(Option<String>) + Send + Sync>,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Arc<dyn Component> {
        let items = theme_items(&self.inner.available_themes, current_value);
        let self_can = self.clone();
        let self_chg = self.clone();
        Arc::new(SelectSubmenu::new(
            title,
            description,
            items,
            current_value,
            on_select,
            Arc::new(move || {
                let setting = self_can.get_theme_setting();
                self_can.preview(&setting);
                done(None);
            }),
            Some(Arc::new(move |value: &str| {
                self_chg.preview(value);
            })),
            None,
        ))
    }

    fn show_automatic_menu(&self) {
        let (light_theme, dark_theme) = {
            let mut s = self.inner.state.lock().unwrap();
            s.mode = ThemeSubmenuMode::Automatic;
            (s.light_theme.clone(), s.dark_theme.clone())
        };

        let t = theme();
        let content = Arc::new(Container::new());
        content.add_child(Arc::new(Text::new(
            Some(&t.bold(&t.fg(ThemeColor::Accent, "Automatic Theme"))),
            Some(0),
            Some(0),
            None,
        )));
        content.add_child(Arc::new(Spacer::new(Some(1))));
        content.add_child(Arc::new(Text::new(
            Some(&t.fg(
                ThemeColor::Muted,
                "Choose themes for terminal light and dark appearance.",
            )),
            Some(0),
            Some(0),
            None,
        )));
        content.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Muted, "Light/dark detection requires terminal support.")),
            Some(0),
            Some(0),
            None,
        )));
        content.add_child(Arc::new(Spacer::new(Some(1))));

        let self_light = self.clone();
        let self_dark = self.clone();
        let items = vec![
            SettingItem {
                id: "light-theme".to_string(),
                label: "Light theme".to_string(),
                description: Some("Theme to use in automatic mode when the terminal is light".to_string()),
                current_value: light_theme,
                values: None,
                submenu: Some(Arc::new(move |current_val, done| {
                    let s_inner = self_light.clone();
                    let done_c = Arc::clone(&done);
                    self_light.create_theme_select(
                        "Light Theme",
                        "Select the theme to use for light terminal appearance",
                        current_val,
                        Arc::new(move |v| done(v, None)),
                        Arc::new(move |value: &str| {
                            s_inner.inner.state.lock().unwrap().light_theme = value.to_string();
                            let setting = s_inner.get_theme_setting();
                            s_inner.preview(&setting);
                            done_c(Some(value.to_string()), None);
                        }),
                    )
                })),
            },
            SettingItem {
                id: "dark-theme".to_string(),
                label: "Dark theme".to_string(),
                description: Some("Theme to use in automatic mode when the terminal is dark".to_string()),
                current_value: dark_theme,
                values: None,
                submenu: Some(Arc::new(move |current_val, done| {
                    let s_inner = self_dark.clone();
                    let done_c = Arc::clone(&done);
                    self_dark.create_theme_select(
                        "Dark Theme",
                        "Select the theme to use for dark terminal appearance",
                        current_val,
                        Arc::new(move |v| done(v, None)),
                        Arc::new(move |value: &str| {
                            s_inner.inner.state.lock().unwrap().dark_theme = value.to_string();
                            let setting = s_inner.get_theme_setting();
                            s_inner.preview(&setting);
                            done_c(Some(value.to_string()), None);
                        }),
                    )
                })),
            },
            SettingItem {
                id: "apply".to_string(),
                label: "Apply".to_string(),
                description: Some("Save and go back".to_string()),
                current_value: "save and go back".to_string(),
                values: Some(vec!["save and go back".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "single-mode".to_string(),
                label: "Change mode".to_string(),
                description: Some("Switch to one theme for light and dark".to_string()),
                current_value: "switch to single theme".to_string(),
                values: Some(vec!["switch to single theme".to_string()]),
                submenu: None,
            },
        ];

        let self_chg = self.clone();
        let self_can = self.clone();
        let settings_list = Arc::new(SettingsList::new(
            items.clone(),
            items.len().min(10) as i64,
            get_settings_list_theme(),
            Arc::new(move |id: &str, _| match id {
                "single-mode" => {
                    let single = self_chg.get_active_automatic_theme();
                    {
                        let mut s = self_chg.inner.state.lock().unwrap();
                        s.mode = ThemeSubmenuMode::Single;
                        s.single_theme = single.clone();
                    }
                    self_chg.preview(&single);
                    self_chg.show_single_menu();
                }
                "apply" => {
                    let setting = self_chg.get_automatic_theme_setting();
                    self_chg.apply(&setting);
                }
                _ => {}
            }),
            Arc::new(move || self_can.cancel()),
            None,
        ));
        content.add_child(settings_list.clone());
        self.set_content(content, settings_list);
    }
}

impl Component for ThemeSubmenu {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }
    fn handle_input(&self, data: &str) {
        let comp = self.inner.state.lock().unwrap().input_component.clone();
        if let Some(c) = comp {
            c.handle_input(data);
        }
    }
    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }
    fn wants_key_release(&self) -> bool {
        false
    }
    fn invalidate(&self) {
        self.inner.container.invalidate();
    }
    fn has_handle_input(&self) -> bool {
        true
    }
    fn is_container_handle_mouse(&self) -> bool {
        true
    }
    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}

/// Main settings selector component.
#[derive(Clone)]
pub struct SettingsSelectorComponent {
    container: Container,
    settings_list: SettingsList,
}

impl SettingsSelectorComponent {
    pub fn new(config: SettingsConfig, callbacks: SettingsCallbacks) -> Self {
        let supports_images = get_capabilities().images.is_some();
        let follow_up_key = key_display_text(&"app.message.followUp".to_string());
        let cycle_thinking_key = key_display_text(&"app.thinking.cycle".to_string());

        let current_warnings = Arc::new(Mutex::new(config.warnings));
        let current_model_thinking_levels = Arc::new(Mutex::new(config.model_thinking_levels.clone()));

        let mut default_model_by_value: IndexMap<String, Model> = IndexMap::new();
        for model in &config.available_default_models {
            default_model_by_value.insert(model_setting_key(model), model.clone());
        }
        let default_model_by_value = Arc::new(default_model_by_value);

        let current_default_model_key = default_model_by_value
            .contains_key(&config.default_model)
            .then(|| config.default_model.clone());
        let current_model_key = config.current_model.as_ref().map(model_setting_key);

        let mut wheel_set: IndexSet<i64> = [1, 2, 3, 5, 10].into_iter().collect();
        if let WheelScrollLines::Lines(n) = config.fullscreen_wheel_scroll_lines
            && n.is_finite()
        {
            wheel_set.insert(n as i64);
        }
        let mut wheel_nums: Vec<i64> = wheel_set.into_iter().collect();
        wheel_nums.sort_unstable();
        let mut wheel_values = vec!["auto".to_string()];
        wheel_values.extend(wheel_nums.into_iter().map(|n| n.to_string()));

        let mut items: Vec<SettingItem> = vec![
            SettingItem {
                id: "autocompact".to_string(),
                label: "Auto-compact".to_string(),
                description: Some("Automatically compact context when it gets too large".to_string()),
                current_value: config.auto_compact.to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "steering-mode".to_string(),
                label: "Steering mode".to_string(),
                description: Some(
                    "Enter while streaming queues steering messages. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once."
                        .to_string(),
                ),
                current_value: steering_mode_as_str(config.steering_mode).to_string(),
                values: Some(vec!["one-at-a-time".to_string(), "all".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "follow-up-mode".to_string(),
                label: "Follow-up mode".to_string(),
                description: Some(format!(
                    "{follow_up_key} queues follow-up messages until agent stops. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once."
                )),
                current_value: steering_mode_as_str(config.follow_up_mode).to_string(),
                values: Some(vec!["one-at-a-time".to_string(), "all".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "transport".to_string(),
                label: "Transport".to_string(),
                description: Some("Preferred transport for providers that support multiple transports".to_string()),
                current_value: match config.transport {
                    Transport::Sse => "sse",
                    Transport::Websocket => "websocket",
                    Transport::WebsocketCached => "websocket-cached",
                    Transport::Auto => "auto",
                }
                .to_string(),
                values: Some(vec![
                    "sse".to_string(),
                    "websocket".to_string(),
                    "websocket-cached".to_string(),
                    "auto".to_string(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "share-destination".to_string(),
                label: "Session share destination".to_string(),
                description: Some(
                    "Where to upload sessions with /share (auto prefers Radius when authenticated, otherwise GitHub Gist)"
                        .to_string(),
                ),
                current_value: match config.session_share_destination {
                    SessionShareDestination::Auto => "auto",
                    SessionShareDestination::Gist => "gist",
                    SessionShareDestination::Radius => "radius",
                }
                .to_string(),
                values: Some(vec!["auto".to_string(), "gist".to_string(), "radius".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "http-idle-timeout".to_string(),
                label: "HTTP idle timeout".to_string(),
                description: Some(
                    "Maximum idle gap while waiting for HTTP headers or body chunks. Disable for local models that pause longer than five minutes."
                        .to_string(),
                ),
                current_value: format_http_idle_timeout_ms(config.http_idle_timeout_ms),
                values: Some(HTTP_IDLE_TIMEOUT_CHOICES.iter().map(|c| c.label.to_string()).collect()),
                submenu: None,
            },
            SettingItem {
                id: "cache-warming-mode".to_string(),
                label: "Cache warming".to_string(),
                description: Some(
                    "off; streaming while the agent runs; idle also between runs while continuation stays profitable"
                        .to_string(),
                ),
                current_value: cache_warming_mode_as_str(config.cache_warming_mode).to_string(),
                values: Some(
                    CACHE_WARMING_MODES
                        .iter()
                        .map(|m| cache_warming_mode_as_str(*m).to_string())
                        .collect(),
                ),
                submenu: None,
            },
            SettingItem {
                id: "hide-thinking".to_string(),
                label: "Hide thinking".to_string(),
                description: Some("Hide thinking blocks in assistant responses".to_string()),
                current_value: config.hide_thinking_block.to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "mermaid-rendering".to_string(),
                label: "Mermaid diagrams".to_string(),
                description: Some("Render Mermaid code blocks as Unicode diagrams".to_string()),
                current_value: match config.mermaid_rendering_mode {
                    MermaidRenderingMode::Off => "off",
                    MermaidRenderingMode::Final => "final",
                    MermaidRenderingMode::Streaming => "streaming",
                }
                .to_string(),
                values: Some(vec!["off".to_string(), "final".to_string(), "streaming".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "cache-miss-notices".to_string(),
                label: "Cache miss notices".to_string(),
                description: Some(
                    "Show transcript notices for cache costs and provider recovery diagnostics".to_string(),
                ),
                current_value: config.show_cache_miss_notices.to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "collapse-changelog".to_string(),
                label: "Collapse changelog".to_string(),
                description: Some("Show condensed changelog after updates".to_string()),
                current_value: config.collapse_changelog.to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "quiet-startup".to_string(),
                label: "Quiet startup".to_string(),
                description: Some(
                    "Disable verbose printing at startup (header: keep only the startup header)".to_string(),
                ),
                current_value: config.quiet_startup.as_str().to_string(),
                values: Some(vec!["true".to_string(), "header".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "install-telemetry".to_string(),
                label: "Install telemetry".to_string(),
                description: Some(
                    "Send an anonymous version/update ping after changelog-detected updates".to_string(),
                ),
                current_value: config.enable_install_telemetry.to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "default-project-trust".to_string(),
                label: "Default project trust".to_string(),
                description: Some(
                    "Fallback behavior when no extension or saved trust decision decides project trust".to_string(),
                ),
                current_value: default_project_trust_label(config.default_project_trust).to_string(),
                values: Some(vec![
                    "Ask".to_string(),
                    "Always trust".to_string(),
                    "Never trust".to_string(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "double-escape-action".to_string(),
                label: "Double-escape action".to_string(),
                description: Some("Action when pressing Escape twice with empty editor".to_string()),
                current_value: double_escape_action_as_str(config.double_escape_action).to_string(),
                values: Some(vec!["tree".to_string(), "fork".to_string(), "none".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "tree-filter-mode".to_string(),
                label: "Tree filter mode".to_string(),
                description: Some("Default filter when opening /tree".to_string()),
                current_value: tree_filter_mode_as_str(config.tree_filter_mode).to_string(),
                values: Some(vec![
                    "default".to_string(),
                    "no-tools".to_string(),
                    "user-only".to_string(),
                    "labeled-only".to_string(),
                    "all".to_string(),
                ]),
                submenu: None,
            },
            {
                let cw_c = Arc::clone(&current_warnings);
                let cb_w = callbacks.on_warnings_change.clone();
                SettingItem {
                    id: "warnings".to_string(),
                    label: "Warnings".to_string(),
                    description: Some("Enable or disable individual warnings".to_string()),
                    current_value: "configure".to_string(),
                    values: None,
                    submenu: Some(Arc::new(move |_, done| {
                        let initial = cw_c.lock().unwrap().clone();
                        let cw_inner = Arc::clone(&cw_c);
                        let cb_inner = cb_w.clone();
                        Arc::new(WarningSettingsSubmenu::new(
                            initial,
                            Arc::new(move |w| {
                                *cw_inner.lock().unwrap() = w.clone();
                                if let Some(ref cb) = cb_inner {
                                    cb(w);
                                }
                            }),
                            Arc::new(move || done(None, None)),
                        ))
                    })),
                }
            },
            {
                let models = config.available_default_models.clone();
                let by_val = Arc::clone(&default_model_by_value);
                let levels_map = Arc::clone(&current_model_thinking_levels);
                let cur_model_key = current_model_key.clone();
                let global_thinking = config.thinking_level;
                let cb_set = callbacks.on_model_thinking_level_change.clone();
                let cb_rem = callbacks.on_model_thinking_level_remove.clone();
                let summary_initial = model_thinking_overrides_summary(&config.model_thinking_levels);
                SettingItem {
                    id: "model-thinking".to_string(),
                    label: "Default thinking level per model".to_string(),
                    description: Some(format!(
                        "Override the default thinking level for specific models. {cycle_thinking_key} cycles in-session."
                    )),
                    current_value: summary_initial,
                    values: None,
                    submenu: Some(Arc::new(move |_, done| {
                        let models_step1 = models.clone();
                        let levels_step1 = Arc::clone(&levels_map);
                        let cur_m1 = cur_model_key.clone();
                        let cur_d1 = current_default_model_key.clone();
                        let cur_m1_pre = cur_model_key.clone();
                        let cur_d1_pre = current_default_model_key.clone();
                        let by_val_title = Arc::clone(&by_val);
                        let by_val_opts = Arc::clone(&by_val);
                        let levels_step2 = Arc::clone(&levels_map);
                        let by_val_done = Arc::clone(&by_val);
                        let levels_done = Arc::clone(&levels_map);
                        let levels_cancel = Arc::clone(&levels_map);
                        let cb_set_c = cb_set.clone();
                        let cb_rem_c = cb_rem.clone();

                        let steps = vec![
                            SteppedSubmenuStep {
                                key: "model".to_string(),
                                title: StepText::Static("Per-Model Thinking Level".to_string()),
                                description: StepText::Static("Select a model to configure".to_string()),
                                options: Arc::new(move |_| {
                                    let mut sorted = models_step1.clone();
                                    sorted.sort_by(|a, b| {
                                        let a_key = model_setting_key(a);
                                        let b_key = model_setting_key(b);
                                        let ord_priority = |k: &str| -> u8 {
                                            if Some(k) == cur_m1.as_deref() {
                                                0
                                            } else if Some(k) == cur_d1.as_deref() {
                                                1
                                            } else {
                                                2
                                            }
                                        };
                                        ord_priority(&a_key)
                                            .cmp(&ord_priority(&b_key))
                                            .then_with(|| locale_compare(&a.provider, &b.provider))
                                    });
                                    let overrides = levels_step1.lock().unwrap();
                                    let mut out: Vec<SelectItem> = sorted
                                        .iter()
                                        .map(|m| {
                                            let k = model_setting_key(m);
                                            let ov = overrides.get(&k).copied();
                                            SelectItem {
                                                value: k,
                                                label: model_item_label(m),
                                                description: ov.map(|l| thinking_level_as_str(l).to_string()),
                                            }
                                        })
                                        .collect();
                                    if out.is_empty() {
                                        out.push(SelectItem {
                                            value: "__none__".to_string(),
                                            label: "No models available".to_string(),
                                            description: Some(
                                                "Log in to a provider or configure an API key first".to_string(),
                                            ),
                                        });
                                    }
                                    out
                                }),
                                preselect: Some(Arc::new(move |_| cur_m1_pre.clone().or_else(|| cur_d1_pre.clone()))),
                                searchable: true,
                                layout: Some(MODEL_PICKER_LAYOUT),
                            },
                            SteppedSubmenuStep {
                                key: "level".to_string(),
                                title: StepText::Dynamic(Arc::new(move |ctx| {
                                    let key = ctx.get("model").map(String::as_str).unwrap_or("");
                                    let label = by_val_title
                                        .get(key)
                                        .map(model_display_label)
                                        .unwrap_or_else(|| key.to_string());
                                    format!("Thinking Level for {label}")
                                })),
                                description: StepText::Static(
                                    "Select default thinking level for this model".to_string(),
                                ),
                                options: Arc::new(move |ctx| {
                                    let key = ctx.get("model").map(String::as_str).unwrap_or("");
                                    let Some(model) = by_val_opts.get(key) else {
                                        return Vec::new();
                                    };
                                    let levels = if model.reasoning {
                                        get_supported_thinking_levels(model)
                                    } else {
                                        vec![ModelThinkingLevel::Off]
                                    };
                                    let overrides = levels_step2.lock().unwrap();
                                    let active = overrides.get(key).copied();
                                    let mut out: Vec<SelectItem> = levels
                                        .into_iter()
                                        .map(|lvl| {
                                            let s = thinking_level_as_str(lvl);
                                            SelectItem {
                                                value: s.to_string(),
                                                label: format!("{}{s}", if active == Some(lvl) { "✓ " } else { "  " }),
                                                description: Some(thinking_description(lvl).to_string()),
                                            }
                                        })
                                        .collect();
                                    if active.is_some() {
                                        out.push(SelectItem {
                                            value: CLEAR_OVERRIDE_VALUE.to_string(),
                                            label: "  (clear override)".to_string(),
                                            description: Some(format!(
                                                "Revert to global default ({})",
                                                thinking_level_as_str(global_thinking)
                                            )),
                                        });
                                    }
                                    out
                                }),
                                preselect: Some(Arc::new({
                                    let levels_p = Arc::clone(&levels_map);
                                    move |ctx| {
                                        let key = ctx.get("model").map(String::as_str).unwrap_or("");
                                        levels_p
                                            .lock()
                                            .unwrap()
                                            .get(key)
                                            .copied()
                                            .map(|l| thinking_level_as_str(l).to_string())
                                    }
                                })),
                                searchable: false,
                                layout: None,
                            },
                        ];

                        Arc::new(SteppedSubmenu::new(
                            steps,
                            Arc::new(move |selections| {
                                let Some(model_key) = selections.get("model") else {
                                    return;
                                };
                                let Some(model) = by_val_done.get(model_key) else {
                                    return;
                                };
                                let level_str = selections.get("level").map(String::as_str).unwrap_or("");
                                if level_str == CLEAR_OVERRIDE_VALUE {
                                    if let Some(ref cb) = cb_rem_c {
                                        cb(&model.provider, &model.id);
                                    }
                                    levels_done.lock().unwrap().shift_remove(model_key);
                                } else if let Some(lvl) = parse_thinking_level(level_str) {
                                    if let Some(ref cb) = cb_set_c {
                                        cb(&model.provider, &model.id, lvl);
                                    }
                                    levels_done.lock().unwrap().insert(model_key.clone(), lvl);
                                }
                            }),
                            Arc::new(move || {
                                let summary = model_thinking_overrides_summary(&levels_cancel.lock().unwrap());
                                done(Some(summary), None);
                            }),
                            Some(SteppedSubmenuOptions {
                                start_at_step: None,
                                initial_context: None,
                                loop_on_complete: true,
                            }),
                        ))
                    })),
                }
            },
            SettingItem {
                id: "tui-mode".to_string(),
                label: "TUI mode".to_string(),
                description: Some("Interface layout; regular mode uses the terminal's normal scrollback".to_string()),
                current_value: tui_mode_as_str(config.tui_mode).to_string(),
                values: Some(vec!["regular".to_string(), "fullscreen".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "fullscreen-exit-output".to_string(),
                label: "Fullscreen exit output".to_string(),
                description: Some(
                    "Print the transcript or only a session resume hint when exiting fullscreen mode".to_string(),
                ),
                current_value: fullscreen_exit_output_as_str(config.fullscreen_exit_output).to_string(),
                values: Some(vec!["transcript".to_string(), "resume-hint".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "fullscreen-scrollbar".to_string(),
                label: "Fullscreen scrollbar".to_string(),
                description: Some(
                    "Scrollbar behavior in fullscreen mode; has no effect in regular mode".to_string(),
                ),
                current_value: scrollbar_as_str(config.fullscreen_scrollbar).to_string(),
                values: Some(vec![
                    "auto".to_string(),
                    "always".to_string(),
                    "hidden".to_string(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "fullscreen-copy-on-select".to_string(),
                label: "Fullscreen copy on select".to_string(),
                description: Some(
                    "Automatically copy selected text in fullscreen mode; disable to copy selections with Ctrl+X"
                        .to_string(),
                ),
                current_value: config.fullscreen_copy_on_select.to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "fullscreen-wheel-scroll-lines".to_string(),
                label: "Fullscreen wheel scrolling".to_string(),
                description: Some(
                    "Lines per mouse-wheel event in fullscreen mode; 'auto' speeds up fast wheel spins where the terminal does not"
                        .to_string(),
                ),
                current_value: wheel_scroll_as_str(config.fullscreen_wheel_scroll_lines),
                values: Some(wheel_values),
                submenu: None,
            },
            {
                let term_theme = config.terminal_theme;
                let avail_themes = config.available_themes.clone();
                let preview_cb = callbacks.on_theme_preview.clone();
                SettingItem {
                    id: "theme".to_string(),
                    label: "Theme".to_string(),
                    description: Some("Color theme for the interface".to_string()),
                    current_value: config.current_theme.clone(),
                    values: None,
                    submenu: Some(Arc::new(move |current_val, done| {
                        let done_c = Arc::clone(&done);
                        Arc::new(ThemeSubmenu::new(
                            current_val,
                            term_theme,
                            avail_themes.clone(),
                            preview_cb.clone(),
                            Arc::new(move |val| done_c(val, None)),
                        ))
                    })),
                }
            },
        ];

        if supports_images {
            items.insert(
                1,
                SettingItem {
                    id: "show-images".to_string(),
                    label: "Show images".to_string(),
                    description: Some("Render images inline in terminal".to_string()),
                    current_value: config.show_images.to_string(),
                    values: Some(vec!["true".to_string(), "false".to_string()]),
                    submenu: None,
                },
            );
            items.insert(
                2,
                SettingItem {
                    id: "image-width-cells".to_string(),
                    label: "Image width".to_string(),
                    description: Some("Preferred inline image width in terminal cells".to_string()),
                    current_value: config.image_width_cells.to_string(),
                    values: Some(vec!["60".to_string(), "80".to_string(), "120".to_string()]),
                    submenu: None,
                },
            );
        }

        let insert_at = if supports_images { 3 } else { 1 };
        items.insert(
            insert_at,
            SettingItem {
                id: "auto-resize-images".to_string(),
                label: "Auto-resize images".to_string(),
                description: Some("Resize large images to 2000x2000 max for better model compatibility".to_string()),
                current_value: config.auto_resize_images.to_string(),
                values: Some(vec!["true".to_string(), "false".to_string()]),
                submenu: None,
            },
        );

        let mut next_idx = insert_at + 1;
        let extra_toggles = [
            (
                "block-images",
                "Block images",
                "Prevent images from being sent to LLM providers",
                config.block_images.to_string(),
                vec!["true".to_string(), "false".to_string()],
            ),
            (
                "skill-commands",
                "Skill commands",
                "Register skills as /skill:name commands",
                config.enable_skill_commands.to_string(),
                vec!["true".to_string(), "false".to_string()],
            ),
            (
                "show-hardware-cursor",
                "Show hardware cursor",
                "Use the terminal cursor instead of Pi's drawn cursor",
                config.show_hardware_cursor.to_string(),
                vec!["true".to_string(), "false".to_string()],
            ),
            (
                "editor-padding",
                "Editor padding",
                "Horizontal padding for input editor (0-3)",
                config.editor_padding_x.to_string(),
                vec!["0".to_string(), "1".to_string(), "2".to_string(), "3".to_string()],
            ),
            (
                "output-padding",
                "Output padding",
                "Horizontal padding for messages, tool output, and command output",
                config.output_pad.to_string(),
                vec!["0".to_string(), "1".to_string()],
            ),
            (
                "autocomplete-max-visible",
                "Autocomplete max items",
                "Max visible items in autocomplete dropdown (3-20)",
                config.autocomplete_max_visible.to_string(),
                vec![
                    "3".to_string(),
                    "5".to_string(),
                    "7".to_string(),
                    "10".to_string(),
                    "15".to_string(),
                    "20".to_string(),
                ],
            ),
            (
                "clear-on-shrink",
                "Clear on shrink",
                "Clear empty rows when content shrinks (may cause flicker)",
                config.clear_on_shrink.to_string(),
                vec!["true".to_string(), "false".to_string()],
            ),
            (
                "terminal-progress",
                "Terminal progress",
                "Show OSC 9;4 progress indicators in the terminal tab bar",
                config.show_terminal_progress.to_string(),
                vec!["true".to_string(), "false".to_string()],
            ),
        ];

        for (id, label, desc, val, vals) in extra_toggles {
            items.insert(
                next_idx,
                SettingItem {
                    id: id.to_string(),
                    label: label.to_string(),
                    description: Some(desc.to_string()),
                    current_value: val,
                    values: Some(vals),
                    submenu: None,
                },
            );
            next_idx += 1;
        }

        let container = Container::new();
        container.add_child(Arc::new(DynamicBorder::default()));

        let cb = callbacks.clone();
        let on_change = Arc::new(move |id: &str, new_value: &str| match id {
            "autocompact" => {
                if let Some(ref f) = cb.on_auto_compact_change {
                    f(new_value == "true");
                }
            }
            "show-images" => {
                if let Some(ref f) = cb.on_show_images_change {
                    f(new_value == "true");
                }
            }
            "image-width-cells" => {
                if let Ok(v) = new_value.parse::<i64>()
                    && let Some(ref f) = cb.on_image_width_cells_change
                {
                    f(v);
                }
            }
            "auto-resize-images" => {
                if let Some(ref f) = cb.on_auto_resize_images_change {
                    f(new_value == "true");
                }
            }
            "block-images" => {
                if let Some(ref f) = cb.on_block_images_change {
                    f(new_value == "true");
                }
            }
            "skill-commands" => {
                if let Some(ref f) = cb.on_enable_skill_commands_change {
                    f(new_value == "true");
                }
            }
            "steering-mode" => {
                if let Some(mode) = parse_steering_mode(new_value)
                    && let Some(ref f) = cb.on_steering_mode_change
                {
                    f(mode);
                }
            }
            "follow-up-mode" => {
                if let Some(mode) = parse_steering_mode(new_value)
                    && let Some(ref f) = cb.on_follow_up_mode_change
                {
                    f(mode);
                }
            }
            "transport" => {
                let transport = match new_value {
                    "sse" => Some(Transport::Sse),
                    "websocket" => Some(Transport::Websocket),
                    "websocket-cached" => Some(Transport::WebsocketCached),
                    "auto" => Some(Transport::Auto),
                    _ => None,
                };
                if let Some(tr) = transport
                    && let Some(ref f) = cb.on_transport_change
                {
                    f(tr);
                }
            }
            "share-destination" => {
                let dest = match new_value {
                    "auto" => Some(SessionShareDestination::Auto),
                    "gist" => Some(SessionShareDestination::Gist),
                    "radius" => Some(SessionShareDestination::Radius),
                    _ => None,
                };
                if let Some(dest) = dest
                    && let Some(ref f) = cb.on_session_share_destination_change
                {
                    f(dest);
                }
            }
            "http-idle-timeout" => {
                if let Some(choice) = HTTP_IDLE_TIMEOUT_CHOICES.iter().find(|c| c.label == new_value)
                    && let Some(ref f) = cb.on_http_idle_timeout_ms_change
                {
                    f(choice.timeout_ms);
                }
            }
            "cache-warming-mode" => {
                if let Some(mode) = parse_cache_warming_mode(new_value)
                    && let Some(ref f) = cb.on_cache_warming_mode_change
                {
                    f(mode);
                }
            }
            "hide-thinking" => {
                if let Some(ref f) = cb.on_hide_thinking_block_change {
                    f(new_value == "true");
                }
            }
            "mermaid-rendering" => {
                let mode = match new_value {
                    "off" => Some(MermaidRenderingMode::Off),
                    "final" => Some(MermaidRenderingMode::Final),
                    "streaming" => Some(MermaidRenderingMode::Streaming),
                    _ => None,
                };
                if let Some(m) = mode
                    && let Some(ref f) = cb.on_mermaid_rendering_mode_change
                {
                    f(m);
                }
            }
            "cache-miss-notices" => {
                if let Some(ref f) = cb.on_show_cache_miss_notices_change {
                    f(new_value == "true");
                }
            }
            "collapse-changelog" => {
                if let Some(ref f) = cb.on_collapse_changelog_change {
                    f(new_value == "true");
                }
            }
            "quiet-startup" => {
                let val = match new_value {
                    "header" => QuietStartup::Header,
                    "true" => QuietStartup::True,
                    _ => QuietStartup::False,
                };
                if let Some(ref f) = cb.on_quiet_startup_change {
                    f(val);
                }
            }
            "install-telemetry" => {
                if let Some(ref f) = cb.on_enable_install_telemetry_change {
                    f(new_value == "true");
                }
            }
            "default-project-trust" => {
                if let Some(trust) = parse_default_project_trust_label(new_value)
                    && let Some(ref f) = cb.on_default_project_trust_change
                {
                    f(trust);
                }
            }
            "double-escape-action" => {
                if let Some(act) = parse_double_escape_action(new_value)
                    && let Some(ref f) = cb.on_double_escape_action_change
                {
                    f(act);
                }
            }
            "tree-filter-mode" => {
                if let Some(mode) = parse_tree_filter_mode(new_value)
                    && let Some(ref f) = cb.on_tree_filter_mode_change
                {
                    f(mode);
                }
            }
            "show-hardware-cursor" => {
                if let Some(ref f) = cb.on_show_hardware_cursor_change {
                    f(new_value == "true");
                }
            }
            "editor-padding" => {
                if let Ok(v) = new_value.parse::<i64>()
                    && let Some(ref f) = cb.on_editor_padding_x_change
                {
                    f(v);
                }
            }
            "output-padding" => {
                if let Some(ref f) = cb.on_output_pad_change {
                    f(if new_value == "0" { 0 } else { 1 });
                }
            }
            "autocomplete-max-visible" => {
                if let Ok(v) = new_value.parse::<i64>()
                    && let Some(ref f) = cb.on_autocomplete_max_visible_change
                {
                    f(v);
                }
            }
            "clear-on-shrink" => {
                if let Some(ref f) = cb.on_clear_on_shrink_change {
                    f(new_value == "true");
                }
            }
            "terminal-progress" => {
                if let Some(ref f) = cb.on_show_terminal_progress_change {
                    f(new_value == "true");
                }
            }
            "tui-mode" => {
                let mode = if new_value == "fullscreen" {
                    TuiMode::Fullscreen
                } else {
                    TuiMode::Regular
                };
                if let Some(ref f) = cb.on_tui_mode_change {
                    f(mode);
                }
            }
            "fullscreen-exit-output" => {
                let out = if new_value == "resume-hint" {
                    FullscreenExitOutput::ResumeHint
                } else {
                    FullscreenExitOutput::Transcript
                };
                if let Some(ref f) = cb.on_fullscreen_exit_output_change {
                    f(out);
                }
            }
            "fullscreen-scrollbar" => {
                if let Some(sb) = parse_scrollbar(new_value)
                    && let Some(ref f) = cb.on_fullscreen_scrollbar_change
                {
                    f(sb);
                }
            }
            "fullscreen-copy-on-select" => {
                if let Some(ref f) = cb.on_fullscreen_copy_on_select_change {
                    f(new_value == "true");
                }
            }
            "fullscreen-wheel-scroll-lines" => {
                let val = if new_value == "auto" {
                    Some(WheelScrollLines::AUTO)
                } else {
                    new_value.parse::<f64>().ok().map(WheelScrollLines::Lines)
                };
                if let Some(w) = val
                    && let Some(ref f) = cb.on_fullscreen_wheel_scroll_lines_change
                {
                    f(w);
                }
            }
            "theme" => {
                if let Some(ref f) = cb.on_theme_change {
                    f(new_value);
                }
            }
            _ => {}
        });

        let cancel_cb = callbacks.on_cancel.unwrap_or_else(|| Arc::new(|| {}));
        let settings_list = SettingsList::new(
            items,
            10,
            get_settings_list_theme(),
            on_change,
            cancel_cb,
            Some(SettingsListOptions {
                enable_search: Some(true),
            }),
        );
        container.add_child(Arc::new(settings_list.clone()));
        container.add_child(Arc::new(DynamicBorder::default()));

        Self {
            container,
            settings_list,
        }
    }

    pub fn get_settings_list(&self) -> &SettingsList {
        &self.settings_list
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    pub fn handle_input(&self, data: &str) {
        self.settings_list.handle_input(data);
    }

    pub fn invalidate(&self) {
        self.container.invalidate();
    }
}

impl Component for SettingsSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        SettingsSelectorComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        SettingsSelectorComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        SettingsSelectorComponent::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.container.children())
    }
}

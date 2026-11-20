//! Port of packages/coding-agent/src/modes/interactive/theme/theme.ts

use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use crate::core::source_info::SourceInfo;
use crate::utils::fs_watch::{FsWatcher, close_watcher, watch_with_error_handler};
use crate::utils::syntax_highlight::{
    HighlightFormatter, HighlightOptions, HighlightTheme, get_highlight_language_count, highlight, supports_language,
};
use crate::utils::text::strip_bom;
use crate::vendor::chalk::Chalk;
use indexmap::{IndexMap, IndexSet};
use pi_agent_core::ThinkingLevel;
use pi_tui::colors::ParseColorValue;
use pi_tui::{
    Color, EditorTheme, MarkdownTheme, RgbColor, SelectListTheme, SettingsListTheme, TerminalColorMode, TerminalColors,
    TextAttributes, background_ansi, color_to_hex, color_to_oklch, foreground_ansi, get_terminal_color_mode,
    indexed_color, mix_colors, parse_color, rgb_color, style_text_with_ansi,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use super::system_theme::SYSTEM_THEME_NAME;
use super::system_theme::{SystemThemeInput, generate_system_theme_colors, terminal_appearance};
use super::theme_schema::ThemeColorValue;
pub use super::theme_schema::ValidatedThemeJson as ThemeJson;

pub type ThemeJsonValidator = Arc<dyn Fn(&str, &Value) -> pi_js::Result<ThemeJson> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeAppearance {
    Dark,
    Light,
}

pub type TerminalTheme = ThemeAppearance;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThemeColor {
    #[serde(rename = "accent")]
    Accent,
    #[serde(rename = "border")]
    Border,
    #[serde(rename = "borderAccent")]
    BorderAccent,
    #[serde(rename = "borderMuted")]
    BorderMuted,
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "warning")]
    Warning,
    #[serde(rename = "muted")]
    Muted,
    #[serde(rename = "dim")]
    Dim,
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "thinkingText")]
    ThinkingText,
    #[serde(rename = "scrollbarTrack")]
    ScrollbarTrack,
    #[serde(rename = "scrollbarThumb")]
    ScrollbarThumb,
    #[serde(rename = "searchMatchText")]
    SearchMatchText,
    #[serde(rename = "userMessageText")]
    UserMessageText,
    #[serde(rename = "customMessageText")]
    CustomMessageText,
    #[serde(rename = "customMessageLabel")]
    CustomMessageLabel,
    #[serde(rename = "toolTitle")]
    ToolTitle,
    #[serde(rename = "toolOutput")]
    ToolOutput,
    #[serde(rename = "mdHeading")]
    MdHeading,
    #[serde(rename = "mdLink")]
    MdLink,
    #[serde(rename = "mdLinkUrl")]
    MdLinkUrl,
    #[serde(rename = "mdCode")]
    MdCode,
    #[serde(rename = "mdCodeBlock")]
    MdCodeBlock,
    #[serde(rename = "mdCodeBlockBorder")]
    MdCodeBlockBorder,
    #[serde(rename = "mdQuote")]
    MdQuote,
    #[serde(rename = "mdQuoteBorder")]
    MdQuoteBorder,
    #[serde(rename = "mdHr")]
    MdHr,
    #[serde(rename = "mdListBullet")]
    MdListBullet,
    #[serde(rename = "toolDiffAdded")]
    ToolDiffAdded,
    #[serde(rename = "toolDiffRemoved")]
    ToolDiffRemoved,
    #[serde(rename = "toolDiffContext")]
    ToolDiffContext,
    #[serde(rename = "syntaxComment")]
    SyntaxComment,
    #[serde(rename = "syntaxKeyword")]
    SyntaxKeyword,
    #[serde(rename = "syntaxFunction")]
    SyntaxFunction,
    #[serde(rename = "syntaxVariable")]
    SyntaxVariable,
    #[serde(rename = "syntaxString")]
    SyntaxString,
    #[serde(rename = "syntaxNumber")]
    SyntaxNumber,
    #[serde(rename = "syntaxType")]
    SyntaxType,
    #[serde(rename = "syntaxOperator")]
    SyntaxOperator,
    #[serde(rename = "syntaxPunctuation")]
    SyntaxPunctuation,
    #[serde(rename = "thinkingOff")]
    ThinkingOff,
    #[serde(rename = "thinkingMinimal")]
    ThinkingMinimal,
    #[serde(rename = "thinkingLow")]
    ThinkingLow,
    #[serde(rename = "thinkingMedium")]
    ThinkingMedium,
    #[serde(rename = "thinkingHigh")]
    ThinkingHigh,
    #[serde(rename = "thinkingXhigh")]
    ThinkingXhigh,
    #[serde(rename = "thinkingMax")]
    ThinkingMax,
    #[serde(rename = "bashMode")]
    BashMode,
}

impl ThemeColor {
    pub fn as_str(self) -> &'static str {
        use ThemeColor::*;
        match self {
            Accent => "accent",
            Border => "border",
            BorderAccent => "borderAccent",
            BorderMuted => "borderMuted",
            Success => "success",
            Error => "error",
            Warning => "warning",
            Muted => "muted",
            Dim => "dim",
            Text => "text",
            ThinkingText => "thinkingText",
            ScrollbarTrack => "scrollbarTrack",
            ScrollbarThumb => "scrollbarThumb",
            SearchMatchText => "searchMatchText",
            UserMessageText => "userMessageText",
            CustomMessageText => "customMessageText",
            CustomMessageLabel => "customMessageLabel",
            ToolTitle => "toolTitle",
            ToolOutput => "toolOutput",
            MdHeading => "mdHeading",
            MdLink => "mdLink",
            MdLinkUrl => "mdLinkUrl",
            MdCode => "mdCode",
            MdCodeBlock => "mdCodeBlock",
            MdCodeBlockBorder => "mdCodeBlockBorder",
            MdQuote => "mdQuote",
            MdQuoteBorder => "mdQuoteBorder",
            MdHr => "mdHr",
            MdListBullet => "mdListBullet",
            ToolDiffAdded => "toolDiffAdded",
            ToolDiffRemoved => "toolDiffRemoved",
            ToolDiffContext => "toolDiffContext",
            SyntaxComment => "syntaxComment",
            SyntaxKeyword => "syntaxKeyword",
            SyntaxFunction => "syntaxFunction",
            SyntaxVariable => "syntaxVariable",
            SyntaxString => "syntaxString",
            SyntaxNumber => "syntaxNumber",
            SyntaxType => "syntaxType",
            SyntaxOperator => "syntaxOperator",
            SyntaxPunctuation => "syntaxPunctuation",
            ThinkingOff => "thinkingOff",
            ThinkingMinimal => "thinkingMinimal",
            ThinkingLow => "thinkingLow",
            ThinkingMedium => "thinkingMedium",
            ThinkingHigh => "thinkingHigh",
            ThinkingXhigh => "thinkingXhigh",
            ThinkingMax => "thinkingMax",
            BashMode => "bashMode",
        }
    }

    pub fn from_token_str(s: &str) -> pi_js::Result<Self> {
        use ThemeColor::*;
        match s {
            "accent" => Ok(Accent),
            "border" => Ok(Border),
            "borderAccent" => Ok(BorderAccent),
            "borderMuted" => Ok(BorderMuted),
            "success" => Ok(Success),
            "error" => Ok(Error),
            "warning" => Ok(Warning),
            "muted" => Ok(Muted),
            "dim" => Ok(Dim),
            "text" => Ok(Text),
            "thinkingText" => Ok(ThinkingText),
            "scrollbarTrack" => Ok(ScrollbarTrack),
            "scrollbarThumb" => Ok(ScrollbarThumb),
            "searchMatchText" => Ok(SearchMatchText),
            "userMessageText" => Ok(UserMessageText),
            "customMessageText" => Ok(CustomMessageText),
            "customMessageLabel" => Ok(CustomMessageLabel),
            "toolTitle" => Ok(ToolTitle),
            "toolOutput" => Ok(ToolOutput),
            "mdHeading" => Ok(MdHeading),
            "mdLink" => Ok(MdLink),
            "mdLinkUrl" => Ok(MdLinkUrl),
            "mdCode" => Ok(MdCode),
            "mdCodeBlock" => Ok(MdCodeBlock),
            "mdCodeBlockBorder" => Ok(MdCodeBlockBorder),
            "mdQuote" => Ok(MdQuote),
            "mdQuoteBorder" => Ok(MdQuoteBorder),
            "mdHr" => Ok(MdHr),
            "mdListBullet" => Ok(MdListBullet),
            "toolDiffAdded" => Ok(ToolDiffAdded),
            "toolDiffRemoved" => Ok(ToolDiffRemoved),
            "toolDiffContext" => Ok(ToolDiffContext),
            "syntaxComment" => Ok(SyntaxComment),
            "syntaxKeyword" => Ok(SyntaxKeyword),
            "syntaxFunction" => Ok(SyntaxFunction),
            "syntaxVariable" => Ok(SyntaxVariable),
            "syntaxString" => Ok(SyntaxString),
            "syntaxNumber" => Ok(SyntaxNumber),
            "syntaxType" => Ok(SyntaxType),
            "syntaxOperator" => Ok(SyntaxOperator),
            "syntaxPunctuation" => Ok(SyntaxPunctuation),
            "thinkingOff" => Ok(ThinkingOff),
            "thinkingMinimal" => Ok(ThinkingMinimal),
            "thinkingLow" => Ok(ThinkingLow),
            "thinkingMedium" => Ok(ThinkingMedium),
            "thinkingHigh" => Ok(ThinkingHigh),
            "thinkingXhigh" => Ok(ThinkingXhigh),
            "thinkingMax" => Ok(ThinkingMax),
            "bashMode" => Ok(BashMode),
            _ => Err(pi_js::Error::msg(format!("Unknown theme color: {s}"))),
        }
    }

    pub fn all() -> &'static [Self] {
        use ThemeColor::*;
        &[
            Accent,
            Border,
            BorderAccent,
            BorderMuted,
            Success,
            Error,
            Warning,
            Muted,
            Dim,
            Text,
            ThinkingText,
            ScrollbarTrack,
            ScrollbarThumb,
            SearchMatchText,
            UserMessageText,
            CustomMessageText,
            CustomMessageLabel,
            ToolTitle,
            ToolOutput,
            MdHeading,
            MdLink,
            MdLinkUrl,
            MdCode,
            MdCodeBlock,
            MdCodeBlockBorder,
            MdQuote,
            MdQuoteBorder,
            MdHr,
            MdListBullet,
            ToolDiffAdded,
            ToolDiffRemoved,
            ToolDiffContext,
            SyntaxComment,
            SyntaxKeyword,
            SyntaxFunction,
            SyntaxVariable,
            SyntaxString,
            SyntaxNumber,
            SyntaxType,
            SyntaxOperator,
            SyntaxPunctuation,
            ThinkingOff,
            ThinkingMinimal,
            ThinkingLow,
            ThinkingMedium,
            ThinkingHigh,
            ThinkingXhigh,
            ThinkingMax,
            BashMode,
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThemeBg {
    #[serde(rename = "selectedBg")]
    SelectedBg,
    #[serde(rename = "searchMatchBg")]
    SearchMatchBg,
    #[serde(rename = "userMessageBg")]
    UserMessageBg,
    #[serde(rename = "customMessageBg")]
    CustomMessageBg,
    #[serde(rename = "toolPendingBg")]
    ToolPendingBg,
    #[serde(rename = "toolSuccessBg")]
    ToolSuccessBg,
    #[serde(rename = "toolErrorBg")]
    ToolErrorBg,
}

impl ThemeBg {
    pub fn as_str(self) -> &'static str {
        use ThemeBg::*;
        match self {
            SelectedBg => "selectedBg",
            SearchMatchBg => "searchMatchBg",
            UserMessageBg => "userMessageBg",
            CustomMessageBg => "customMessageBg",
            ToolPendingBg => "toolPendingBg",
            ToolSuccessBg => "toolSuccessBg",
            ToolErrorBg => "toolErrorBg",
        }
    }

    pub fn from_token_str(s: &str) -> pi_js::Result<Self> {
        use ThemeBg::*;
        match s {
            "selectedBg" => Ok(SelectedBg),
            "searchMatchBg" => Ok(SearchMatchBg),
            "userMessageBg" => Ok(UserMessageBg),
            "customMessageBg" => Ok(CustomMessageBg),
            "toolPendingBg" => Ok(ToolPendingBg),
            "toolSuccessBg" => Ok(ToolSuccessBg),
            "toolErrorBg" => Ok(ToolErrorBg),
            _ => Err(pi_js::Error::msg(format!("Unknown theme background: {s}"))),
        }
    }

    pub fn all() -> &'static [Self] {
        use ThemeBg::*;
        &[
            SelectedBg,
            SearchMatchBg,
            UserMessageBg,
            CustomMessageBg,
            ToolPendingBg,
            ToolSuccessBg,
            ToolErrorBg,
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeToken {
    Fg(ThemeColor),
    Bg(ThemeBg),
}

impl ThemeToken {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fg(c) => c.as_str(),
            Self::Bg(b) => b.as_str(),
        }
    }

    pub fn from_token_str(s: &str) -> Option<Self> {
        ThemeColor::from_token_str(s)
            .map(Self::Fg)
            .ok()
            .or_else(|| ThemeBg::from_token_str(s).map(Self::Bg).ok())
    }

    pub fn all() -> &'static [Self] {
        static ALL: LazyLock<Vec<ThemeToken>> = LazyLock::new(|| {
            let mut out = Vec::with_capacity(ThemeColor::all().len() + ThemeBg::all().len());
            for &c in ThemeColor::all() {
                out.push(ThemeToken::Fg(c));
            }
            for &b in ThemeBg::all() {
                out.push(ThemeToken::Bg(b));
            }
            out
        });
        &ALL
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThemeStyleFg {
    Token(ThemeColor),
    Color(Color),
}

impl From<ThemeColor> for ThemeStyleFg {
    fn from(value: ThemeColor) -> Self {
        Self::Token(value)
    }
}

impl From<Color> for ThemeStyleFg {
    fn from(value: Color) -> Self {
        Self::Color(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThemeStyleBg {
    Token(ThemeBg),
    Color(Color),
}

impl From<ThemeBg> for ThemeStyleBg {
    fn from(value: ThemeBg) -> Self {
        Self::Token(value)
    }
}

impl From<Color> for ThemeStyleBg {
    fn from(value: Color) -> Self {
        Self::Color(value)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ThemeStyle {
    pub fg: Option<ThemeStyleFg>,
    pub bg: Option<ThemeStyleBg>,
    pub bold: Option<bool>,
    pub dim: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub inverse: Option<bool>,
    pub strikethrough: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeInfo {
    pub name: String,
    pub path: Option<String>,
}

// ============================================================================
// Color & Variable Resolution
// ============================================================================

static OK_SPACE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^ok(?:lch|hsl)\(").expect("OK_SPACE_RE compiles"));
static OKHSL_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^okhsl\(").expect("OKHSL_PREFIX_RE compiles"));

fn resolve_var_refs(
    value: &ThemeColorValue,
    vars: &IndexMap<String, ThemeColorValue>,
    visited: &mut IndexSet<String>,
) -> pi_js::Result<ThemeColorValue> {
    let s = match value {
        ThemeColorValue::Indexed(n) => return Ok(ThemeColorValue::Indexed(*n)),
        ThemeColorValue::String(s) => s,
    };
    if s.is_empty() || s.starts_with('#') || OK_SPACE_RE.is_match(s) {
        return Ok(ThemeColorValue::String(s.clone()));
    }
    if visited.contains(s) {
        return Err(pi_js::Error::msg(format!("Circular variable reference detected: {s}")));
    }
    let Some(target) = vars.get(s) else {
        return Err(pi_js::Error::msg(format!("Variable reference not found: {s}")));
    };
    visited.insert(s.clone());
    resolve_var_refs(target, vars, visited)
}

fn resolve_theme_colors(
    colors: &IndexMap<String, ThemeColorValue>,
    vars: Option<&IndexMap<String, ThemeColorValue>>,
) -> pi_js::Result<IndexMap<String, ThemeColorValue>> {
    let empty_vars = IndexMap::new();
    let vars = vars.unwrap_or(&empty_vars);
    let mut resolved = IndexMap::with_capacity(colors.len());
    for (k, v) in colors {
        let mut visited = IndexSet::new();
        resolved.insert(k.clone(), resolve_var_refs(v, vars, &mut visited)?);
    }
    Ok(resolved)
}

fn with_theme_color_fallbacks(colors: &IndexMap<String, ThemeColorValue>) -> IndexMap<String, ThemeColorValue> {
    let mut out = colors.clone();
    if !out.contains_key("scrollbarTrack")
        && let Some(v) = out.get("muted").cloned()
    {
        out.insert("scrollbarTrack".to_string(), v);
    }
    if !out.contains_key("scrollbarThumb")
        && let Some(v) = out.get("text").cloned()
    {
        out.insert("scrollbarThumb".to_string(), v);
    }
    if !out.contains_key("thinkingMax")
        && let Some(v) = out.get("thinkingXhigh").cloned()
    {
        out.insert("thinkingMax".to_string(), v);
    }
    if !out.contains_key("searchMatchBg")
        && let Some(v) = out.get("selectedBg").cloned()
    {
        out.insert("searchMatchBg".to_string(), v);
    }
    if !out.contains_key("searchMatchText")
        && let Some(v) = out.get("text").cloned()
    {
        out.insert("searchMatchText".to_string(), v);
    }
    out
}

fn split_theme_colors(
    colors: &IndexMap<String, ThemeColorValue>,
) -> (
    IndexMap<ThemeColor, ThemeColorValue>,
    IndexMap<ThemeBg, ThemeColorValue>,
) {
    let mut fg = IndexMap::new();
    let mut bg = IndexMap::new();
    for (k, v) in colors {
        if let Ok(bg_tok) = ThemeBg::from_token_str(k) {
            bg.insert(bg_tok, v.clone());
        } else if let Ok(fg_tok) = ThemeColor::from_token_str(k) {
            fg.insert(fg_tok, v.clone());
        }
    }
    (fg, bg)
}

fn parse_theme_color_value(val: &ThemeColorValue) -> pi_js::Result<Color> {
    match val {
        ThemeColorValue::Indexed(n) => parse_color(ParseColorValue::Number(*n as f64)),
        ThemeColorValue::String(s) => parse_color(ParseColorValue::Text(s)),
    }
}

// ============================================================================
// Appearance & Terminal Default Colors
// ============================================================================

#[derive(Default)]
struct ThemeGlobalState {
    terminal_colors: Arc<TerminalColors>,
    terminal_colors_pending: bool,
    terminal_color_scheme: Option<TerminalTheme>,
    active_theme: Option<Theme>,
    current_theme_name: Option<String>,
    registered_themes: IndexMap<String, Theme>,
    on_theme_change: Option<Arc<dyn Fn() + Send + Sync>>,
    theme_reload_timer: Option<pi_js::time::Timeout>,
    theme_json_validator: Option<ThemeJsonValidator>,
    watcher: Option<Arc<FsWatcher>>,
}

static GLOBAL_STATE: LazyLock<Mutex<ThemeGlobalState>> = LazyLock::new(|| Mutex::new(ThemeGlobalState::default()));

pub fn set_theme_json_validator(validator: Option<ThemeJsonValidator>) {
    GLOBAL_STATE.lock().unwrap().theme_json_validator = validator;
}

pub fn set_terminal_colors(colors: TerminalColors) {
    let mut state = GLOBAL_STATE.lock().unwrap();
    state.terminal_colors = Arc::new(colors);
    state.terminal_colors_pending = false;
}

pub fn set_terminal_color_scheme(scheme: Option<TerminalTheme>) {
    GLOBAL_STATE.lock().unwrap().terminal_color_scheme = scheme;
}

pub fn mark_terminal_colors_pending() {
    GLOBAL_STATE.lock().unwrap().terminal_colors_pending = true;
}

fn guessed_default_color(appearance: ThemeAppearance) -> (Color, Color) {
    let fallback_fg = Color::Rgb(pi_tui::RgbColorValue {
        r: 229.0,
        g: 229.0,
        b: 231.0,
    });
    let fallback_bg = Color::Rgb(pi_tui::RgbColorValue { r: 0.0, g: 0.0, b: 0.0 });
    let white = Color::Rgb(pi_tui::RgbColorValue {
        r: 255.0,
        g: 255.0,
        b: 255.0,
    });
    match appearance {
        ThemeAppearance::Dark => (
            parse_color(ParseColorValue::Text("#e5e5e7")).unwrap_or(fallback_fg),
            parse_color(ParseColorValue::Text("#000000")).unwrap_or(fallback_bg),
        ),
        ThemeAppearance::Light => (
            parse_color(ParseColorValue::Text("#000000")).unwrap_or(fallback_bg),
            parse_color(ParseColorValue::Text("#ffffff")).unwrap_or(white),
        ),
    }
}

fn average_lightness(colors: &[Color]) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for &c in colors {
        if let Color::Indexed(idx) = c
            && idx.index < 16
        {
            continue;
        }
        sum += color_to_oklch(c).l;
        count += 1;
    }
    if count == 0 { None } else { Some(sum / count as f64) }
}

fn detect_appearance(foregrounds: &[Color], backgrounds: &[Color]) -> Option<ThemeAppearance> {
    let fg = average_lightness(foregrounds);
    let bg = average_lightness(backgrounds);
    match (fg, bg) {
        (Some(f), Some(b)) => Some(if b < f {
            ThemeAppearance::Dark
        } else {
            ThemeAppearance::Light
        }),
        (None, Some(b)) => Some(if b < 0.5 {
            ThemeAppearance::Dark
        } else {
            ThemeAppearance::Light
        }),
        (Some(f), None) => Some(if f > 0.5 {
            ThemeAppearance::Dark
        } else {
            ThemeAppearance::Light
        }),
        (None, None) => None,
    }
}

// ============================================================================
// Theme Class (Handle Pattern)
// ============================================================================

struct ThemeMutable {
    resolved_colors: Option<(Arc<TerminalColors>, IndexMap<ThemeToken, Color>)>,
}

struct ThemeInner {
    name: Option<String>,
    source_path: Option<String>,
    source_info: Option<SourceInfo>,
    mode: TerminalColorMode,
    fg_ansi: IndexMap<ThemeColor, String>,
    bg_ansi: IndexMap<ThemeBg, String>,
    concrete_colors: IndexMap<ThemeToken, Color>,
    default_foreground_tokens: Vec<ThemeToken>,
    default_background_tokens: Vec<ThemeToken>,
    dim_tokens: IndexSet<ThemeColor>,
    own_appearance: Option<ThemeAppearance>,
    mutable: Mutex<ThemeMutable>,
}

#[derive(Clone)]
pub struct Theme {
    inner: Arc<ThemeInner>,
}

#[derive(Clone, Debug, Default)]
pub struct ThemeOptions {
    pub name: Option<String>,
    pub source_path: Option<String>,
    pub source_info: Option<SourceInfo>,
    pub appearance: Option<ThemeAppearance>,
    pub dim: Vec<ThemeColor>,
}

impl Theme {
    pub fn from_token_maps(
        fg_colors: &IndexMap<ThemeColor, ThemeColorValue>,
        bg_colors: &IndexMap<ThemeBg, ThemeColorValue>,
        mode: TerminalColorMode,
        options: ThemeOptions,
    ) -> pi_js::Result<Self> {
        let mut foregrounds = fg_colors.clone();
        if !foregrounds.contains_key(&ThemeColor::ScrollbarTrack)
            && let Some(v) = foregrounds.get(&ThemeColor::Muted).cloned()
        {
            foregrounds.insert(ThemeColor::ScrollbarTrack, v);
        }
        if !foregrounds.contains_key(&ThemeColor::ScrollbarThumb)
            && let Some(v) = foregrounds.get(&ThemeColor::Text).cloned()
        {
            foregrounds.insert(ThemeColor::ScrollbarThumb, v);
        }
        if !foregrounds.contains_key(&ThemeColor::ThinkingMax)
            && let Some(v) = foregrounds.get(&ThemeColor::ThinkingXhigh).cloned()
        {
            foregrounds.insert(ThemeColor::ThinkingMax, v);
        }
        if !foregrounds.contains_key(&ThemeColor::SearchMatchText)
            && let Some(v) = foregrounds.get(&ThemeColor::Text).cloned()
        {
            foregrounds.insert(ThemeColor::SearchMatchText, v);
        }

        let mut backgrounds = bg_colors.clone();
        if !backgrounds.contains_key(&ThemeBg::SearchMatchBg)
            && let Some(v) = backgrounds.get(&ThemeBg::SelectedBg).cloned()
        {
            backgrounds.insert(ThemeBg::SearchMatchBg, v);
        }

        let mut fg_ansi = IndexMap::new();
        let mut bg_ansi = IndexMap::new();
        let mut concrete_colors = IndexMap::new();
        let mut default_foreground_tokens = Vec::new();
        let mut default_background_tokens = Vec::new();
        let mut concrete_foregrounds = Vec::new();
        let mut concrete_backgrounds = Vec::new();

        for (tok, val) in &foregrounds {
            if matches!(val, ThemeColorValue::String(s) if s.is_empty()) {
                default_foreground_tokens.push(ThemeToken::Fg(*tok));
                fg_ansi.insert(*tok, "\x1b[39m".to_string());
            } else {
                let color = parse_theme_color_value(val)?;
                concrete_colors.insert(ThemeToken::Fg(*tok), color);
                concrete_foregrounds.push(color);
                fg_ansi.insert(*tok, foreground_ansi(color, mode));
            }
        }

        for (tok, val) in &backgrounds {
            if matches!(val, ThemeColorValue::String(s) if s.is_empty()) {
                default_background_tokens.push(ThemeToken::Bg(*tok));
                bg_ansi.insert(*tok, "\x1b[49m".to_string());
            } else {
                let color = parse_theme_color_value(val)?;
                concrete_colors.insert(ThemeToken::Bg(*tok), color);
                concrete_backgrounds.push(color);
                bg_ansi.insert(*tok, background_ansi(color, mode));
            }
        }

        let own_appearance = options
            .appearance
            .or_else(|| detect_appearance(&concrete_foregrounds, &concrete_backgrounds));
        let dim_tokens: IndexSet<ThemeColor> = options.dim.into_iter().collect();

        Ok(Self {
            inner: Arc::new(ThemeInner {
                name: options.name,
                source_path: options.source_path,
                source_info: options.source_info,
                mode,
                fg_ansi,
                bg_ansi,
                concrete_colors,
                default_foreground_tokens,
                default_background_tokens,
                dim_tokens,
                own_appearance,
                mutable: Mutex::new(ThemeMutable { resolved_colors: None }),
            }),
        })
    }

    pub fn name(&self) -> Option<&str> {
        self.inner.name.as_deref()
    }

    pub fn source_path(&self) -> Option<&str> {
        self.inner.source_path.as_deref()
    }

    pub fn source_info(&self) -> Option<&SourceInfo> {
        self.inner.source_info.as_ref()
    }

    pub fn appearance(&self) -> ThemeAppearance {
        self.inner.own_appearance.unwrap_or_else(get_terminal_theme)
    }

    pub fn colors(&self) -> IndexMap<ThemeToken, Color> {
        let terminal = Arc::clone(&GLOBAL_STATE.lock().unwrap().terminal_colors);
        {
            let guard = self.inner.mutable.lock().unwrap();
            if let Some((ref cached_term, ref cached_colors)) = guard.resolved_colors
                && Arc::ptr_eq(cached_term, &terminal)
            {
                return cached_colors.clone();
            }
        }
        let (guess_fg, guess_bg) = guessed_default_color(self.appearance());
        let to_color = |rgb_opt: Option<RgbColor>, fallback: Color| -> Color {
            rgb_opt
                .and_then(|rgb| rgb_color(rgb.r, rgb.g, rgb.b).ok().map(Color::Rgb))
                .unwrap_or(fallback)
        };
        let foreground = to_color(terminal.foreground, guess_fg);
        let background = to_color(terminal.background, guess_bg);
        let mut colors = self.inner.concrete_colors.clone();
        for &tok in &self.inner.default_foreground_tokens {
            colors.insert(tok, foreground);
        }
        for &tok in &self.inner.default_background_tokens {
            colors.insert(tok, background);
        }
        for &dim_tok in &self.inner.dim_tokens {
            let tok = ThemeToken::Fg(dim_tok);
            if let Some(&c) = colors.get(&tok)
                && let Ok(mixed) = mix_colors(c, background, 0.4, None)
            {
                colors.insert(tok, mixed);
            }
        }
        let mut guard = self.inner.mutable.lock().unwrap();
        guard.resolved_colors = Some((terminal, colors.clone()));
        colors
    }

    pub fn color(&self, token: ThemeToken) -> Option<Color> {
        self.colors().get(&token).copied()
    }

    pub fn style(&self, text: &str, mut options: ThemeStyle) -> String {
        let fg_ansi = match options.fg {
            None => None,
            Some(ThemeStyleFg::Token(tok)) => {
                if self.inner.dim_tokens.contains(&tok) {
                    options.dim = Some(true);
                }
                Some(self.fg_token_ansi(tok).unwrap_or_default().to_string())
            }
            Some(ThemeStyleFg::Color(c)) => Some(foreground_ansi(c, self.inner.mode)),
        };
        let bg_ansi = match options.bg {
            None => None,
            Some(ThemeStyleBg::Token(tok)) => Some(self.bg_token_ansi(tok).unwrap_or_default().to_string()),
            Some(ThemeStyleBg::Color(c)) => Some(background_ansi(c, self.inner.mode)),
        };
        style_text_with_ansi(
            text,
            fg_ansi.as_deref(),
            bg_ansi.as_deref(),
            TextAttributes {
                bold: options.bold,
                dim: options.dim,
                italic: options.italic,
                underline: options.underline,
                inverse: options.inverse,
                strikethrough: options.strikethrough,
            },
        )
    }

    pub fn try_style_tokens(
        &self,
        text: &str,
        fg_token: Option<&str>,
        bg_token: Option<&str>,
        attrs: TextAttributes,
    ) -> pi_js::Result<String> {
        let fg = match fg_token {
            None => None,
            Some(s) => Some(ThemeStyleFg::Token(ThemeColor::from_token_str(s)?)),
        };
        let bg = match bg_token {
            None => None,
            Some(s) => Some(ThemeStyleBg::Token(ThemeBg::from_token_str(s)?)),
        };
        Ok(self.style(
            text,
            ThemeStyle {
                fg,
                bg,
                bold: attrs.bold,
                dim: attrs.dim,
                italic: attrs.italic,
                underline: attrs.underline,
                inverse: attrs.inverse,
                strikethrough: attrs.strikethrough,
            },
        ))
    }

    pub fn fg(&self, color: ThemeColor, text: &str) -> String {
        let ansi = self.fg_token_ansi(color).unwrap_or("");
        if self.inner.dim_tokens.contains(&color) {
            format!("{ansi}\x1b[2m{text}\x1b[22;39m")
        } else {
            format!("{ansi}{text}\x1b[39m")
        }
    }

    pub fn bg(&self, color: ThemeBg, text: &str) -> String {
        let ansi = self.bg_token_ansi(color).unwrap_or("");
        format!("{ansi}{text}\x1b[49m")
    }

    fn fg_token_ansi(&self, token: ThemeColor) -> pi_js::Result<&str> {
        self.inner
            .fg_ansi
            .get(&token)
            .map(String::as_str)
            .ok_or_else(|| pi_js::Error::msg(format!("Unknown theme color: {}", token.as_str())))
    }

    fn bg_token_ansi(&self, token: ThemeBg) -> pi_js::Result<&str> {
        self.inner
            .bg_ansi
            .get(&token)
            .map(String::as_str)
            .ok_or_else(|| pi_js::Error::msg(format!("Unknown theme color: {}", token.as_str())))
    }

    pub fn bold(&self, text: &str) -> String {
        Chalk::new(Some(3)).bold().apply(text)
    }

    pub fn italic(&self, text: &str) -> String {
        Chalk::new(Some(3)).italic().apply(text)
    }

    pub fn underline(&self, text: &str) -> String {
        Chalk::new(Some(3)).underline().apply(text)
    }

    pub fn inverse(&self, text: &str) -> String {
        Chalk::new(Some(3)).inverse().apply(text)
    }

    pub fn strikethrough(&self, text: &str) -> String {
        Chalk::new(Some(3)).strikethrough().apply(text)
    }

    pub fn get_fg_ansi(&self, color: ThemeColor) -> String {
        let ansi = self.fg_token_ansi(color).unwrap_or("");
        if self.inner.dim_tokens.contains(&color) {
            format!("{ansi}\x1b[2m")
        } else {
            ansi.to_string()
        }
    }

    pub fn get_bg_ansi(&self, color: ThemeBg) -> String {
        self.bg_token_ansi(color).unwrap_or("").to_string()
    }

    pub fn get_color_mode(&self) -> TerminalColorMode {
        self.inner.mode
    }

    pub fn get_thinking_border_color(&self, level: ThinkingLevel) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        let this = self.clone();
        let tok = match level {
            ThinkingLevel::Off => ThemeColor::ThinkingOff,
            ThinkingLevel::Minimal => ThemeColor::ThinkingMinimal,
            ThinkingLevel::Low => ThemeColor::ThinkingLow,
            ThinkingLevel::Medium => ThemeColor::ThinkingMedium,
            ThinkingLevel::High => ThemeColor::ThinkingHigh,
            ThinkingLevel::Xhigh => ThemeColor::ThinkingXhigh,
        };
        Arc::new(move |s: &str| this.fg(tok, s))
    }

    pub fn get_thinking_max_border_color(&self) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        let this = self.clone();
        Arc::new(move |s: &str| this.fg(ThemeColor::ThinkingMax, s))
    }

    pub fn get_thinking_border_color_by_name(&self, level: &str) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        let this = self.clone();
        let tok = match level {
            "off" => ThemeColor::ThinkingOff,
            "minimal" => ThemeColor::ThinkingMinimal,
            "low" => ThemeColor::ThinkingLow,
            "medium" => ThemeColor::ThinkingMedium,
            "high" => ThemeColor::ThinkingHigh,
            "xhigh" => ThemeColor::ThinkingXhigh,
            "max" => ThemeColor::ThinkingMax,
            _ => ThemeColor::ThinkingOff,
        };
        Arc::new(move |s: &str| this.fg(tok, s))
    }

    pub fn get_bash_mode_border_color(&self) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        let this = self.clone();
        Arc::new(move |s: &str| this.fg(ThemeColor::BashMode, s))
    }
}

impl crate::core::extensions::types::ExtensionTheme for Theme {
    fn fg(&self, color: &str, text: &str) -> String {
        ThemeColor::from_token_str(color).map_or_else(|_| text.to_owned(), |c| Self::fg(self, c, text))
    }

    fn bg(&self, bg: &str, text: &str) -> String {
        ThemeBg::from_token_str(bg).map_or_else(|_| text.to_owned(), |b| Self::bg(self, b, text))
    }

    fn bold(&self, text: &str) -> String {
        Self::bold(self, text)
    }

    fn italic(&self, text: &str) -> String {
        Self::italic(self, text)
    }

    fn underline(&self, text: &str) -> String {
        Self::underline(self, text)
    }

    fn inverse(&self, text: &str) -> String {
        Self::inverse(self, text)
    }

    fn strikethrough(&self, text: &str) -> String {
        Self::strikethrough(self, text)
    }

    fn get_fg_ansi(&self, color: &str) -> String {
        ThemeColor::from_token_str(color).map_or_else(|_| String::new(), |c| Self::get_fg_ansi(self, c))
    }

    fn to_interactive_theme(&self) -> Option<Theme> {
        Some(self.clone())
    }

    fn get_bg_ansi(&self, bg: &str) -> String {
        ThemeBg::from_token_str(bg).map_or_else(|_| String::new(), |b| Self::get_bg_ansi(self, b))
    }
}

// ============================================================================
// Theme Loading
// ============================================================================

const BUILTIN_DARK_JSON: &str = include_str!("dark.json");
const BUILTIN_LIGHT_JSON: &str = include_str!("light.json");

static BUILTIN_THEMES: OnceLock<(ThemeJson, ThemeJson)> = OnceLock::new();

fn parse_builtin_theme_str(label: &str, raw: &str) -> ThemeJson {
    let val: Value = pi_js::json::parse(strip_bom(raw)).unwrap_or(Value::Null);
    super::theme_schema::validate_theme_json(label, &val).unwrap_or_else(|_| ThemeJson {
        schema: None,
        name: label.to_string(),
        appearance: None,
        vars: None,
        colors: IndexMap::new(),
        export: None,
    })
}

fn builtin_theme_json(name: &str) -> Option<&'static ThemeJson> {
    let (dark, light) = BUILTIN_THEMES.get_or_init(|| {
        let themes_dir = crate::config::get_themes_dir();
        let dark_path = pi_js::path::join(&[&themes_dir, "dark.json"]);
        let light_path = pi_js::path::join(&[&themes_dir, "light.json"]);
        let dark_raw = pi_js::fs::read_to_string(&dark_path).unwrap_or_else(|_| BUILTIN_DARK_JSON.to_string());
        let light_raw = pi_js::fs::read_to_string(&light_path).unwrap_or_else(|_| BUILTIN_LIGHT_JSON.to_string());
        (
            parse_builtin_theme_str("dark", &dark_raw),
            parse_builtin_theme_str("light", &light_raw),
        )
    });
    match name {
        "dark" => Some(dark),
        "light" => Some(light),
        _ => None,
    }
}

pub fn get_available_themes() -> Vec<String> {
    get_available_themes_with_paths()
        .into_iter()
        .map(|info| info.name)
        .collect()
}

pub fn get_available_themes_with_paths() -> Vec<ThemeInfo> {
    let themes_dir = crate::config::get_themes_dir();
    let mut result = Vec::new();
    let mut seen = IndexSet::new();
    let mut add_theme = |info: ThemeInfo| {
        if seen.insert(info.name.clone()) {
            result.push(info);
        }
    };

    add_theme(ThemeInfo {
        name: SYSTEM_THEME_NAME.to_string(),
        path: None,
    });
    for name in ["dark", "light"] {
        add_theme(ThemeInfo {
            name: name.to_string(),
            path: Some(pi_js::path::join(&[&themes_dir, &format!("{name}.json")])),
        });
    }

    for info in get_custom_theme_infos() {
        add_theme(info);
    }

    {
        let state = GLOBAL_STATE.lock().unwrap();
        for (name, t) in &state.registered_themes {
            add_theme(ThemeInfo {
                name: name.clone(),
                path: t.source_path().map(str::to_string),
            });
        }
    }

    result.sort_by(|a, b| {
        if a.name == SYSTEM_THEME_NAME && b.name != SYSTEM_THEME_NAME {
            std::cmp::Ordering::Less
        } else if b.name == SYSTEM_THEME_NAME && a.name != SYSTEM_THEME_NAME {
            std::cmp::Ordering::Greater
        } else {
            a.name.cmp(&b.name)
        }
    });
    result
}

fn get_custom_theme_infos() -> Vec<ThemeInfo> {
    let custom_dir = crate::config::get_custom_themes_dir();
    let mut result = Vec::new();
    if !pi_js::fs::exists(&custom_dir) {
        return result;
    }
    let Ok(entries) = pi_js::fs::readdir(&custom_dir) else {
        return result;
    };
    for file in entries {
        if !file.ends_with(".json") {
            continue;
        }
        let theme_path = pi_js::path::join(&[&custom_dir, &file]);
        if let Ok(custom_theme) = load_theme_from_path(&theme_path, None)
            && let Some(name) = custom_theme.name()
            && !name.is_empty()
        {
            result.push(ThemeInfo {
                name: name.to_string(),
                path: Some(theme_path),
            });
        }
    }
    result
}

fn assert_theme_name_is_valid(name: &str) -> pi_js::Result<()> {
    if name.contains('/') {
        return Err(pi_js::Error::msg(format!(
            "Invalid theme name \"{name}\": theme names cannot contain \"/\" because it is reserved for automatic light/dark theme settings."
        )));
    }
    Ok(())
}

fn parse_theme_json(label: &str, json: &Value) -> pi_js::Result<ThemeJson> {
    let validator = GLOBAL_STATE.lock().unwrap().theme_json_validator.clone();
    if let Some(validate) = validator {
        return validate(label, json);
    }
    let Some(obj) = json.as_object() else {
        return Err(pi_js::Error::msg(format!(
            "Invalid theme \"{label}\": expected an object with a \"colors\" map."
        )));
    };
    if !obj.contains_key("colors") {
        return Err(pi_js::Error::msg(format!(
            "Invalid theme \"{label}\": expected an object with a \"colors\" map."
        )));
    }
    if let Some(name) = obj.get("name").and_then(Value::as_str) {
        assert_theme_name_is_valid(name)?;
    }
    serde_json::from_value::<ThemeJson>(json.clone())
        .map_err(|e| pi_js::Error::msg(format!("Failed to parse theme {label}: {e}")))
}

fn parse_theme_json_content(label: &str, content: &str) -> pi_js::Result<ThemeJson> {
    let json: Value = pi_js::json::parse(strip_bom(content))
        .map_err(|e| pi_js::Error::msg(format!("Failed to parse theme {label}: {e}")))?;
    parse_theme_json(label, &json)
}

fn load_theme_json(name: &str) -> pi_js::Result<ThemeJson> {
    if let Some(builtin) = builtin_theme_json(name) {
        return Ok(builtin.clone());
    }
    let registered_info = {
        let state = GLOBAL_STATE.lock().unwrap();
        state
            .registered_themes
            .get(name)
            .map(|t| (t.source_path().map(str::to_string), true))
    };
    if let Some((source_path_opt, _)) = registered_info {
        if let Some(source_path) = source_path_opt {
            let content = pi_js::fs::read_to_string(&source_path)?;
            return parse_theme_json_content(&source_path, &content);
        }
        return Err(pi_js::Error::msg(format!(
            "Theme \"{name}\" does not have a source path for export"
        )));
    }
    let custom_dir = crate::config::get_custom_themes_dir();
    let theme_path = pi_js::path::join(&[&custom_dir, &format!("{name}.json")]);
    if !pi_js::fs::exists(&theme_path) {
        return Err(pi_js::Error::msg(format!("Theme not found: {name}")));
    }
    let content = pi_js::fs::read_to_string(&theme_path)?;
    parse_theme_json_content(name, &content)
}

fn create_theme(
    theme_json: &ThemeJson,
    mode: Option<TerminalColorMode>,
    source_path: Option<&str>,
) -> pi_js::Result<Theme> {
    let color_mode = mode.unwrap_or_else(|| get_terminal_color_mode(None));
    let resolved_colors = resolve_theme_colors(
        &with_theme_color_fallbacks(&theme_json.colors),
        theme_json.vars.as_ref(),
    )?;
    let (fg_colors, bg_colors) = split_theme_colors(&resolved_colors);
    let appearance = theme_json.appearance.map(|a| match a {
        super::theme_schema::ThemeJsonAppearance::Dark => ThemeAppearance::Dark,
        super::theme_schema::ThemeJsonAppearance::Light => ThemeAppearance::Light,
    });
    Theme::from_token_maps(
        &fg_colors,
        &bg_colors,
        color_mode,
        ThemeOptions {
            name: Some(theme_json.name.clone()),
            source_path: source_path.map(str::to_string),
            source_info: None,
            appearance,
            dim: Vec::new(),
        },
    )
}

fn create_system_theme(mode: Option<TerminalColorMode>) -> Theme {
    let (terminal_colors, terminal_colors_pending) = {
        let state = GLOBAL_STATE.lock().unwrap();
        (Arc::clone(&state.terminal_colors), state.terminal_colors_pending)
    };
    let generated = generate_system_theme_colors(&SystemThemeInput {
        foreground: terminal_colors.foreground,
        background: terminal_colors.background,
        palette: terminal_colors.palette.clone(),
        saturation: Some(if terminal_colors_pending { 0.0 } else { 1.0 }),
        appearance_hint: Some(get_terminal_theme()),
    });
    let mut fg_colors = IndexMap::new();
    let mut bg_colors = IndexMap::new();
    for (tok, val) in generated.colors {
        match tok {
            ThemeToken::Fg(fg) => {
                fg_colors.insert(fg, val);
            }
            ThemeToken::Bg(bg) => {
                bg_colors.insert(bg, val);
            }
        }
    }
    let resolved_mode = mode.unwrap_or_else(|| get_terminal_color_mode(None));
    Theme::from_token_maps(
        &fg_colors,
        &bg_colors,
        resolved_mode,
        ThemeOptions {
            name: Some(SYSTEM_THEME_NAME.to_string()),
            source_path: None,
            source_info: None,
            appearance: generated.appearance,
            dim: generated.dim,
        },
    )
    .unwrap_or_else(|_| {
        Theme::from_token_maps(
            &IndexMap::new(),
            &IndexMap::new(),
            resolved_mode,
            ThemeOptions::default(),
        )
        .expect("empty fallback theme builds")
    })
}

pub fn load_theme_from_path(theme_path: &str, mode: Option<TerminalColorMode>) -> pi_js::Result<Theme> {
    let content = pi_js::fs::read_to_string(theme_path)?;
    let theme_json = parse_theme_json_content(theme_path, &content)?;
    create_theme(&theme_json, mode, Some(theme_path))
}

fn load_theme(name: &str, mode: Option<TerminalColorMode>) -> pi_js::Result<Theme> {
    if name == SYSTEM_THEME_NAME {
        return Ok(create_system_theme(mode));
    }
    if let Some(registered) = GLOBAL_STATE.lock().unwrap().registered_themes.get(name).cloned() {
        return Ok(registered);
    }
    let theme_json = load_theme_json(name)?;
    create_theme(&theme_json, mode, None)
}

pub fn get_theme_by_name(name: &str) -> Option<Theme> {
    load_theme(name, None).ok()
}

pub fn parse_auto_theme_setting(theme_setting: Option<&str>) -> Option<(String, String)> {
    let setting = theme_setting.filter(|s| !s.is_empty())?;
    let slash_index = setting.find('/')?;
    if setting[slash_index + 1..].contains('/') {
        return None;
    }
    let light = setting[..slash_index].trim();
    let dark = setting[slash_index + 1..].trim();
    if light.is_empty() || dark.is_empty() {
        return None;
    }
    Some((light.to_string(), dark.to_string()))
}

pub fn resolve_theme_setting(theme_setting: Option<&str>, terminal_theme: TerminalTheme) -> Option<String> {
    if let Some((light, dark)) = parse_auto_theme_setting(theme_setting) {
        return Some(match terminal_theme {
            TerminalTheme::Light => light,
            TerminalTheme::Dark => dark,
        });
    }
    let setting = theme_setting?;
    if setting.contains('/') {
        return None;
    }
    Some(setting.to_string())
}

pub fn detect_color_fg_bg_theme(colorfgbg: Option<&str>) -> Option<TerminalTheme> {
    let raw = match colorfgbg {
        Some(v) => Some(v.to_string()),
        None => pi_js::env::var("COLORFGBG"),
    }?;
    let bg = raw.rsplit(';').next()?.trim();
    if bg.is_empty() || bg.len() > 2 || !bg.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let index: u8 = bg.parse().ok()?;
    if index > 15 {
        return None;
    }
    Some(if index <= 6 || index == 8 {
        TerminalTheme::Dark
    } else {
        TerminalTheme::Light
    })
}

pub fn detect_terminal_theme(
    colors: &TerminalColors,
    reported_scheme: Option<TerminalTheme>,
    colorfgbg: Option<&str>,
) -> TerminalTheme {
    if let Some(bg) = colors.background {
        return terminal_appearance(bg, colors.foreground);
    }
    reported_scheme
        .or_else(|| detect_color_fg_bg_theme(colorfgbg))
        .unwrap_or(TerminalTheme::Dark)
}

pub fn get_terminal_theme() -> TerminalTheme {
    let (colors, scheme) = {
        let state = GLOBAL_STATE.lock().unwrap();
        (Arc::clone(&state.terminal_colors), state.terminal_color_scheme)
    };
    detect_terminal_theme(&colors, scheme, None)
}

// ============================================================================
// Global Theme Instance
// ============================================================================

pub fn try_theme() -> pi_js::Result<Theme> {
    GLOBAL_STATE
        .lock()
        .unwrap()
        .active_theme
        .clone()
        .ok_or_else(|| pi_js::Error::msg("Theme not initialized. Call initTheme() first."))
}

pub fn theme() -> Theme {
    if let Some(t) = GLOBAL_STATE.lock().unwrap().active_theme.clone() {
        return t;
    }
    create_system_theme(None)
}

fn set_global_theme(t: Theme) {
    GLOBAL_STATE.lock().unwrap().active_theme = Some(t);
}

pub fn set_registered_themes(themes: Vec<Theme>) -> pi_js::Result<()> {
    let mut next = IndexMap::new();
    for t in themes {
        if let Some(name) = t.name() {
            assert_theme_name_is_valid(name)?;
            next.insert(name.to_string(), t);
        }
    }
    GLOBAL_STATE.lock().unwrap().registered_themes = next;
    Ok(())
}

pub fn init_theme(theme_name: Option<&str>, enable_watcher: Option<bool>) {
    let name = theme_name.unwrap_or(SYSTEM_THEME_NAME);
    {
        GLOBAL_STATE.lock().unwrap().current_theme_name = Some(name.to_string());
    }
    match load_theme(name, None) {
        Ok(loaded) => {
            set_global_theme(loaded);
            if enable_watcher.unwrap_or(false) {
                start_theme_watcher();
            }
        }
        Err(_) => {
            GLOBAL_STATE.lock().unwrap().current_theme_name = Some(SYSTEM_THEME_NAME.to_string());
            set_global_theme(create_system_theme(None));
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetThemeResult {
    pub success: bool,
    pub error: Option<String>,
}

pub fn set_theme(name: &str, enable_watcher: Option<bool>) -> SetThemeResult {
    {
        GLOBAL_STATE.lock().unwrap().current_theme_name = Some(name.to_string());
    }
    match load_theme(name, None) {
        Ok(loaded) => {
            set_global_theme(loaded);
            if enable_watcher.unwrap_or(false) {
                start_theme_watcher();
            }
            let cb = GLOBAL_STATE.lock().unwrap().on_theme_change.clone();
            if let Some(cb) = cb {
                cb();
            }
            SetThemeResult {
                success: true,
                error: None,
            }
        }
        Err(err) => {
            GLOBAL_STATE.lock().unwrap().current_theme_name = Some(SYSTEM_THEME_NAME.to_string());
            set_global_theme(create_system_theme(None));
            SetThemeResult {
                success: false,
                error: Some(err.to_string()),
            }
        }
    }
}

pub fn set_theme_instance(theme_instance: Theme) {
    stop_theme_watcher();
    let cb = {
        let mut state = GLOBAL_STATE.lock().unwrap();
        state.active_theme = Some(theme_instance);
        state.current_theme_name = Some("<in-memory>".to_string());
        state.on_theme_change.clone()
    };
    if let Some(cb) = cb {
        cb();
    }
}

pub fn on_theme_change(callback: Option<Arc<dyn Fn() + Send + Sync>>) {
    GLOBAL_STATE.lock().unwrap().on_theme_change = callback;
}

fn start_theme_watcher() {
    stop_theme_watcher();
    let current = GLOBAL_STATE.lock().unwrap().current_theme_name.clone();
    let Some(watched_name) = current else {
        return;
    };
    if watched_name.is_empty() || watched_name == "dark" || watched_name == "light" || watched_name == SYSTEM_THEME_NAME
    {
        return;
    }
    let custom_dir = crate::config::get_custom_themes_dir();
    let watched_file_name = format!("{watched_name}.json");
    let theme_file = pi_js::path::join(&[&custom_dir, &watched_file_name]);
    if !pi_js::fs::exists(&theme_file) {
        return;
    }

    let watched_name_for_cb = watched_name.clone();
    let theme_file_for_cb = theme_file.clone();
    let schedule_reload = Arc::new(move || {
        let watched_name_timer = watched_name_for_cb.clone();
        let theme_file_timer = theme_file_for_cb.clone();
        let new_timer = pi_js::time::set_timeout(100, move || {
            let still_active = {
                let mut state = GLOBAL_STATE.lock().unwrap();
                state.theme_reload_timer = None;
                state.current_theme_name.as_deref() == Some(watched_name_timer.as_str())
            };
            if !still_active || !pi_js::fs::exists(&theme_file_timer) {
                return;
            }
            if let Ok(reloaded_theme) = load_theme_from_path(&theme_file_timer, None) {
                let cb = {
                    let mut state = GLOBAL_STATE.lock().unwrap();
                    if state.current_theme_name.as_deref() != Some(watched_name_timer.as_str()) {
                        None
                    } else {
                        state
                            .registered_themes
                            .insert(watched_name_timer.clone(), reloaded_theme.clone());
                        state.active_theme = Some(reloaded_theme);
                        state.on_theme_change.clone()
                    }
                };
                if let Some(cb) = cb {
                    cb();
                }
            }
        });
        let prev = {
            let mut state = GLOBAL_STATE.lock().unwrap();
            state.theme_reload_timer.replace(new_timer)
        };
        if let Some(prev) = prev {
            prev.clear();
        }
    });

    let watcher_slot: Arc<Mutex<Option<Arc<FsWatcher>>>> = Arc::new(Mutex::new(None));
    let watcher_slot_err = Arc::clone(&watcher_slot);
    let watched_name_listener = watched_name;
    let watched_file_listener = watched_file_name;

    if let Some(w) = watch_with_error_handler(
        &custom_dir,
        move |_event_type, filename| {
            let matches_current =
                { GLOBAL_STATE.lock().unwrap().current_theme_name.as_deref() == Some(watched_name_listener.as_str()) };
            if !matches_current {
                return;
            }
            if filename.is_empty() || filename == watched_file_listener {
                schedule_reload();
            }
        },
        move || {
            let w = watcher_slot_err.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(ref w_arc) = w {
                let mut state = GLOBAL_STATE.lock().unwrap();
                if state.watcher.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, w_arc)) {
                    state.watcher = None;
                }
            }
            close_watcher(w.as_deref());
        },
    ) {
        *watcher_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&w));
        let prev = {
            let mut state = GLOBAL_STATE.lock().unwrap();
            state.watcher.replace(w)
        };
        close_watcher(prev.as_deref());
    }
}

pub fn stop_theme_watcher() {
    let (timer, watcher) = {
        let mut state = GLOBAL_STATE.lock().unwrap();
        (state.theme_reload_timer.take(), state.watcher.take())
    };
    if let Some(timer) = timer {
        timer.clear();
    }
    close_watcher(watcher.as_deref());
}

// ============================================================================
// HTML Export Helpers
// ============================================================================

pub fn get_resolved_theme_colors(theme_name: Option<&str>) -> IndexMap<String, String> {
    let name = theme_name
        .map(str::to_string)
        .or_else(|| GLOBAL_STATE.lock().unwrap().current_theme_name.clone())
        .unwrap_or_else(|| SYSTEM_THEME_NAME.to_string());
    let loaded = load_theme(&name, None).unwrap_or_else(|_| create_system_theme(None));
    let mut out = IndexMap::new();
    for (tok, color) in loaded.colors() {
        out.insert(tok.as_str().to_string(), color_to_hex(color));
    }
    out
}

pub fn is_light_theme(theme_name: Option<&str>) -> bool {
    let name = theme_name
        .map(str::to_string)
        .or_else(|| GLOBAL_STATE.lock().unwrap().current_theme_name.clone())
        .unwrap_or_else(|| SYSTEM_THEME_NAME.to_string());
    let loaded = load_theme(&name, None).unwrap_or_else(|_| create_system_theme(None));
    loaded.appearance() == ThemeAppearance::Light
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemeExportColors {
    pub page_bg: Option<String>,
    pub card_bg: Option<String>,
    pub info_bg: Option<String>,
}

pub fn get_theme_export_colors(theme_name: Option<&str>) -> ThemeExportColors {
    let name = theme_name
        .map(str::to_string)
        .or_else(|| GLOBAL_STATE.lock().unwrap().current_theme_name.clone())
        .unwrap_or_else(|| SYSTEM_THEME_NAME.to_string());
    if name == SYSTEM_THEME_NAME {
        return ThemeExportColors::default();
    }
    let Ok(theme_json) = load_theme_json(&name) else {
        return ThemeExportColors::default();
    };
    let Some(export_section) = theme_json.export else {
        return ThemeExportColors::default();
    };
    let empty_vars = IndexMap::new();
    let vars = theme_json.vars.as_ref().unwrap_or(&empty_vars);

    let resolve = |value: Option<&ThemeColorValue>| -> pi_js::Result<Option<String>> {
        let Some(val) = value else {
            return Ok(None);
        };
        let mut visited = IndexSet::new();
        let resolved = resolve_var_refs(val, vars, &mut visited)?;
        match resolved {
            ThemeColorValue::Indexed(idx) => Ok(Some(color_to_hex(Color::Indexed(indexed_color(idx as f64)?)))),
            ThemeColorValue::String(s) if s.is_empty() => Ok(None),
            ThemeColorValue::String(s) if OKHSL_PREFIX_RE.is_match(&s) => {
                Ok(Some(color_to_hex(parse_color(ParseColorValue::Text(&s))?)))
            }
            ThemeColorValue::String(s) => Ok(Some(s)),
        }
    };

    let Ok(page_bg) = resolve(export_section.page_bg.as_ref()) else {
        return ThemeExportColors::default();
    };
    let Ok(card_bg) = resolve(export_section.card_bg.as_ref()) else {
        return ThemeExportColors::default();
    };
    let Ok(info_bg) = resolve(export_section.info_bg.as_ref()) else {
        return ThemeExportColors::default();
    };
    ThemeExportColors {
        page_bg,
        card_bg,
        info_bg,
    }
}

// ============================================================================
// TUI Helpers
// ============================================================================

static CACHED_HIGHLIGHT_THEME: LazyLock<Mutex<Option<(usize, HighlightTheme)>>> = LazyLock::new(|| Mutex::new(None));

fn fg_formatter(t: &Theme, token: ThemeColor) -> HighlightFormatter {
    let t = t.clone();
    Arc::new(move |s| t.fg(token, s))
}

fn build_cli_highlight_theme(t: &Theme) -> HighlightTheme {
    let t_italic = t.clone();
    let t_bold = t.clone();
    let t_underline = t.clone();
    IndexMap::from([
        ("keyword".to_string(), fg_formatter(t, ThemeColor::SyntaxKeyword)),
        ("built_in".to_string(), fg_formatter(t, ThemeColor::SyntaxType)),
        ("literal".to_string(), fg_formatter(t, ThemeColor::SyntaxNumber)),
        ("number".to_string(), fg_formatter(t, ThemeColor::SyntaxNumber)),
        ("regexp".to_string(), fg_formatter(t, ThemeColor::SyntaxString)),
        ("string".to_string(), fg_formatter(t, ThemeColor::SyntaxString)),
        ("subst".to_string(), fg_formatter(t, ThemeColor::Text)),
        ("comment".to_string(), fg_formatter(t, ThemeColor::SyntaxComment)),
        ("doctag".to_string(), fg_formatter(t, ThemeColor::SyntaxComment)),
        ("meta".to_string(), fg_formatter(t, ThemeColor::Muted)),
        ("function".to_string(), fg_formatter(t, ThemeColor::SyntaxFunction)),
        ("title".to_string(), fg_formatter(t, ThemeColor::SyntaxFunction)),
        ("class".to_string(), fg_formatter(t, ThemeColor::SyntaxType)),
        ("type".to_string(), fg_formatter(t, ThemeColor::SyntaxType)),
        ("tag".to_string(), fg_formatter(t, ThemeColor::SyntaxPunctuation)),
        ("name".to_string(), fg_formatter(t, ThemeColor::SyntaxKeyword)),
        ("attr".to_string(), fg_formatter(t, ThemeColor::SyntaxVariable)),
        ("variable".to_string(), fg_formatter(t, ThemeColor::SyntaxVariable)),
        ("params".to_string(), fg_formatter(t, ThemeColor::SyntaxVariable)),
        ("operator".to_string(), fg_formatter(t, ThemeColor::SyntaxOperator)),
        (
            "punctuation".to_string(),
            fg_formatter(t, ThemeColor::SyntaxPunctuation),
        ),
        (
            "emphasis".to_string(),
            Arc::new(move |s: &str| t_italic.italic(s)) as HighlightFormatter,
        ),
        (
            "strong".to_string(),
            Arc::new(move |s: &str| t_bold.bold(s)) as HighlightFormatter,
        ),
        (
            "link".to_string(),
            Arc::new(move |s: &str| t_underline.underline(s)) as HighlightFormatter,
        ),
        ("addition".to_string(), fg_formatter(t, ThemeColor::ToolDiffAdded)),
        ("deletion".to_string(), fg_formatter(t, ThemeColor::ToolDiffRemoved)),
    ])
}

fn get_cli_highlight_theme(t: &Theme) -> HighlightTheme {
    let ptr = Arc::as_ptr(&t.inner) as usize;
    let mut guard = CACHED_HIGHLIGHT_THEME.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((cached_ptr, ref cached)) = *guard
        && cached_ptr == ptr
    {
        return cached.clone();
    }
    let built = build_cli_highlight_theme(t);
    *guard = Some((ptr, built.clone()));
    built
}

pub fn highlight_code(code: &str, lang: Option<&str>) -> Vec<String> {
    let t = theme();
    let valid_lang = lang.map(str::trim).filter(|l| !l.is_empty() && supports_language(l));
    let Some(valid_lang) = valid_lang else {
        return code
            .split('\n')
            .map(|line| t.fg(ThemeColor::MdCodeBlock, line))
            .collect();
    };
    let highlighted = highlight(
        code,
        HighlightOptions {
            language: Some(valid_lang.to_string()),
            ignore_illegals: Some(true),
            language_subset: None,
            theme: Some(get_cli_highlight_theme(&t)),
        },
    );
    highlighted.split('\n').map(str::to_string).collect()
}

// SAFETY-FREE Note: we store usize pointer address rather than raw *const to remain Send+Sync without unsafe.
struct CodeHighlighterCache {
    code: String,
    lang: Option<String>,
    theme_ptr: Option<usize>,
    language_count: usize,
    lines: Arc<Vec<String>>,
}

/// Highlights code for one owner and reuses the last result while code, language, theme,
/// and registered grammars stay unchanged.
#[derive(Clone, Default)]
pub struct CodeHighlighter {
    last: Arc<Mutex<Option<CodeHighlighterCache>>>,
}

impl CodeHighlighter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn highlight(&self, code: &str, lang: Option<&str>) -> Arc<Vec<String>> {
        let current_theme = GLOBAL_STATE.lock().unwrap().active_theme.clone();
        let theme_ptr = current_theme.as_ref().map(|t| Arc::as_ptr(&t.inner) as usize);
        let language_count = get_highlight_language_count();
        {
            let guard = self.last.lock().unwrap();
            if let Some(ref prev) = *guard
                && prev.code == code
                && prev.lang.as_deref() == lang
                && prev.theme_ptr == theme_ptr
                && prev.language_count == language_count
            {
                return Arc::clone(&prev.lines);
            }
        }
        let lines = Arc::new(highlight_code(code, lang));
        let mut guard = self.last.lock().unwrap();
        *guard = Some(CodeHighlighterCache {
            code: code.to_string(),
            lang: lang.map(str::to_string),
            theme_ptr,
            language_count,
            lines: Arc::clone(&lines),
        });
        lines
    }
}

pub fn get_language_from_path(file_path: &str) -> Option<&'static str> {
    let ext = file_path.rsplit('.').next()?.to_lowercase();
    match ext.as_str() {
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "py" => Some("python"),
        "rb" => Some("ruby"),
        "rs" => Some("rust"),
        "go" => Some("go"),
        "java" => Some("java"),
        "kt" => Some("kotlin"),
        "swift" => Some("swift"),
        "dart" => Some("dart"),
        "groovy" | "gradle" => Some("groovy"),
        "nix" => Some("nix"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp"),
        "cs" => Some("csharp"),
        "php" => Some("php"),
        "sh" | "bash" | "zsh" => Some("bash"),
        "fish" => Some("fish"),
        "ps1" => Some("powershell"),
        "sql" => Some("sql"),
        "html" | "htm" => Some("html"),
        "css" => Some("css"),
        "scss" => Some("scss"),
        "sass" => Some("sass"),
        "less" => Some("less"),
        "json" => Some("json"),
        "yaml" | "yml" => Some("yaml"),
        "toml" => Some("toml"),
        "xml" => Some("xml"),
        "md" | "markdown" => Some("markdown"),
        "dockerfile" => Some("dockerfile"),
        "makefile" => Some("makefile"),
        "cmake" => Some("cmake"),
        "lua" => Some("lua"),
        "perl" => Some("perl"),
        "r" => Some("r"),
        "scala" => Some("scala"),
        "clj" => Some("clojure"),
        "ex" | "exs" => Some("elixir"),
        "erl" => Some("erlang"),
        "hs" => Some("haskell"),
        "ml" => Some("ocaml"),
        "vim" => Some("vim"),
        "graphql" => Some("graphql"),
        "proto" => Some("protobuf"),
        "tf" | "hcl" => Some("hcl"),
        _ => None,
    }
}

pub fn get_markdown_theme() -> MarkdownTheme {
    MarkdownTheme {
        heading: Arc::new(|s| theme().fg(ThemeColor::MdHeading, s)),
        link: Arc::new(|s| theme().fg(ThemeColor::MdLink, s)),
        link_url: Arc::new(|s| theme().fg(ThemeColor::MdLinkUrl, s)),
        code: Arc::new(|s| theme().fg(ThemeColor::MdCode, s)),
        code_block: Arc::new(|s| theme().fg(ThemeColor::MdCodeBlock, s)),
        code_block_border: Arc::new(|s| theme().fg(ThemeColor::MdCodeBlockBorder, s)),
        quote: Arc::new(|s| theme().fg(ThemeColor::MdQuote, s)),
        quote_border: Arc::new(|s| theme().fg(ThemeColor::MdQuoteBorder, s)),
        hr: Arc::new(|s| theme().fg(ThemeColor::MdHr, s)),
        list_bullet: Arc::new(|s| theme().fg(ThemeColor::MdListBullet, s)),
        bold: Arc::new(|s| theme().bold(s)),
        italic: Arc::new(|s| theme().italic(s)),
        strikethrough: Arc::new(|s| theme().strikethrough(s)),
        underline: Arc::new(|s| theme().underline(s)),
        highlight_code: Some(Arc::new(|code, lang| highlight_code(code, lang))),
        code_block_indent: None,
    }
}

pub fn get_select_list_theme() -> SelectListTheme {
    SelectListTheme {
        selected_prefix: Arc::new(|s| theme().fg(ThemeColor::Accent, s)),
        selected_text: Arc::new(|s| theme().fg(ThemeColor::Accent, s)),
        description: Arc::new(|s| theme().fg(ThemeColor::Muted, s)),
        scroll_info: Arc::new(|s| theme().fg(ThemeColor::Muted, s)),
        no_match: Arc::new(|s| theme().fg(ThemeColor::Muted, s)),
    }
}

pub fn get_editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Arc::new(|s| theme().fg(ThemeColor::BorderMuted, s)),
        select_list: get_select_list_theme(),
    }
}

pub fn get_settings_list_theme() -> SettingsListTheme {
    SettingsListTheme {
        label: Arc::new(|text, selected| {
            if selected {
                theme().fg(ThemeColor::Accent, text)
            } else {
                text.to_string()
            }
        }),
        value: Arc::new(|text, selected| {
            if selected {
                theme().fg(ThemeColor::Accent, text)
            } else {
                theme().fg(ThemeColor::Muted, text)
            }
        }),
        description: Arc::new(|text| theme().fg(ThemeColor::Dim, text)),
        cursor: theme().fg(ThemeColor::Accent, "→ "),
        hint: Arc::new(|text| theme().fg(ThemeColor::Dim, text)),
    }
}

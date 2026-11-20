//! Port of packages/coding-agent/src/modes/interactive/components/provider-brand.ts

use std::sync::{Arc, LazyLock};

pub use crate::core::preferred_providers::{PREFERRED_PROVIDER_IDS, compare_providers, is_preferred_provider};
pub use crate::core::radius::RADIUS_PROVIDER_ID;
use pi_js::time::{Interval, set_interval};
use pi_tui::TUI;
use pi_tui::colors::{Color, ColorMixSpace, ParseColorValue, foreground_ansi, mix_colors, parse_color};

use super::super::theme::theme::{ThemeAppearance, theme};

/// Colors `text` for animation time `elapsed_ms`.
pub type ProviderBrandStyle = fn(&str, f64) -> String;

/// The animated style of a provider's name, or `None` when the provider has none.
pub fn get_provider_brand_style(provider_id: &str) -> Option<ProviderBrandStyle> {
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

/// Colors `text` with the Radius logo colors flowing left to right.
pub fn radius_shimmer(text: &str, elapsed_ms: f64) -> String {
    let mode = theme().get_color_mode();
    let cycle = (RADIUS_COLORS.len() as f64) * CHARS_PER_COLOR;
    let offset = (elapsed_ms / 1000.0) * CHARS_PER_SECOND;
    let mut result = String::new();
    for (index, ch) in text.chars().enumerate() {
        let position = ((((index as f64) - offset) % cycle) + cycle) % cycle;
        let band = ((position / CHARS_PER_COLOR).floor() as usize) % RADIUS_COLORS.len();
        let t = (position / CHARS_PER_COLOR - band as f64).clamp(0.0, 1.0);
        // Smoothstep keeps each band recognizable while still blending into the next one.
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

fn open_router_glow(text: &str, elapsed_ms: f64) -> String {
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

/// Re-renders `tui` every animation frame while `is_animating()` holds.
pub fn start_brand_animation(
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

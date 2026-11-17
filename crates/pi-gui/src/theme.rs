//! Waku graphite color palette and Rune Kit token mapping.

use std::sync::OnceLock;

use rune::Color;
use rune::theme::srgb_decode;
use rune_kit::Kit;
use serde::{Deserialize, Serialize};

/// User theme preference persisted and selectable in Settings.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// Resolves whether dark mode should be active given the host OS appearance.
    #[must_use]
    pub const fn is_dark(self, system_dark: bool) -> bool {
        match self {
            Self::System => system_dark,
            Self::Light => false,
            Self::Dark => true,
        }
    }
}

/// Shared layout constants matching Waku's desktop geometry.
pub struct LayoutMetrics;

impl LayoutMetrics {
    pub const DEFAULT_WINDOW_WIDTH: f32 = 1280.0;
    pub const DEFAULT_WINDOW_HEIGHT: f32 = 820.0;
    pub const MIN_WINDOW_WIDTH: f32 = 680.0;
    pub const MIN_WINDOW_HEIGHT: f32 = 460.0;
    pub const DEFAULT_SIDEBAR_WIDTH: f32 = 252.0;
    pub const SIDEBAR_MIN_WIDTH: f32 = 180.0;
    pub const SIDEBAR_MAX_WIDTH: f32 = 420.0;
    pub const DEFAULT_RIGHT_PANEL_WIDTH: f32 = 460.0;
    pub const RIGHT_PANEL_MIN_WIDTH: f32 = 280.0;
    pub const RIGHT_PANEL_MAX_WIDTH: f32 = 1000.0;
    pub const DEFAULT_FILE_TREE_WIDTH: f32 = 184.0;
    pub const CONTENT_MAX_WIDTH: f32 = 720.0;
    pub const NAV_RAIL_WIDTH: f32 = 44.0;
    pub const TITLEBAR_HEIGHT: f32 = 40.0;
    pub const DEFAULT_UI_FONT_SIZE: f32 = 14.0;

    #[must_use]
    pub const fn traffic_light_clearance() -> f32 {
        if cfg!(target_os = "macos") { 86.0 } else { 8.0 }
    }
}

/// Scales a point dimension authored at Waku's default 14 px UI font size.
#[must_use]
pub fn sp(value: f32) -> f32 {
    value * (14.0 / LayoutMetrics::DEFAULT_UI_FONT_SIZE)
}

/// Converts a packed 24-bit sRGB hex literal into a linear Rune [`Color`].
#[must_use]
pub fn srgb_hex(hex: u32) -> Color {
    let r = ((hex >> 16) & 0xff) as f32 / 255.0;
    let g = ((hex >> 8) & 0xff) as f32 / 255.0;
    let b = (hex & 0xff) as f32 / 255.0;
    Color {
        r: srgb_decode(r),
        g: srgb_decode(g),
        b: srgb_decode(b),
        a: 1.0,
    }
}

/// Converts sRGB `0..=1` components with straight alpha into a linear Rune [`Color`].
#[must_use]
pub fn srgba(r: f32, g: f32, b: f32, a: f32) -> Color {
    Color {
        r: srgb_decode(r),
        g: srgb_decode(g),
        b: srgb_decode(b),
        a,
    }
}

/// Converts HSLA (`h` in `[0.0, 1.0]`, `s`/`l`/`a` in `[0.0, 1.0]`) into a linear Rune [`Color`].
#[must_use]
pub fn hsla(h: f32, s: f32, l: f32, a: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = (h.rem_euclid(1.0)) * 6.0;
    let x = c * (1.0 - (h_prime % 2.0 - 1.0).abs());
    let (r1, g1, b1) = if h_prime < 1.0 {
        (c, x, 0.0)
    } else if h_prime < 2.0 {
        (x, c, 0.0)
    } else if h_prime < 3.0 {
        (0.0, c, x)
    } else if h_prime < 4.0 {
        (0.0, x, c)
    } else if h_prime < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = l - c * 0.5;
    srgba(r1 + m, g1 + m, b1 + m, a)
}

/// Waku's neutral graphite palette with coral accent, expressed in Rune linear [`Color`]s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WakuPalette {
    pub is_dark: bool,
    pub canvas: Color,
    pub sidebar: Color,
    pub sidebar_item_background: Color,
    pub surface: Color,
    pub raised: Color,
    pub composer: Color,
    pub inset: Color,
    pub terminal: Color,
    pub overlay: Color,
    pub overlay_strong: Color,

    pub border: Color,
    pub border_strong: Color,
    pub sidebar_border: Color,

    pub text: Color,
    pub text_secondary: Color,
    pub text_tertiary: Color,
    pub text_ghost: Color,

    /// Brand coral for live-activity pulses, caret, and brand marks.
    pub accent: Color,
    pub resize_handle: Color,
    pub gauge: Color,

    pub selection: Color,
    pub code_text: Color,
    pub code_wash: Color,

    pub inverse: Color,
    pub on_inverse: Color,

    pub warning: Color,
    pub success: Color,
    pub favorite: Color,
    pub danger: Color,
    pub danger_soft: Color,
}

impl WakuPalette {
    #[must_use]
    pub fn dark() -> Self {
        Self {
            is_dark: true,
            canvas: srgb_hex(0x1A1A1A),
            sidebar: srgb_hex(0x181818),
            sidebar_item_background: hsla(0.0, 0.0, 0.941, 0.06),
            surface: srgb_hex(0x1A1A1A),
            raised: srgb_hex(0x232323),
            composer: srgb_hex(0x212121),
            inset: srgb_hex(0x151515),
            terminal: srgb_hex(0x151515),
            overlay: hsla(220.0 / 360.0, 0.10, 0.90, 0.05),
            overlay_strong: hsla(220.0 / 360.0, 0.10, 0.90, 0.09),

            border: hsla(220.0 / 360.0, 0.10, 0.90, 0.07),
            border_strong: hsla(220.0 / 360.0, 0.10, 0.90, 0.14),
            sidebar_border: hsla(126.93 / 360.0, 0.0, 0.16077, 1.0),

            text: srgb_hex(0xE2E2E2),
            text_secondary: srgb_hex(0xA3A3A3),
            text_tertiary: srgb_hex(0x7D7D7D),
            text_ghost: srgb_hex(0x575757),

            accent: srgb_hex(0xE2795B),
            resize_handle: srgb_hex(0x3B82F6),
            gauge: srgb_hex(0x3B82F6),

            selection: hsla(211.0 / 360.0, 1.0, 0.50, 0.55),
            code_text: srgb_hex(0xE0A882),
            code_wash: hsla(220.0 / 360.0, 0.10, 0.90, 0.08),

            inverse: srgb_hex(0xE7E9EC),
            on_inverse: srgb_hex(0x17181C),

            warning: srgb_hex(0xE0B36A),
            success: srgb_hex(0x62C987),
            favorite: srgb_hex(0xEAB308),
            danger: srgb_hex(0xE2726A),
            danger_soft: hsla(4.0 / 360.0, 0.55, 0.63, 0.10),
        }
    }

    #[must_use]
    pub fn light() -> Self {
        Self {
            is_dark: false,
            canvas: srgb_hex(0xF6F5F6),
            sidebar: srgb_hex(0xF3F3F3),
            sidebar_item_background: hsla(0.0, 0.0, 0.078, 0.06),
            surface: srgb_hex(0xF6F5F6),
            raised: srgb_hex(0xECECEC),
            composer: srgb_hex(0xFFFFFF),
            inset: srgb_hex(0xE6E6E6),
            terminal: srgb_hex(0xFFFFFF),
            overlay: hsla(220.0 / 360.0, 0.10, 0.12, 0.05),
            overlay_strong: hsla(220.0 / 360.0, 0.10, 0.12, 0.09),

            border: hsla(220.0 / 360.0, 0.10, 0.12, 0.08),
            border_strong: hsla(220.0 / 360.0, 0.10, 0.12, 0.15),
            sidebar_border: hsla(0.0, 0.0, 0.078, 0.12),

            text: srgb_hex(0x242424),
            text_secondary: srgb_hex(0x666666),
            text_tertiary: srgb_hex(0x858585),
            text_ghost: srgb_hex(0xA4A4A4),

            accent: srgb_hex(0xC85F44),
            resize_handle: srgb_hex(0x2563EB),
            gauge: srgb_hex(0x2563EB),

            selection: hsla(211.0 / 360.0, 1.0, 0.50, 0.35),
            code_text: srgb_hex(0x9A5528),
            code_wash: hsla(220.0 / 360.0, 0.10, 0.12, 0.07),

            inverse: srgb_hex(0x202227),
            on_inverse: srgb_hex(0xF8F8F9),

            warning: srgb_hex(0xA66B20),
            success: srgb_hex(0x2F8F52),
            favorite: srgb_hex(0xCA8A04),
            danger: srgb_hex(0xC64A42),
            danger_soft: hsla(4.0 / 360.0, 0.55, 0.52, 0.10),
        }
    }
}

/// Returns the cached [`WakuPalette`] for dark or light mode.
#[must_use]
pub fn palette(dark: bool) -> WakuPalette {
    static DARK: OnceLock<WakuPalette> = OnceLock::new();
    static LIGHT: OnceLock<WakuPalette> = OnceLock::new();
    if dark {
        *DARK.get_or_init(WakuPalette::dark)
    } else {
        *LIGHT.get_or_init(WakuPalette::light)
    }
}

/// Returns a `rune_kit::Kit` whose semantic surface/text/border roles align with [`WakuPalette`].
#[must_use]
pub fn waku_kit(dark: bool) -> &'static Kit {
    static KITS: [[OnceLock<Kit>; 2]; 2] = [const { [const { OnceLock::new() }; 2] }; 2];
    let srgb = rune::theme::blends_srgb();
    KITS[dark as usize][srgb as usize].get_or_init(|| {
        let pal = palette(dark);
        let mut kit = rune_theme_opencode::kit(dark).clone();
        kit.c.page = pal.canvas;
        kit.c.bg_base = pal.canvas;
        kit.c.bg_deep = pal.inset;
        kit.c.layer_01 = pal.sidebar;
        kit.c.layer_02 = pal.composer;
        kit.c.layer_03 = pal.raised;
        kit.c.text_base = pal.text;
        kit.c.text_muted = pal.text_secondary;
        kit.c.text_faint = pal.text_tertiary;
        kit.c.text_accent = pal.accent;
        kit.c.border_muted = pal.border;
        kit.c.border_base = pal.border;
        kit.c.border_strong = pal.border_strong;
        kit.c.surface_base = pal.surface;
        kit.c.surface_raised = pal.raised;
        kit.c.surface_float = pal.raised;
        kit
    })
}

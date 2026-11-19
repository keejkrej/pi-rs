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

/// Spacing scale matching Waku desktop views (`4pt` base rhythm).
pub struct WakuSpacing;

impl WakuSpacing {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 20.0;
    pub const XXL: f32 = 24.0;
    pub const CARD_GAP: f32 = 12.0;
    pub const COMPOSER_PADDING: f32 = 12.0;
}

/// Corner radii aligned with Waku surfaces and modal cards.
pub struct WakuRadius;

impl WakuRadius {
    pub const SM: f32 = 6.0;
    pub const MD: f32 = 9.0;
    pub const LG: f32 = 12.0;
    pub const COMPOSER: f32 = 13.0;
    pub const PALETTE: f32 = 15.0;
    pub const DIALOG: f32 = 18.0;
    pub const TOAST: f32 = 10.0;
    pub const PILL: f32 = 9999.0;
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
    pub const SIDEBAR_HEADER_HEIGHT: f32 = 48.0;
    pub const SIDEBAR_ACTION_ROW_HEIGHT: f32 = 32.0;
    pub const SIDEBAR_GROUP_HEADER_HEIGHT: f32 = 28.0;
    pub const SIDEBAR_SESSION_CARD_HEIGHT: f32 = 51.0;
    pub const SIDEBAR_FOOTER_HEIGHT: f32 = 40.0;
    pub const DEFAULT_RIGHT_PANEL_WIDTH: f32 = 460.0;
    pub const RIGHT_PANEL_MIN_WIDTH: f32 = 280.0;
    pub const RIGHT_PANEL_MAX_WIDTH: f32 = 1000.0;
    pub const RIGHT_PANEL_HEADER_HEIGHT: f32 = 48.0;
    pub const RIGHT_PANEL_TAB_HEIGHT: f32 = 28.0;
    pub const DEFAULT_FILE_TREE_WIDTH: f32 = 184.0;
    pub const CONTENT_MAX_WIDTH: f32 = 720.0;
    pub const QUEUE_TRAY_MAX_WIDTH: f32 = 692.0;
    pub const USER_BUBBLE_MAX_WIDTH: f32 = 540.0;
    pub const COMMAND_PALETTE_MAX_WIDTH: f32 = 680.0;
    pub const COMMAND_PALETTE_MAX_HEIGHT: f32 = 480.0;
    pub const COMMIT_DIALOG_MAX_WIDTH: f32 = 420.0;
    pub const SETTINGS_CONTENT_MAX_WIDTH: f32 = 760.0;
    pub const SETTINGS_USAGE_MAX_WIDTH: f32 = 1024.0;
    pub const TOAST_TOP: f32 = 56.0;
    pub const TOAST_MAX_WIDTH: f32 = 560.0;
    pub const TITLEBAR_HEIGHT: f32 = 48.0;
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

    /// Subtle translucent green row fill for added lines in diff views.
    #[must_use]
    pub fn diff_added_bg(self) -> Color {
        if self.is_dark {
            srgba(0.18, 0.56, 0.34, 0.16)
        } else {
            srgba(0.16, 0.55, 0.31, 0.12)
        }
    }

    /// Subtle translucent red row fill for removed lines in diff views.
    #[must_use]
    pub fn diff_removed_bg(self) -> Color {
        if self.is_dark {
            srgba(0.75, 0.25, 0.22, 0.16)
        } else {
            srgba(0.76, 0.22, 0.19, 0.12)
        }
    }

    /// Subtle header wash for diff hunk markers.
    #[must_use]
    pub fn diff_hunk_bg(self) -> Color {
        if self.is_dark {
            srgba(0.23, 0.51, 0.96, 0.10)
        } else {
            srgba(0.15, 0.39, 0.92, 0.08)
        }
    }

    /// Formats a linear Rune [`Color`] as an sRGB CSS hex or `rgba(...)` string.
    #[must_use]
    pub fn color_to_css(color: Color) -> String {
        let hex = color.hex();
        let bytes = hex.as_bytes();
        let r = u8::from_str_radix(std::str::from_utf8(&bytes[1..3]).unwrap_or("00"), 16).unwrap_or(0);
        let g = u8::from_str_radix(std::str::from_utf8(&bytes[3..5]).unwrap_or("00"), 16).unwrap_or(0);
        let b = u8::from_str_radix(std::str::from_utf8(&bytes[5..7]).unwrap_or("00"), 16).unwrap_or(0);
        if color.a >= 0.999 {
            format!("#{r:02X}{g:02X}{b:02X}")
        } else {
            let trimmed = format!("{:.2}", color.a)
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string();
            format!("rgba({r}, {g}, {b}, {trimmed})")
        }
    }

    /// Returns all canonical `ui-tokens/1` color token entries for this resolved palette.
    #[must_use]
    pub fn color_tokens(self) -> Vec<(String, String)> {
        let prefix = if self.is_dark { "dark" } else { "light" };
        [
            ("bg.canvas", self.canvas),
            ("bg.surface", self.surface),
            ("bg.sidebar", self.sidebar),
            ("bg.sidebar_item", self.sidebar_item_background),
            ("bg.raised", self.raised),
            ("bg.composer", self.composer),
            ("bg.inset", self.inset),
            ("bg.terminal", self.terminal),
            ("bg.overlay", self.overlay),
            ("bg.overlay_strong", self.overlay_strong),
            ("border.default", self.border),
            ("border.strong", self.border_strong),
            ("border.sidebar", self.sidebar_border),
            ("fg.primary", self.text),
            ("fg.secondary", self.text_secondary),
            ("fg.tertiary", self.text_tertiary),
            ("fg.ghost", self.text_ghost),
            ("accent.default", self.accent),
            ("gauge.default", self.gauge),
            ("code.text", self.code_text),
            ("code.wash", self.code_wash),
            ("inverse.bg", self.inverse),
            ("inverse.fg", self.on_inverse),
            ("status.warning", self.warning),
            ("status.success", self.success),
            ("status.favorite", self.favorite),
            ("status.danger", self.danger),
        ]
        .into_iter()
        .map(|(k, c)| (format!("{prefix}.{k}"), Self::color_to_css(c)))
        .collect()
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
        kit.c.layer_04 = pal.raised;
        kit.c.bg_inverse = pal.inverse;
        kit.c.bg_accent = pal.accent;
        kit.c.text_base = pal.text;
        kit.c.text_strong = pal.text;
        kit.c.text_weak = pal.text_secondary;
        kit.c.text_muted = pal.text_secondary;
        kit.c.text_faint = pal.text_tertiary;
        kit.c.text_inverse = pal.on_inverse;
        kit.c.text_accent = pal.accent;
        kit.c.icon_base = pal.text;
        kit.c.icon_muted = pal.text_secondary;
        kit.c.icon_faint = pal.text_tertiary;
        kit.c.icon_inverse = pal.on_inverse;
        kit.c.icon_accent = pal.accent;
        kit.c.border_muted = pal.border;
        kit.c.border_base = pal.border;
        kit.c.border_weak = pal.border;
        kit.c.border_strong = pal.border_strong;
        kit.c.border_focus = pal.gauge;
        kit.c.hover = pal.overlay;
        kit.c.pressed = pal.overlay_strong;
        kit.c.surface_base = pal.surface;
        kit.c.surface_weak = pal.inset;
        kit.c.surface_hover = pal.overlay;
        kit.c.surface_raised = pal.raised;
        kit.c.surface_raised_base = pal.raised;
        kit.c.surface_raised_hover = pal.overlay_strong;
        kit.c.surface_float = pal.raised;
        kit.c.success.icon = pal.success;
        kit.c.warning.icon = pal.warning;
        kit.c.danger.icon = pal.danger;
        kit.c.info.icon = pal.gauge;
        kit
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_and_light_palettes_resolve_distinct_surfaces() {
        let dark = palette(true);
        let light = palette(false);
        assert!(dark.is_dark);
        assert!(!light.is_dark);
        assert_ne!(dark.canvas, light.canvas);
        assert_ne!(dark.accent, light.accent);
        assert_eq!(waku_kit(true).c.page, dark.canvas);
        assert_eq!(waku_kit(false).c.page, light.canvas);
    }
}

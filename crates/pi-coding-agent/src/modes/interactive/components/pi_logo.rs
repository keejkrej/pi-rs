//! Port of packages/coding-agent/src/modes/interactive/components/pi-logo.ts

use pi_tui::{Color, RgbColorValue, background_ansi, foreground_ansi, is_apple_terminal_session};

use super::super::theme::theme::theme;

const CORAL: Color = Color::Rgb(RgbColorValue {
    r: 228.0,
    g: 138.0,
    b: 122.0,
});
const BLUE: Color = Color::Rgb(RgbColorValue {
    r: 79.0,
    g: 142.0,
    b: 179.0,
});
const YELLOW: Color = Color::Rgb(RgbColorValue {
    r: 234.0,
    g: 182.0,
    b: 93.0,
});
const RESET: &str = "\x1b[0m";

/// The pi logo: 4 cells wide and 2 lines tall.
pub fn pi_logo_lines() -> [String; 2] {
    let mode = theme().get_color_mode();
    let top = format!(
        "{}{}▀{RESET}{}▀█{RESET} ",
        foreground_ansi(CORAL, mode),
        background_ansi(BLUE, mode),
        foreground_ansi(CORAL, mode)
    );
    let bottom = format!(
        "{}█▀{RESET} {}█{RESET}",
        foreground_ansi(BLUE, mode),
        foreground_ansi(YELLOW, mode)
    );
    [top, bottom]
}

/// Whether the terminal renders the half-block logo correctly.
pub fn supports_pi_logo() -> bool {
    !is_apple_terminal_session()
}

/// Text fallback for the logo: `"Pi"` with the logo's coral and yellow.
pub fn pi_wordmark() -> String {
    let mode = theme().get_color_mode();
    format!(
        "{}P{RESET}{}i{RESET}",
        foreground_ansi(CORAL, mode),
        foreground_ansi(YELLOW, mode)
    )
}

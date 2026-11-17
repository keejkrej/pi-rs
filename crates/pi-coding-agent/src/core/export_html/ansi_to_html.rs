//! ANSI escape code to HTML converter.
//!
//! Converts terminal ANSI color/style codes to HTML with inline styles.
//! Supports:
//! - Standard foreground colors (30-37) and bright variants (90-97)
//! - Standard background colors (40-47) and bright variants (100-107)
//! - 256-color palette (`38;5;N` and `48;5;N`)
//! - RGB true color (`38;2;R;G;B` and `48;2;R;G;B`)
//! - Text styles: bold (1), dim (2), italic (3), underline (4)
//! - Reset (0)
//! - OSC 8 hyperlinks with `http`, `https`, or `mailto` URLs (other CSI, OSC, and APC sequences and stray ESC are dropped)
//!
//! Port of `packages/coding-agent/src/core/export-html/ansi-to-html.ts`.

use std::sync::LazyLock;

use regex::Regex;

const ANSI_COLORS: [&str; 16] = [
    "#000000", // 0: black
    "#800000", // 1: red
    "#008000", // 2: green
    "#808000", // 3: yellow
    "#000080", // 4: blue
    "#800080", // 5: magenta
    "#008080", // 6: cyan
    "#c0c0c0", // 7: white
    "#808080", // 8: bright black
    "#ff0000", // 9: bright red
    "#00ff00", // 10: bright green
    "#ffff00", // 11: bright yellow
    "#0000ff", // 12: bright blue
    "#ff00ff", // 13: bright magenta
    "#00ffff", // 14: bright cyan
    "#ffffff", // 15: bright white
];

static ANSI_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\x1b\[([\d;]*)m|\x1b\[[0-?]*[ -/]*[@-~]|\x1b\]([^\x07\x1b]*)(?:\x07|\x1b\\)|\x1b_[^\x07\x1b]*(?:\x07|\x1b\\)",
    )
    .expect("valid ANSI regex")
});

static OSC8_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)^8;[^;]*;(.*)$").expect("valid OSC 8 regex"));

static SAFE_LINK_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:https?:|mailto:)").expect("valid safe link regex"));

/// Convert 256-color index to hex (`#rrggbb`).
fn color256_to_hex(index: usize) -> String {
    let idx = index.min(255);
    if idx < 16 {
        return ANSI_COLORS[idx].to_owned();
    }
    if idx < 232 {
        let cube_index = idx - 16;
        let r = cube_index / 36;
        let g = (cube_index % 36) / 6;
        let b = cube_index % 6;
        let to_component = |n: usize| if n == 0 { 0 } else { 55 + n * 40 };
        return format!("#{:02x}{:02x}{:02x}", to_component(r), to_component(g), to_component(b));
    }
    let gray = 8 + (idx - 232) * 10;
    format!("#{gray:02x}{gray:02x}{gray:02x}")
}

/// Escape HTML special characters (`&`, `<`, `>`, `"`, `'`).
fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#039;"),
            _ => out.push(ch),
        }
    }
    out
}

#[derive(Clone, Default)]
struct TextStyle {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

impl TextStyle {
    fn reset(&mut self) {
        self.fg = None;
        self.bg = None;
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
    }

    fn has_style(&self) -> bool {
        self.fg.is_some() || self.bg.is_some() || self.bold || self.dim || self.italic || self.underline
    }

    fn to_inline_css(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(fg) = &self.fg {
            parts.push(format!("color:{fg}"));
        }
        if let Some(bg) = &self.bg {
            parts.push(format!("background-color:{bg}"));
        }
        if self.bold {
            parts.push("font-weight:bold".to_owned());
        }
        if self.dim {
            parts.push("opacity:0.6".to_owned());
        }
        if self.italic {
            parts.push("font-style:italic".to_owned());
        }
        if self.underline {
            parts.push("text-decoration:underline".to_owned());
        }
        parts.join(";")
    }
}

/// Parse ANSI SGR (Select Graphic Rendition) codes and update style.
fn apply_sgr_code(params: &[usize], style: &mut TextStyle) {
    let mut i = 0;
    while i < params.len() {
        let code = params[i];
        match code {
            0 => style.reset(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            30..=37 => {
                style.fg = Some(ANSI_COLORS[code - 30].to_owned());
            }
            38 => {
                if params.get(i + 1) == Some(&5) && i + 2 < params.len() {
                    style.fg = Some(color256_to_hex(params[i + 2]));
                    i += 2;
                } else if params.get(i + 1) == Some(&2) && i + 4 < params.len() {
                    let r = params[i + 2];
                    let g = params[i + 3];
                    let b = params[i + 4];
                    style.fg = Some(format!("rgb({r},{g},{b})"));
                    i += 4;
                }
            }
            39 => style.fg = None,
            40..=47 => {
                style.bg = Some(ANSI_COLORS[code - 40].to_owned());
            }
            48 => {
                if params.get(i + 1) == Some(&5) && i + 2 < params.len() {
                    style.bg = Some(color256_to_hex(params[i + 2]));
                    i += 2;
                } else if params.get(i + 1) == Some(&2) && i + 4 < params.len() {
                    let r = params[i + 2];
                    let g = params[i + 3];
                    let b = params[i + 4];
                    style.bg = Some(format!("rgb({r},{g},{b})"));
                    i += 4;
                }
            }
            49 => style.bg = None,
            90..=97 => {
                style.fg = Some(ANSI_COLORS[code - 90 + 8].to_owned());
            }
            100..=107 => {
                style.bg = Some(ANSI_COLORS[code - 100 + 8].to_owned());
            }
            _ => {}
        }
        i += 1;
    }
}

/// Remove all escape sequences matched by the converter, plus stray ESC characters.
pub fn strip_ansi_escapes(text: &str) -> String {
    ANSI_REGEX.replace_all(text, "").replace('\x1b', "")
}

/// Convert ANSI-escaped text to HTML with inline styles.
pub fn ansi_to_html(text: &str) -> String {
    let mut style = TextStyle::default();
    let mut result = String::new();
    let mut last_index = 0usize;
    let mut in_span = false;
    let mut in_link = false;

    let close_span = |out: &mut String, span_open: &mut bool| {
        if *span_open {
            out.push_str("</span>");
            *span_open = false;
        }
    };
    let open_span = |out: &mut String, span_open: &mut bool, st: &TextStyle| {
        if st.has_style() {
            out.push_str("<span style=\"");
            out.push_str(&st.to_inline_css());
            out.push_str("\">");
            *span_open = true;
        }
    };

    for caps in ANSI_REGEX.captures_iter(text) {
        let Some(full) = caps.get(0) else {
            continue;
        };
        if full.start() > last_index {
            let before = text[last_index..full.start()].replace('\x1b', "");
            result.push_str(&escape_html(&before));
        }

        if let Some(sgr_match) = caps.get(1) {
            let param_str = sgr_match.as_str();
            let params: Vec<usize> = if param_str.is_empty() {
                vec![0]
            } else {
                param_str.split(';').map(|p| p.parse::<usize>().unwrap_or(0)).collect()
            };
            close_span(&mut result, &mut in_span);
            apply_sgr_code(&params, &mut style);
            open_span(&mut result, &mut in_span, &style);
        } else if let Some(osc_match) = caps.get(2)
            && let Some(osc8_caps) = OSC8_REGEX.captures(osc_match.as_str())
            && let Some(raw_url) = osc8_caps.get(1)
        {
            let trimmed = pi_js::str16::trim(raw_url.as_str());
            let clean_url: String = trimmed.chars().filter(|&c| c > '\x1f' && c != '\x7f').collect();

            close_span(&mut result, &mut in_span);
            if in_link {
                result.push_str("</a>");
                in_link = false;
            }
            if !clean_url.is_empty() && SAFE_LINK_REGEX.is_match(&clean_url) {
                result.push_str("<a href=\"");
                result.push_str(&escape_html(&clean_url));
                result.push_str("\">");
                in_link = true;
            }
            open_span(&mut result, &mut in_span, &style);
        }

        last_index = full.end();
    }

    if last_index < text.len() {
        let remaining = text[last_index..].replace('\x1b', "");
        result.push_str(&escape_html(&remaining));
    }

    close_span(&mut result, &mut in_span);
    if in_link {
        result.push_str("</a>");
    }

    result
}

/// Convert array of ANSI-escaped lines to HTML.
/// Each line is wrapped in `<div class="ansi-line">...</div>`.
pub fn ansi_lines_to_html<S: AsRef<str>>(lines: &[S]) -> String {
    let mut out = String::new();
    for line in lines {
        let converted = ansi_to_html(line.as_ref());
        out.push_str("<div class=\"ansi-line\">");
        if converted.is_empty() {
            out.push_str("&nbsp;");
        } else {
            out.push_str(&converted);
        }
        out.push_str("</div>");
    }
    out
}

//! Port of vendor chalk@6.0.0 (+ supports-color) (PORTING.md §2.6).

use std::io::IsTerminal;
use std::sync::LazyLock;

use indexmap::IndexSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorSupport {
    pub level: u8,
    pub has_basic: bool,
    pub has256: bool,
    pub has16m: bool,
}

fn translate_level(level: u8) -> Option<ColorSupport> {
    if level == 0 {
        return None;
    }
    Some(ColorSupport {
        level,
        has_basic: true,
        has256: level >= 2,
        has16m: level >= 3,
    })
}

fn has_flag(flag: &str) -> bool {
    let prefix = if flag.starts_with('-') {
        ""
    } else if flag.len() == 1 {
        "-"
    } else {
        "--"
    };
    let needle = format!("{prefix}{flag}");
    let args: Vec<String> = std::env::args().collect();
    let terminator = args.iter().position(|a| a == "--");
    if let Some(pos) = args.iter().position(|a| a == &needle) {
        terminator.is_none_or(|t| pos < t)
    } else {
        false
    }
}

fn detect_level(have_stream: bool, stream_is_tty: bool) -> u8 {
    let env_force_color = pi_js::env::var("FORCE_COLOR").map(|v| {
        if v == "false" {
            return (Some(0u8), false);
        }
        if v == "true" || v.is_empty() {
            return (Some(1u8), false);
        }
        if v.bytes().all(|b| b.is_ascii_digit()) {
            let parsed = v.parse::<u64>().unwrap_or(3).min(3) as u8;
            return (Some(parsed), true);
        }
        (None, false)
    });

    let mut flag_force_color: Option<u8> =
        if has_flag("no-color") || has_flag("no-colors") || has_flag("color=false") || has_flag("color=never") {
            Some(0)
        } else if has_flag("color") || has_flag("colors") || has_flag("color=true") || has_flag("color=always") {
            Some(1)
        } else {
            None
        };

    let mut is_numeric_force = false;
    if let Some((Some(val), numeric)) = env_force_color {
        flag_force_color = Some(val);
        is_numeric_force = numeric;
    }

    if flag_force_color == Some(0) {
        return 0;
    }
    if has_flag("color=16m") || has_flag("color=full") || has_flag("color=truecolor") {
        return 3;
    }
    if has_flag("color=256") {
        return 2;
    }
    if let Some(fc) = flag_force_color
        && is_numeric_force
    {
        return fc;
    }

    if pi_js::env::var("TF_BUILD").is_some() && pi_js::env::var("AGENT_NAME").is_some() {
        return 1;
    }
    if have_stream && !stream_is_tty && flag_force_color.is_none() {
        return 0;
    }

    let min = flag_force_color.unwrap_or(0);
    let term = pi_js::env::var("TERM").unwrap_or_default();
    if term == "dumb" {
        return min;
    }

    if pi_js::env::platform() == "win32" {
        let rel = pi_js::env::os_release();
        let parts: Vec<&str> = rel.split('.').collect();
        let major = parts.first().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        let build = parts.get(2).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        if major >= 10 && build >= 10_586 {
            return if build >= 14_931 { 3 } else { 2 };
        }
        return 1;
    }

    if pi_js::env::var("CI").is_some() {
        for key in ["GITHUB_ACTIONS", "GITEA_ACTIONS", "CIRCLECI"] {
            if pi_js::env::var(key).is_some() {
                return 3;
            }
        }
        for key in ["TRAVIS", "APPVEYOR", "GITLAB_CI", "BUILDKITE", "DRONE"] {
            if pi_js::env::var(key).is_some() {
                return 1;
            }
        }
        if pi_js::env::var("CI_NAME").as_deref() == Some("codeship") {
            return 1;
        }
        return min;
    }

    if let Some(tc) = pi_js::env::var("TEAMCITY_VERSION") {
        let ok = tc.starts_with("9.")
            || tc
                .split('.')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                .is_some_and(|n| n >= 10);
        return if ok { 1 } else { 0 };
    }

    if pi_js::env::var("COLORTERM").as_deref() == Some("truecolor") {
        return 3;
    }
    if matches!(term.as_str(), "xterm-kitty" | "xterm-ghostty" | "wezterm") {
        return 3;
    }
    if let Some(tp) = pi_js::env::var("TERM_PROGRAM") {
        let ver = pi_js::env::var("TERM_PROGRAM_VERSION")
            .unwrap_or_default()
            .split('.')
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        match tp.as_str() {
            "iTerm.app" => return if ver >= 3 { 3 } else { 2 },
            "Apple_Terminal" => return 2,
            _ => {}
        }
    }

    let term_lower = term.to_ascii_lowercase();
    if term_lower.ends_with("-256") || term_lower.ends_with("-256color") {
        return 2;
    }
    if ["screen", "xterm", "vt100", "vt220", "rxvt"]
        .iter()
        .any(|p| term_lower.starts_with(p))
        || ["color", "ansi", "cygwin", "linux"]
            .iter()
            .any(|k| term_lower.contains(k))
    {
        return 1;
    }
    if pi_js::env::var("COLORTERM").is_some() {
        return 1;
    }

    min
}

pub fn supports_color() -> Option<ColorSupport> {
    translate_level(detect_level(true, std::io::stdout().is_terminal()))
}

pub fn supports_color_stderr() -> Option<ColorSupport> {
    translate_level(detect_level(true, std::io::stderr().is_terminal()))
}

#[derive(Clone, Debug)]
struct Styler {
    open: String,
    close: String,
    open_all: String,
    close_all: String,
    parent: Option<Box<Styler>>,
}

#[derive(Clone, Debug)]
pub struct Chalk {
    level: u8,
    styler: Option<Styler>,
    is_empty: bool,
}

impl Default for Chalk {
    fn default() -> Self {
        Self::new(None)
    }
}

fn string_replace_all(s: &str, sub: &str, postfix: &str) -> String {
    if sub.is_empty() || !s.contains(sub) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + postfix.len());
    let mut start = 0;
    while let Some(rel) = s[start..].find(sub) {
        let idx = start + rel;
        out.push_str(&s[start..idx]);
        out.push_str(sub);
        out.push_str(postfix);
        start = idx + sub.len();
    }
    out.push_str(&s[start..]);
    out
}

fn string_encase_crlf(s: &str, prefix: &str, postfix: &str) -> String {
    if !s.contains('\n') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut start = 0;
    while let Some(rel) = s[start..].find('\n') {
        let lf = start + rel;
        let has_cr = lf > start && s.as_bytes()[lf - 1] == b'\r';
        if has_cr {
            out.push_str(&s[start..lf - 1]);
            out.push_str(prefix);
            out.push_str("\r\n");
            out.push_str(postfix);
        } else {
            out.push_str(&s[start..lf]);
            out.push_str(prefix);
            out.push('\n');
            out.push_str(postfix);
        }
        start = lf + 1;
    }
    out.push_str(&s[start..]);
    out
}

fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return (((f64::from(r) - 8.0) / 247.0 * 24.0).round() as u8) + 232;
    }
    16 + 36 * ((f64::from(r) / 255.0 * 5.0).round() as u8)
        + 6 * ((f64::from(g) / 255.0 * 5.0).round() as u8)
        + ((f64::from(b) / 255.0 * 5.0).round() as u8)
}

fn ansi256_to_ansi(code: u8) -> u8 {
    if code < 8 {
        return 30 + code;
    }
    if code < 16 {
        return 90 + (code - 8);
    }
    let (r, g, b) = if code >= 232 {
        let gray = (f64::from(code - 232) * 10.0 + 8.0) / 255.0;
        (gray, gray, gray)
    } else {
        let c = code - 16;
        let rem = c % 36;
        (
            f64::from(c / 36) / 5.0,
            f64::from(rem / 6) / 5.0,
            f64::from(rem % 6) / 5.0,
        )
    };
    let val = r.max(g).max(b) * 2.0;
    if val == 0.0 {
        return 30;
    }
    let mut res = 30 + ((b.round() as u8) << 2 | (g.round() as u8) << 1 | (r.round() as u8));
    if (val - 2.0).abs() < f64::EPSILON {
        res += 60;
    }
    res
}

fn hex_to_rgb(hex: &str) -> (u8, u8, u8) {
    let trimmed = hex.trim().trim_start_matches('#');
    let digits: String = trimmed.chars().take_while(char::is_ascii_hexdigit).collect();
    let normalized = if digits.len() >= 6 {
        digits[..6].to_owned()
    } else if digits.len() >= 3 {
        digits[..3].chars().flat_map(|c| [c, c]).collect()
    } else {
        return (0, 0, 0);
    };
    let int = u32::from_str_radix(&normalized, 16).unwrap_or(0);
    (
        ((int >> 16) & 0xff) as u8,
        ((int >> 8) & 0xff) as u8,
        (int & 0xff) as u8,
    )
}

impl Chalk {
    pub fn new(level: Option<u8>) -> Self {
        let lvl = level.unwrap_or_else(|| detect_level(true, std::io::stdout().is_terminal()));
        Self {
            level: lvl.min(3),
            styler: None,
            is_empty: false,
        }
    }

    pub fn level(&self) -> u8 {
        self.level
    }

    pub fn set_level(&mut self, level: u8) {
        self.level = level.min(3);
    }

    fn chain(&self, open: impl Into<String>, close: impl Into<String>) -> Self {
        let open = open.into();
        let close = close.into();
        let (open_all, close_all) = match &self.styler {
            None => (open.clone(), close.clone()),
            Some(parent) => (
                format!("{}{}", parent.open_all, open),
                format!("{}{}", close, parent.close_all),
            ),
        };
        Self {
            level: self.level,
            styler: Some(Styler {
                open,
                close,
                open_all,
                close_all,
                parent: self.styler.clone().map(Box::new),
            }),
            is_empty: self.is_empty,
        }
    }

    pub fn apply(&self, text: &str) -> String {
        if self.level == 0 || text.is_empty() {
            return if self.is_empty { String::new() } else { text.to_owned() };
        }
        let Some(ref top) = self.styler else {
            return text.to_owned();
        };
        let mut s = text.to_owned();
        if s.contains('\u{001B}') {
            let mut cur = Some(top);
            while let Some(st) = cur {
                s = string_replace_all(&s, &st.close, &st.open);
                cur = st.parent.as_deref();
            }
        }
        if s.contains('\n') {
            s = string_encase_crlf(&s, &top.close_all, &top.open_all);
        }
        format!("{}{}{}", top.open_all, s, top.close_all)
    }

    pub fn visible(&self) -> Self {
        Self {
            level: self.level,
            styler: self.styler.clone(),
            is_empty: true,
        }
    }

    pub fn reset(&self) -> Self {
        self.chain("\u{001B}[0m", "\u{001B}[0m")
    }
    pub fn bold(&self) -> Self {
        self.chain("\u{001B}[1m", "\u{001B}[22m")
    }
    pub fn dim(&self) -> Self {
        self.chain("\u{001B}[2m", "\u{001B}[22m")
    }
    pub fn italic(&self) -> Self {
        self.chain("\u{001B}[3m", "\u{001B}[23m")
    }
    pub fn underline(&self) -> Self {
        self.chain("\u{001B}[4m", "\u{001B}[24m")
    }
    pub fn overline(&self) -> Self {
        self.chain("\u{001B}[53m", "\u{001B}[55m")
    }
    pub fn inverse(&self) -> Self {
        self.chain("\u{001B}[7m", "\u{001B}[27m")
    }
    pub fn hidden(&self) -> Self {
        self.chain("\u{001B}[8m", "\u{001B}[28m")
    }
    pub fn strikethrough(&self) -> Self {
        self.chain("\u{001B}[9m", "\u{001B}[29m")
    }

    pub fn black(&self) -> Self {
        self.chain("\u{001B}[30m", "\u{001B}[39m")
    }
    pub fn red(&self) -> Self {
        self.chain("\u{001B}[31m", "\u{001B}[39m")
    }
    pub fn green(&self) -> Self {
        self.chain("\u{001B}[32m", "\u{001B}[39m")
    }
    pub fn yellow(&self) -> Self {
        self.chain("\u{001B}[33m", "\u{001B}[39m")
    }
    pub fn blue(&self) -> Self {
        self.chain("\u{001B}[34m", "\u{001B}[39m")
    }
    pub fn magenta(&self) -> Self {
        self.chain("\u{001B}[35m", "\u{001B}[39m")
    }
    pub fn cyan(&self) -> Self {
        self.chain("\u{001B}[36m", "\u{001B}[39m")
    }
    pub fn white(&self) -> Self {
        self.chain("\u{001B}[37m", "\u{001B}[39m")
    }
    pub fn black_bright(&self) -> Self {
        self.chain("\u{001B}[90m", "\u{001B}[39m")
    }
    pub fn gray(&self) -> Self {
        self.black_bright()
    }
    pub fn grey(&self) -> Self {
        self.black_bright()
    }
    pub fn red_bright(&self) -> Self {
        self.chain("\u{001B}[91m", "\u{001B}[39m")
    }
    pub fn green_bright(&self) -> Self {
        self.chain("\u{001B}[92m", "\u{001B}[39m")
    }
    pub fn yellow_bright(&self) -> Self {
        self.chain("\u{001B}[93m", "\u{001B}[39m")
    }
    pub fn blue_bright(&self) -> Self {
        self.chain("\u{001B}[94m", "\u{001B}[39m")
    }
    pub fn magenta_bright(&self) -> Self {
        self.chain("\u{001B}[95m", "\u{001B}[39m")
    }
    pub fn cyan_bright(&self) -> Self {
        self.chain("\u{001B}[96m", "\u{001B}[39m")
    }
    pub fn white_bright(&self) -> Self {
        self.chain("\u{001B}[97m", "\u{001B}[39m")
    }

    pub fn bg_black(&self) -> Self {
        self.chain("\u{001B}[40m", "\u{001B}[49m")
    }
    pub fn bg_red(&self) -> Self {
        self.chain("\u{001B}[41m", "\u{001B}[49m")
    }
    pub fn bg_green(&self) -> Self {
        self.chain("\u{001B}[42m", "\u{001B}[49m")
    }
    pub fn bg_yellow(&self) -> Self {
        self.chain("\u{001B}[43m", "\u{001B}[49m")
    }
    pub fn bg_blue(&self) -> Self {
        self.chain("\u{001B}[44m", "\u{001B}[49m")
    }
    pub fn bg_magenta(&self) -> Self {
        self.chain("\u{001B}[45m", "\u{001B}[49m")
    }
    pub fn bg_cyan(&self) -> Self {
        self.chain("\u{001B}[46m", "\u{001B}[49m")
    }
    pub fn bg_white(&self) -> Self {
        self.chain("\u{001B}[47m", "\u{001B}[49m")
    }
    pub fn bg_black_bright(&self) -> Self {
        self.chain("\u{001B}[100m", "\u{001B}[49m")
    }
    pub fn bg_gray(&self) -> Self {
        self.bg_black_bright()
    }
    pub fn bg_grey(&self) -> Self {
        self.bg_black_bright()
    }
    pub fn bg_red_bright(&self) -> Self {
        self.chain("\u{001B}[101m", "\u{001B}[49m")
    }
    pub fn bg_green_bright(&self) -> Self {
        self.chain("\u{001B}[102m", "\u{001B}[49m")
    }
    pub fn bg_yellow_bright(&self) -> Self {
        self.chain("\u{001B}[103m", "\u{001B}[49m")
    }
    pub fn bg_blue_bright(&self) -> Self {
        self.chain("\u{001B}[104m", "\u{001B}[49m")
    }
    pub fn bg_magenta_bright(&self) -> Self {
        self.chain("\u{001B}[105m", "\u{001B}[49m")
    }
    pub fn bg_cyan_bright(&self) -> Self {
        self.chain("\u{001B}[106m", "\u{001B}[49m")
    }
    pub fn bg_white_bright(&self) -> Self {
        self.chain("\u{001B}[107m", "\u{001B}[49m")
    }

    pub fn rgb(&self, r: u8, g: u8, b: u8) -> Self {
        let open = match self.level {
            0 | 1 => format!("\u{001B}[{}m", ansi256_to_ansi(rgb_to_ansi256(r, g, b))),
            2 => format!("\u{001B}[38;5;{}m", rgb_to_ansi256(r, g, b)),
            _ => format!("\u{001B}[38;2;{r};{g};{b}m"),
        };
        self.chain(open, "\u{001B}[39m")
    }

    pub fn hex(&self, hex: &str) -> Self {
        let (r, g, b) = hex_to_rgb(hex);
        self.rgb(r, g, b)
    }

    pub fn ansi256(&self, code: u8) -> Self {
        let open = if self.level <= 1 {
            format!("\u{001B}[{}m", ansi256_to_ansi(code))
        } else {
            format!("\u{001B}[38;5;{code}m")
        };
        self.chain(open, "\u{001B}[39m")
    }

    pub fn bg_rgb(&self, r: u8, g: u8, b: u8) -> Self {
        let open = match self.level {
            0 | 1 => format!("\u{001B}[{}m", ansi256_to_ansi(rgb_to_ansi256(r, g, b)) + 10),
            2 => format!("\u{001B}[48;5;{}m", rgb_to_ansi256(r, g, b)),
            _ => format!("\u{001B}[48;2;{r};{g};{b}m"),
        };
        self.chain(open, "\u{001B}[49m")
    }
}

pub fn chalk() -> Chalk {
    Chalk::new(None)
}

pub fn chalk_stderr() -> Chalk {
    Chalk::new(Some(detect_level(true, std::io::stderr().is_terminal())))
}

pub fn bold(text: &str) -> String {
    chalk().bold().apply(text)
}
pub fn dim(text: &str) -> String {
    chalk().dim().apply(text)
}
pub fn italic(text: &str) -> String {
    chalk().italic().apply(text)
}
pub fn underline(text: &str) -> String {
    chalk().underline().apply(text)
}
pub fn red(text: &str) -> String {
    chalk().red().apply(text)
}
pub fn green(text: &str) -> String {
    chalk().green().apply(text)
}
pub fn yellow(text: &str) -> String {
    chalk().yellow().apply(text)
}
pub fn blue(text: &str) -> String {
    chalk().blue().apply(text)
}
pub fn magenta(text: &str) -> String {
    chalk().magenta().apply(text)
}
pub fn cyan(text: &str) -> String {
    chalk().cyan().apply(text)
}
pub fn white(text: &str) -> String {
    chalk().white().apply(text)
}
pub fn gray(text: &str) -> String {
    chalk().gray().apply(text)
}

static _UNUSED_CHECK: LazyLock<IndexSet<String>> = LazyLock::new(IndexSet::new);

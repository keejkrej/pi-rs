//! Port of packages/coding-agent/src/utils/ansi.ts

use std::sync::LazyLock;

use fancy_regex::Regex;

use pi_js::{Error, Result};

// Valid string terminator sequences are BEL, ESC\, and 0x9c
const ST: &str = r"(?:\x07|\x1b\\|\u{009c})";
const OSC_START: &str = r"\x1b\]";
// CSI and related: ESC/C1, optional intermediates, optional params (supports ; and :), then final byte.
// A private prefix (<, =, >) is accepted only right after a real CSI introducer (ESC [ or 0x9B), so
// ESC[<35;59;1M (SGR mouse report) is stripped whole. After a bare ESC these bytes stay final bytes:
// ESC = and ESC > (keypad modes) must not swallow the digits of the text that follows.
const CSI_START: &str = r"(?:(?:\x1b\[|\u{009b})[<=>]|[\x1b\u{009b}][\[\]()#;?]*)(?:\d{1,4}(?:[;:]\d{0,4})*)?";
const CSI_FINAL: &str = r"[\dA-PR-TZcf-nq-uy=><~]";

// Longest unfinished sequence held back while streaming. Longer ones are processed as-is.
const MAX_PENDING_ANSI_LENGTH: usize = 256;

static ANSI_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(r"(?:{OSC_START}[\s\S]*?{ST})|{CSI_START}{CSI_FINAL}");
    Regex::new(&pattern).expect("valid ansi regex")
});

static UNFINISHED_ANSI_AT_END_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(r"(?:{OSC_START}(?:[^\x07\u{{009c}}\x1b]|\x1b(?!\\))*|{CSI_START})$");
    Regex::new(&pattern).expect("valid unfinished ansi regex")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompleteAnsiSuffix {
    pub complete: String,
    pub pending: String,
}

/// Split streamed text into a part that is safe to pass to `strip_ansi` now and a trailing
/// unfinished escape sequence that should be prepended to the next chunk.
pub fn split_incomplete_ansi_suffix(value: &str) -> IncompleteAnsiSuffix {
    if !value.contains('\u{001B}') && !value.contains('\u{009B}') {
        return IncompleteAnsiSuffix {
            complete: value.to_owned(),
            pending: String::new(),
        };
    }
    let mut window_start = value.len().saturating_sub(MAX_PENDING_ANSI_LENGTH);
    while window_start < value.len() && !value.is_char_boundary(window_start) {
        window_start += 1;
    }
    let tail = &value[window_start..];
    match UNFINISHED_ANSI_AT_END_REGEX.find(tail) {
        Ok(Some(m)) => {
            let split_at = window_start + m.start();
            IncompleteAnsiSuffix {
                complete: value[..split_at].to_owned(),
                pending: value[split_at..].to_owned(),
            }
        }
        _ => IncompleteAnsiSuffix {
            complete: value.to_owned(),
            pending: String::new(),
        },
    }
}

pub fn strip_ansi(value: &str) -> String {
    // Fast path: ANSI codes require ESC (7-bit) or CSI (8-bit) introducer
    if !value.contains('\u{001B}') && !value.contains('\u{009B}') {
        return value.to_owned();
    }

    ANSI_REGEX.replace_all(value, "").into_owned()
}

pub fn try_strip_ansi(value: &serde_json::Value) -> Result<String> {
    let Some(s) = value.as_str() else {
        let got = match value {
            serde_json::Value::Null => "object",
            serde_json::Value::Bool(_) => "boolean",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => "object",
        };
        return Err(Error::js("TypeError", format!("Expected a `string`, got `{got}`")));
    };
    Ok(strip_ansi(s))
}

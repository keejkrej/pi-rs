//! Port of packages/coding-agent/src/utils/ansi.ts

use std::sync::LazyLock;

use fancy_regex::Regex;

use pi_js::{Error, Result};

fn build_ansi_regex(only_first: bool) -> Regex {
    let _ = only_first;
    // Valid string terminator sequences are BEL, ESC\, and 0x9c
    let st = r"(?:\x07|\x1b\\|\u{009c})";
    // OSC sequences only: ESC ] ... ST (non-greedy until the first ST)
    let osc = format!(r"(?:\x1b\][\s\S]*?{st})");
    // CSI and related: ESC/C1, optional intermediates, optional params (supports ; and :) then final byte
    let csi = r"[\x1b\u{009b}][[\]()#;?]*(?:\d{1,4}(?:[;:]\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~]";
    let pattern = format!("{osc}|{csi}");
    Regex::new(&pattern).expect("valid ansi regex")
}

static ANSI_REGEX: LazyLock<Regex> = LazyLock::new(|| build_ansi_regex(false));

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

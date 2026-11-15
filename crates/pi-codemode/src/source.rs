//! Port of packages/codemode/src/source.ts

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Codemode source format: JavaScript, optionally preceded by one options line.
///
/// ```js
/// // @options: {"max_output_tokens": 2000, "timeout_ms": 30000}
/// const text = await tools.read({ path: "package.json" });
/// text(JSON.parse(text).name);
/// ```
pub const CODEMODE_OPTIONS_PREFIX: &str = "// @options:";

const SUPPORTED_FIELDS: &[&str] = &["max_output_tokens", "timeout_ms"];
const SUPPORTED_FIELDS_TEXT: &str = "`max_output_tokens` and `timeout_ms`";
/// Largest delay `setTimeout` supports, which bounds `timeout_ms`.
const MAX_TIMEOUT_MS: i64 = 2_147_483_647;

/// Lark grammar for providers with grammar-constrained tool input. It only fixes the shape of the
/// options line; the options JSON and the code are checked by [`parse_codemode_source`].
pub const CODEMODE_SOURCE_GRAMMAR: &str = r#"
start: options_source | plain_source
options_source: OPTIONS_LINE NEWLINE SOURCE
plain_source: SOURCE

OPTIONS_LINE: /[ \t]*\/\/ @options:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeSourceOptions {
    /// Token budget for the script's output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    /// Hard deadline for the whole script in milliseconds, including tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedCodemodeSource {
    /// The script with the options line replaced by an empty line, so line numbers are unchanged.
    pub code: String,
    pub options: CodemodeSourceOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CodemodeSourceError {
    pub message: String,
}

impl CodemodeSourceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

fn source_error(message: impl Into<String>) -> pi_js::Error {
    pi_js::Error::typed("CodemodeSourceError", CodemodeSourceError::new(message))
}

fn as_non_negative_safe_integer(value: &Value) -> Option<i64> {
    let n = value.as_number()?;
    if let Some(u) = n.as_u64() {
        return (u <= pi_js::num::MAX_SAFE_INTEGER as u64).then_some(u as i64);
    }
    if let Some(i) = n.as_i64() {
        return (0..=pi_js::num::MAX_SAFE_INTEGER).contains(&i).then_some(i);
    }
    if let Some(f) = n.as_f64()
        && f.is_finite()
        && f >= 0.0
        && f.fract() == 0.0
        && f <= pi_js::num::MAX_SAFE_INTEGER as f64
    {
        return Some(f as i64);
    }
    None
}

fn is_safe_integer(value: &Value) -> bool {
    as_non_negative_safe_integer(value).is_some()
}

fn parse_options(directive: &str) -> pi_js::Result<CodemodeSourceOptions> {
    if directive.is_empty() {
        return Err(source_error(format!(
            "@options must be a JSON object with supported fields {SUPPORTED_FIELDS_TEXT}"
        )));
    }
    let value = pi_js::json::parse(directive).map_err(|err| {
        source_error(format!(
            "@options must be valid JSON with supported fields {SUPPORTED_FIELDS_TEXT}: {}",
            err.message()
        ))
    })?;
    let Some(fields) = value.as_object() else {
        return Err(source_error(format!(
            "@options must be a JSON object with supported fields {SUPPORTED_FIELDS_TEXT}"
        )));
    };
    for key in fields.keys() {
        if !SUPPORTED_FIELDS.contains(&key.as_str()) {
            return Err(source_error(format!(
                "@options only supports {SUPPORTED_FIELDS_TEXT}; got `{key}`"
            )));
        }
    }

    let mut options = CodemodeSourceOptions::default();
    if let Some(raw) = fields.get("max_output_tokens") {
        let Some(val) = as_non_negative_safe_integer(raw) else {
            return Err(source_error(
                "@options field `max_output_tokens` must be a non-negative safe integer",
            ));
        };
        let _ = is_safe_integer(raw);
        options.max_output_tokens = Some(val);
    }
    if let Some(raw) = fields.get("timeout_ms") {
        let Some(val) = as_non_negative_safe_integer(raw) else {
            return Err(source_error(format!(
                "@options field `timeout_ms` must be a positive integer up to {MAX_TIMEOUT_MS}"
            )));
        };
        if val == 0 || val > MAX_TIMEOUT_MS {
            return Err(source_error(format!(
                "@options field `timeout_ms` must be a positive integer up to {MAX_TIMEOUT_MS}"
            )));
        }
        options.timeout_ms = Some(val);
    }
    Ok(options)
}

/// Split an optional first-line `// @options: {...}` from the script. Throws
/// [`CodemodeSourceError`] for empty input and invalid options.
pub fn parse_codemode_source(input: &str) -> pi_js::Result<ParsedCodemodeSource> {
    if pi_js::str16::trim(input).is_empty() {
        return Err(source_error(
            r#"Expected JavaScript source text (non-empty). Provide JS only, optionally with a first line `// @options: {"max_output_tokens": 1000}`."#,
        ));
    }
    let newline = input.find('\n');
    let first_line = match newline {
        Some(idx) => &input[..idx],
        None => input,
    };
    let first_line = first_line.strip_suffix('\r').unwrap_or(first_line);
    let trimmed = pi_js::str16::trim_start(first_line);
    if !trimmed.starts_with(CODEMODE_OPTIONS_PREFIX) {
        return Ok(ParsedCodemodeSource {
            code: input.to_string(),
            options: CodemodeSourceOptions::default(),
        });
    }
    let code = match newline {
        Some(idx) => &input[idx..],
        None => "",
    };
    if pi_js::str16::trim(code).is_empty() {
        return Err(source_error(
            "The @options line must be followed by JavaScript source on subsequent lines",
        ));
    }
    let directive = pi_js::str16::trim(&trimmed[CODEMODE_OPTIONS_PREFIX.len()..]);
    Ok(ParsedCodemodeSource {
        code: code.to_string(),
        options: parse_options(directive)?,
    })
}

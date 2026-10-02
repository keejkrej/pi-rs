//! Port of packages/codemode/src/source.ts

#![allow(dead_code, unused_variables)]

use serde::{Deserialize, Serialize};

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

fn is_safe_integer(value: &serde_json::Value) -> bool {
    todo!("port: is_safe_integer")
}

fn parse_options(directive: &str) -> pi_js::Result<CodemodeSourceOptions> {
    todo!("port: parse_options")
}

/// Split an optional first-line `// @options: {...}` from the script. Throws
/// [`CodemodeSourceError`] for empty input and invalid options.
pub fn parse_codemode_source(input: &str) -> pi_js::Result<ParsedCodemodeSource> {
    todo!("port: parse_codemode_source")
}

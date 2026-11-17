//! Port of packages/coding-agent/src/utils/json.ts

use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;

static LINE_COMMENT_RE: LazyLock<FancyRegex> =
    LazyLock::new(|| FancyRegex::new(r#""(?:\\.|[^"\\])*"|//[^\n]*"#).expect("valid regex"));
static TRAILING_COMMA_RE: LazyLock<FancyRegex> =
    LazyLock::new(|| FancyRegex::new(r#""(?:\\.|[^"\\])*"|,(\s*[}\]])"#).expect("valid regex"));

/// Strip `//` line comments and trailing commas from JSON, leaving string literals untouched.
pub fn strip_json_comments(input: &str) -> String {
    let mut no_comments = String::with_capacity(input.len());
    let mut last = 0;
    for caps in LINE_COMMENT_RE.captures_iter(input).flatten() {
        if let Some(m) = caps.get(0) {
            no_comments.push_str(&input[last..m.start()]);
            if m.as_str().starts_with('"') {
                no_comments.push_str(m.as_str());
            }
            last = m.end();
        }
    }
    no_comments.push_str(&input[last..]);

    let mut out = String::with_capacity(no_comments.len());
    let mut last2 = 0;
    for caps in TRAILING_COMMA_RE.captures_iter(&no_comments).flatten() {
        if let Some(m) = caps.get(0) {
            out.push_str(&no_comments[last2..m.start()]);
            if let Some(tail) = caps.get(1) {
                out.push_str(tail.as_str());
            } else if m.as_str().starts_with('"') {
                out.push_str(m.as_str());
            }
            last2 = m.end();
        }
    }
    out.push_str(&no_comments[last2..]);
    out
}

//! Port of packages/coding-agent/src/utils/text.ts

/// Split a leading UTF-8 byte order mark from decoded text.
pub fn split_bom(content: &str) -> (&'static str, &str) {
    if let Some(rest) = content.strip_prefix('\u{FEFF}') {
        ("\u{FEFF}", rest)
    } else {
        ("", content)
    }
}

/// Remove a leading UTF-8 byte order mark from decoded text.
pub fn strip_bom(content: &str) -> &str {
    split_bom(content).1
}

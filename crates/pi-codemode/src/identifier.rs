//! Port of packages/codemode/src/identifier.ts

/// The identifier a script uses for a tool: characters that are not valid in a JavaScript
/// identifier become `_`. `mcp__docs__search` stays as is, `my-tool` becomes `my_tool`.
pub fn to_codemode_identifier(name: &str) -> String {
    let mut identifier = String::with_capacity(name.len());
    for ch in name.chars() {
        let valid = if identifier.is_empty() {
            ch.is_ascii_alphabetic() || ch == '_' || ch == '$'
        } else {
            ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
        };
        identifier.push(if valid { ch } else { '_' });
    }
    if identifier.is_empty() {
        "_".to_string()
    } else {
        identifier
    }
}

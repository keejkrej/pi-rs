//! Port of packages/codemode/src/identifier.ts

#![allow(unused_variables)]

/// The identifier a script uses for a tool: characters that are not valid in a JavaScript
/// identifier become `_`. `mcp__docs__search` stays as is, `my-tool` becomes `my_tool`.
pub fn to_codemode_identifier(name: &str) -> String {
    todo!("port: to_codemode_identifier")
}

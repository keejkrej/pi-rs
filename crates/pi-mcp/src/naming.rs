//! Port of packages/mcp/src/naming.ts

/// Provider tool names are limited to 64 characters of `[A-Za-z0-9_-]`.
const MAX_TOOL_NAME_LENGTH: usize = 64;

fn sanitize_tool_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for unit in raw.encode_utf16() {
        if unit < 128 {
            let b = unit as u8;
            if b.is_ascii_alphanumeric() || b == b'_' {
                out.push(b as char);
                continue;
            }
        }
        out.push('_');
    }
    out
}

/// `mcp__<server>__<tool>`, sanitized and shortened with a hash suffix when too long. Like Codex,
/// everything but `[A-Za-z0-9_]` becomes `_`, so the name is also the identifier code mode scripts
/// call it by. `is_taken` reports names used by a different MCP tool: sanitizing can map two tools
/// to one name (`a-b` and `a_b`), which then get the hash suffix.
pub fn create_mcp_tool_name(server: &str, tool: &str, is_taken: Option<&dyn Fn(&str) -> bool>) -> String {
    let name = sanitize_tool_name(&format!("mcp__{server}__{tool}"));
    if name.len() <= MAX_TOOL_NAME_LENGTH && !is_taken.is_some_and(|f| f(&name)) {
        return name;
    }
    let hash_bytes = pi_js::crypto::sha256(format!("{server}\0{tool}").as_bytes());
    let hash_hex = hex::encode(hash_bytes);
    let hash = &hash_hex[..8];
    let prefix_len = name.len().min(MAX_TOOL_NAME_LENGTH - hash.len() - 1);
    format!("{}_{}", &name[..prefix_len], hash)
}

/// Namespace of a server's tools: `mcp__<server>` with `-` replaced by `_`, like the tool names. Server
/// names that differ only in `-` and `_` share a namespace, so a configuration must not have both.
pub fn mcp_namespace(server: &str) -> String {
    format!("mcp__{}", server.replace('-', "_"))
}

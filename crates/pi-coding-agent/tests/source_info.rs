//! Port of packages/coding-agent/test/source-info.test.ts

use pi_coding_agent::core::source_info::get_resource_path_key;
use pi_js::env::testing::PlatformGuard;
use pretty_assertions::assert_eq;

#[test]
fn folds_file_path_casing_on_windows_but_keeps_synthetic_identifiers_unchanged() {
    let _platform = PlatformGuard::set("win32");
    assert_eq!(
        get_resource_path_key(r"C:\Users\Me\ext.ts"),
        get_resource_path_key(r"c:\users\me\ext.ts")
    );
    assert_eq!(get_resource_path_key("builtin:MCP"), "builtin:MCP");
    assert_ne!(
        get_resource_path_key("builtin:MCP"),
        get_resource_path_key("builtin:mcp")
    );
    assert_eq!(get_resource_path_key("<inline:Name>"), "<inline:Name>");
}

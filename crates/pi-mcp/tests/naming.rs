use pi_mcp::{create_mcp_tool_name, mcp_namespace};
use pretty_assertions::assert_eq;

#[test]
fn joins_server_and_tool_and_replaces_characters_outside_identifier_set() {
    assert_eq!(create_mcp_tool_name("docs", "search", None), "mcp__docs__search");
    assert_eq!(
        create_mcp_tool_name("my-server", "get.item/v2", None),
        "mcp__my_server__get_item_v2"
    );
}

#[test]
fn shortens_names_over_64_characters_with_a_hash_of_server_and_tool() {
    let long = create_mcp_tool_name("server", &"x".repeat(100), None);
    assert_eq!(long.len(), 64);
    assert!(long.starts_with("mcp__server__"));
    let (prefix, hash) = long.rsplit_once('_').unwrap();
    assert!(prefix.len() == 55 && hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));

    // Names that share their first 55 characters still differ.
    assert_ne!(
        create_mcp_tool_name("server", &format!("{}y", "x".repeat(100)), None),
        long
    );
    // The hash covers the unsanitized names and is stable.
    assert_eq!(create_mcp_tool_name("server", &"x".repeat(100), None), long);
    assert_ne!(
        create_mcp_tool_name("ser-ver", &"x".repeat(100), None),
        create_mcp_tool_name("ser_ver", &"x".repeat(100), None)
    );
}

#[test]
fn adds_the_hash_suffix_when_the_sanitized_name_is_taken() {
    let taken = create_mcp_tool_name("s", "a_b", None);
    assert_eq!(taken, "mcp__s__a_b");
    let is_taken = |name: &str| name == taken;
    let second = create_mcp_tool_name("s", "a-b", Some(&is_taken));
    assert!(second.starts_with("mcp__s__a_b_"));
    assert_eq!(second.len(), "mcp__s__a_b_".len() + 8);
    assert_ne!(create_mcp_tool_name("s", "a_b", Some(&is_taken)), second);
}

#[test]
fn formats_mcp_namespace_like_tool_name_prefix() {
    assert_eq!(mcp_namespace("docs"), "mcp__docs");
    assert_eq!(mcp_namespace("my-server"), "mcp__my_server");
    assert_eq!(mcp_namespace("my-server"), mcp_namespace("my_server"));
    assert!(
        create_mcp_tool_name("my-server", "search", None).starts_with(&format!("{}__", mcp_namespace("my-server")))
    );
}

//! Port of packages/coding-agent/test/suite/regressions/8337-utf8-bom-parsing.test.ts (unit coding-agent-config-foundation-utils)

use pi_coding_agent::utils::frontmatter::parse_frontmatter;
use pi_coding_agent::utils::text::{split_bom, strip_bom};
use serde_json::json;

#[test]
fn strips_and_splits_leading_utf8_bom_on_utilities() {
    assert_eq!(split_bom("\u{FEFF}hello"), ("\u{FEFF}", "hello"));
    assert_eq!(split_bom("plain"), ("", "plain"));
    assert_eq!(strip_bom("\u{FEFF}hello"), "hello");
    assert_eq!(strip_bom("plain"), "plain");

    let parsed = parse_frontmatter("\u{FEFF}---\nname: bom-demo\n---\nContent");
    assert_eq!(parsed.frontmatter.get("name"), Some(&json!("bom-demo")));
    assert_eq!(parsed.body, "Content");
}

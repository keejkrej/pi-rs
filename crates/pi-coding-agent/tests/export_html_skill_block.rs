//! Port of `packages/coding-agent/test/export-html-skill-block.test.ts`.

use std::path::PathBuf;

use regex::Regex;

#[test]
fn html_export_skill_block_rendering() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let template_js =
        std::fs::read_to_string(manifest_dir.join("src/core/export_html/template.js")).expect("read template.js");

    assert!(Regex::new(r"parseSkillBlock").unwrap().is_match(&template_js));
    assert!(Regex::new(r"skillBlock\.userMessage").unwrap().is_match(&template_js));
    assert!(Regex::new(r"skill-invocation").unwrap().is_match(&template_js));
    assert!(Regex::new(r"hasUserContent").unwrap().is_match(&template_js));
    assert!(
        Regex::new(r"safeMarkedParse\(skillBlock\.content\)")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(Regex::new(r"tree-role-skill").unwrap().is_match(&template_js));
}

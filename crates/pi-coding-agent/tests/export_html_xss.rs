//! Port of `packages/coding-agent/test/export-html-xss.test.ts`.

use std::path::PathBuf;

use regex::Regex;

#[test]
fn export_html_template_js_sanitizes_links_images_and_metadata() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let template_js =
        std::fs::read_to_string(manifest_dir.join("src/core/export_html/template.js")).expect("read template.js");

    assert!(Regex::new(r"link\s*\(\s*token\s*\)").unwrap().is_match(&template_js));
    assert!(
        Regex::new(r"sanitizeMarkdownUrl\(token\.href\)")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(
        Regex::new(r"\^\(https\?\|mailto\|tel\|ftp\)")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(Regex::new(r"image\s*\(\s*token\s*\)").unwrap().is_match(&template_js));

    assert!(template_js.contains("replace(/[\\x00-\\x1f\\x7f]/g, '')"));
    assert!(
        !Regex::new(r"(?i)\^\\s\*\(javascript\|vbscript\|data\):")
            .unwrap()
            .is_match(&template_js)
    );

    assert!(Regex::new(r"escapeHtml\(href\)").unwrap().is_match(&template_js));
    assert!(!Regex::new(r"\$\{img\.mimeType\}").unwrap().is_match(&template_js));
    assert!(Regex::new(r"escapeHtml\(img\.mimeType").unwrap().is_match(&template_js));
    assert!(
        !Regex::new(r#";base64,\$\{img\.data\}""#)
            .unwrap()
            .is_match(&template_js)
    );
    assert!(
        Regex::new(r#";base64,\$\{escapeHtml\(img\.data \|\| (?:''|"")\)\}"#)
            .unwrap()
            .is_match(&template_js)
    );

    assert!(!Regex::new(r#"id="\$\{entryId\}""#).unwrap().is_match(&template_js));
    assert!(
        !Regex::new(r#"data-entry-id="\$\{entryId\}""#)
            .unwrap()
            .is_match(&template_js)
    );
    assert!(
        Regex::new(r"entry-\$\{escapeHtml\(entry\.id\)\}")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(
        Regex::new(r#"data-entry-id="\$\{escapeHtml\(entryId\)\}""#)
            .unwrap()
            .is_match(&template_js)
    );

    assert!(
        !Regex::new(r"\[\$\{msg\.toolName \|\| 'tool'\}\]")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(!Regex::new(r"\[\$\{msg\.role\}\]").unwrap().is_match(&template_js));
    assert!(
        !Regex::new(r"\[model: \$\{entry\.modelId\}\]")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(
        !Regex::new(r"\[thinking: \$\{entry\.thinkingLevel\}\]")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(!Regex::new(r"\[\$\{entry\.type\}\]").unwrap().is_match(&template_js));

    assert!(
        Regex::new(r"\$\{escapeHtml\(msg\.toolName \|\| 'tool'\)\}")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(
        Regex::new(r"\$\{escapeHtml\(msg\.role\)\}")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(
        Regex::new(r"\$\{escapeHtml\(entry\.modelId\)\}")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(
        Regex::new(r"\$\{escapeHtml\(entry\.thinkingLevel\)\}")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(
        Regex::new(r"\$\{escapeHtml\(entry\.type\)\}")
            .unwrap()
            .is_match(&template_js)
    );

    assert!(
        !Regex::new(r"\$\{globalStats\.models\.join\(', '\) \|\| 'unknown'\}")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(
        Regex::new(r"\$\{escapeHtml\(globalStats\.models\.join\(', '\) \|\| 'unknown'\)\}")
            .unwrap()
            .is_match(&template_js)
    );
}

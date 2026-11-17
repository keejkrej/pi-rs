//! Port of packages/coding-agent/test/syntax-highlight.test.ts (unit coding-agent-tools-renderers-highlight)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "syntax_highlight/part_2.rs"]
mod part_2;
// @generated-mods end

use std::sync::Arc;

use indexmap::IndexMap;
use pi_coding_agent::utils::syntax_highlight::{
    HighlightFormatter, HighlightOptions, HighlightTheme, highlight, load_all_highlight_languages,
    render_highlighted_html, supports_language,
};

fn fmt_tag(tag: &'static str) -> HighlightFormatter {
    Arc::new(move |text| format!("[{tag}:{text}]"))
}

#[tokio::test(flavor = "current_thread")]
async fn loads_the_twenty_most_common_languages_at_startup_and_defers_the_rest() {
    let eager_languages = [
        "python",
        "java",
        "go",
        "javascript",
        "cpp",
        "typescript",
        "php",
        "ruby",
        "c",
        "csharp",
        "nix",
        "bash",
        "rust",
        "scala",
        "kotlin",
        "swift",
        "dart",
        "groovy",
        "perl",
        "lua",
    ];
    let eager_loaded_at_startup = eager_languages.iter().all(|lang| supports_language(lang));
    let uncommon_loaded_at_startup = supports_language("ada");

    assert!(eager_loaded_at_startup);
    assert!(!uncommon_loaded_at_startup);

    load_all_highlight_languages().await;
    assert!(supports_language("ada"));
}

#[test]
fn renders_highlighted_spans_with_the_provided_theme() {
    let theme = IndexMap::from([("keyword".to_owned(), fmt_tag("keyword"))]);
    let rendered = render_highlighted_html(r#"<span class="hljs-keyword">const</span> value"#, Some(&theme));
    assert_eq!(rendered, "[keyword:const] value");
}

#[test]
fn decodes_html_entities_emitted_by_highlight_js() {
    let rendered = render_highlighted_html("&lt;tag attr=&quot;value&quot;&gt;&amp;#x41;&#65;&lt;/tag&gt;", None);
    assert_eq!(rendered, r#"<tag attr="value">&#x41;A</tag>"#);
}

#[test]
fn inherits_parent_formatting_for_unmapped_nested_scopes() {
    let interpolation = "${x}";
    let theme = IndexMap::from([("string".to_owned(), fmt_tag("string"))]);
    let html = format!(r#"<span class="hljs-string">a<span class="hljs-subst">{interpolation}</span>b</span>"#);
    let rendered = render_highlighted_html(&html, Some(&theme));
    assert_eq!(rendered, format!("[string:a][string:{interpolation}][string:b]"));
}

#[test]
fn keeps_parent_formatting_across_unscoped_nested_spans() {
    let theme = IndexMap::from([("string".to_owned(), fmt_tag("string"))]);
    let rendered = render_highlighted_html(
        r#"<span class="hljs-string">a<span class="language-xml">b</span>c</span>"#,
        Some(&theme),
    );
    assert_eq!(rendered, "[string:a][string:b][string:c]");
}

#[test]
fn highlights_code_through_syntect_syntax_highlighter() {
    assert!(supports_language("typescript"));
    let theme: HighlightTheme = IndexMap::from([
        ("keyword".to_owned(), fmt_tag("keyword")),
        ("number".to_owned(), fmt_tag("number")),
    ]);
    let rendered = highlight(
        "const value = 1",
        HighlightOptions {
            language: Some("typescript".to_owned()),
            ignore_illegals: Some(true),
            language_subset: None,
            theme: Some(theme),
        },
    );
    assert!(rendered.contains("[keyword:const]"), "rendered: {rendered}");
    assert!(rendered.contains("[number:1]"), "rendered: {rendered}");
}

// WIRE(@tui-w3): once `pi_coding_agent::modes::interactive::theme::theme` lands, add tests for:
// - `get_language_from_path` (`lib/main.dart`, `flake.nix`, `script.groovy`, `build.gradle`, `src/Main.kt`)
// - `theme` syntax highlighting (`colors_each_line_of_python_docstrings_independently`,
//   `colors_diff_additions_and_deletions_in_fenced_diff_blocks`,
//   `keeps_cli_highlight_default_styled_scopes_mapped_to_theme_styles`).

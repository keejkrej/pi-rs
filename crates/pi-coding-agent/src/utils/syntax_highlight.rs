//! Port of packages/coding-agent/src/utils/syntax-highlight.ts (PORTING.md §2.6: syntect + two-face).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use syntect::easy::ScopeRangeIterator;
use syntect::parsing::{ParseState, ScopeStack, ScopeStackOp, SyntaxReference, SyntaxSet};

use crate::utils::html::decode_html_entity_at;

pub type HighlightFormatter = Arc<dyn Fn(&str) -> String + Send + Sync>;
pub type HighlightTheme = IndexMap<String, HighlightFormatter>;

#[derive(Clone, Default)]
pub struct HighlightOptions {
    pub language: Option<String>,
    pub ignore_illegals: Option<bool>,
    pub language_subset: Option<Vec<String>>,
    pub theme: Option<HighlightTheme>,
}

static EAGER_LANGUAGES: &[&str] = &[
    "python",
    "java",
    "go",
    "javascript",
    "json",
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

static ALL_LANGUAGES_LOADED: AtomicBool = AtomicBool::new(false);
static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

pub async fn load_all_highlight_languages() {
    if !ALL_LANGUAGES_LOADED.load(Ordering::Acquire) {
        let _ = &*SYNTAX_SET;
        ALL_LANGUAGES_LOADED.store(true, Ordering::Release);
    }
}

fn find_syntax<'a>(set: &'a SyntaxSet, name: &str) -> Option<&'a SyntaxReference> {
    let lower = name.trim().to_ascii_lowercase();
    let mapped = match lower.as_str() {
        "csharp" | "cs" => "C#",
        "cpp" | "c++" => "C++",
        "typescript" | "ts" => "TypeScript",
        "javascript" | "js" => "JavaScript",
        "bash" | "sh" | "shell" | "zsh" => "Bourne Again Shell (bash)",
        "nix" => "Nix",
        "rust" | "rs" => "Rust",
        "python" | "py" => "Python",
        "json" => "JSON",
        "java" => "Java",
        "go" | "golang" => "Go",
        "php" => "PHP",
        "ruby" | "rb" => "Ruby",
        "c" => "C",
        "scala" => "Scala",
        "kotlin" | "kt" => "Kotlin",
        "swift" => "Swift",
        "dart" => "Dart",
        "groovy" => "Groovy",
        "perl" | "pl" => "Perl",
        "lua" => "Lua",
        "html" => "HTML",
        "xml" => "XML",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "diff" | "patch" => "Diff",
        "ada" => "Ada",
        _ => "",
    };
    if !mapped.is_empty()
        && let Some(found) = set.find_syntax_by_name(mapped)
    {
        return Some(found);
    }
    set.find_syntax_by_token(&lower)
}

pub fn supports_language(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    if !ALL_LANGUAGES_LOADED.load(Ordering::Acquire) {
        return EAGER_LANGUAGES.contains(&lower.as_str());
    }
    find_syntax(&SYNTAX_SET, name).is_some()
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

fn map_syntect_scope_to_hljs(stack: &ScopeStack) -> Option<&'static str> {
    let scopes = stack.as_slice();
    for scope in scopes.iter().rev() {
        let s = scope.build_string();
        if s.starts_with("comment") {
            return Some("comment");
        }
        if s.starts_with("markup.inserted") {
            return Some("addition");
        }
        if s.starts_with("markup.deleted") {
            return Some("deletion");
        }
        if s.starts_with("markup.heading") {
            return Some("section");
        }
        if s.starts_with("string.regexp") {
            return Some("regexp");
        }
        if s.starts_with("string") {
            return Some("string");
        }
        if s.starts_with("constant.numeric") {
            return Some("number");
        }
        if s.starts_with("constant.language") || s.starts_with("constant") {
            return Some("literal");
        }
        if s.starts_with("variable.function")
            || s.starts_with("entity.name.function")
            || s.starts_with("support.function")
        {
            return Some("title.function_");
        }
        if s.starts_with("entity.name.tag") {
            return Some("name");
        }
        if s.starts_with("entity.other.attribute-name") {
            return Some("attr");
        }
        if s.starts_with("entity.name.type")
            || s.starts_with("entity.name.class")
            || s.starts_with("support.type")
            || s.starts_with("support.class")
        {
            return Some("type");
        }
        if s.starts_with("storage.type") || s.starts_with("storage.modifier") || s.starts_with("keyword") {
            return Some("keyword");
        }
        if s.starts_with("meta.annotation")
            || s.starts_with("meta.decorator")
            || s.starts_with("punctuation.definition.annotation")
        {
            return Some("meta");
        }
        if s.starts_with("variable") {
            return Some("variable");
        }
    }
    None
}

fn highlight_to_hljs_html(code: &str, language: Option<&str>) -> String {
    let set = &*SYNTAX_SET;
    let syntax = language
        .and_then(|lang| find_syntax(set, lang))
        .unwrap_or_else(|| set.find_syntax_plain_text());

    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut html = String::with_capacity(code.len());

    for line in syntect::util::LinesWithEndings::from(code) {
        let Ok(ops) = state.parse_line(line, set) else {
            html.push_str(&escape_html(line));
            continue;
        };
        for (range, op) in ScopeRangeIterator::new(&ops, line) {
            let _ = stack.apply(op);
            let _ = ScopeStackOp::Noop;
            if range.is_empty() {
                continue;
            }
            let chunk = escape_html(&line[range]);
            if let Some(cls) = map_syntect_scope_to_hljs(&stack) {
                html.push_str(&format!("<span class=\"hljs-{cls}\">{chunk}</span>"));
            } else {
                html.push_str(&chunk);
            }
        }
    }

    html
}

const SPAN_CLOSE: &str = "</span>";
const HIGHLIGHT_CLASS_PREFIX: &str = "hljs-";

fn get_scope_from_span_tag(tag: &str) -> Option<String> {
    let idx = tag.find("class")?;
    let after_class = &tag[idx + "class".len()..];
    let after_ws = after_class.trim_start();
    let after_eq = after_ws.strip_prefix('=')?.trim_start();
    let quote = after_eq.as_bytes().first().copied()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let rest = &after_eq[1..];
    let end = rest.find(quote as char)?;
    let class_value = &rest[..end];
    for class_name in class_value.split_whitespace() {
        if let Some(stripped) = class_name.strip_prefix(HIGHLIGHT_CLASS_PREFIX) {
            return Some(stripped.to_owned());
        }
    }
    None
}

fn get_scope_formatter<'a>(scope: &str, theme: &'a HighlightTheme) -> Option<&'a HighlightFormatter> {
    if let Some(exact) = theme.get(scope) {
        return Some(exact);
    }
    if let Some(dot_index) = scope.find('.')
        && let Some(fmt) = theme.get(&scope[..dot_index])
    {
        return Some(fmt);
    }
    if let Some(dash_index) = scope.find('-')
        && let Some(fmt) = theme.get(&scope[..dash_index])
    {
        return Some(fmt);
    }
    None
}

fn get_active_formatter<'a>(scopes: &[Option<String>], theme: &'a HighlightTheme) -> Option<&'a HighlightFormatter> {
    for scope in scopes.iter().rev().flatten() {
        if let Some(fmt) = get_scope_formatter(scope, theme) {
            return Some(fmt);
        }
    }
    theme.get("default")
}

fn is_span_open_tag_start(html: &str, index: usize) -> bool {
    let Some(rest) = html.get(index..) else {
        return false;
    };
    let Some(after) = rest.strip_prefix("<span") else {
        return false;
    };
    matches!(
        after.as_bytes().first().copied(),
        Some(b'>' | b' ' | b'\t' | b'\n' | b'\r')
    )
}

pub fn render_highlighted_html(html: &str, theme: Option<&HighlightTheme>) -> String {
    let empty_theme = HighlightTheme::new();
    let theme = theme.unwrap_or(&empty_theme);

    let mut output = String::with_capacity(html.len());
    let mut text_buffer = String::new();
    let mut scopes: Vec<Option<String>> = Vec::new();

    let flush_text = |text_buffer: &mut String, output: &mut String, scopes: &[Option<String>]| {
        if text_buffer.is_empty() {
            return;
        }
        if let Some(formatter) = get_active_formatter(scopes, theme) {
            let formatted = text_buffer
                .split('\n')
                .map(|line| {
                    if line.is_empty() {
                        String::new()
                    } else {
                        formatter(line)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            output.push_str(&formatted);
        } else {
            output.push_str(text_buffer);
        }
        text_buffer.clear();
    };

    let mut index = 0usize;
    while index < html.len() {
        if is_span_open_tag_start(html, index)
            && let Some(rel_end) = html[index + 5..].find('>')
        {
            let tag_end_index = index + 5 + rel_end;
            flush_text(&mut text_buffer, &mut output, &scopes);
            let tag = &html[index..=tag_end_index];
            let scope = get_scope_from_span_tag(tag);
            scopes.push(scope);
            index = tag_end_index + 1;
            continue;
        }

        if html[index..].starts_with(SPAN_CLOSE) {
            flush_text(&mut text_buffer, &mut output, &scopes);
            scopes.pop();
            index += SPAN_CLOSE.len();
            continue;
        }

        if html[index..].starts_with('&')
            && let Some(decoded) = decode_html_entity_at(html, index)
        {
            text_buffer.push_str(&decoded.text);
            index += decoded.length;
            continue;
        }

        if let Some(ch) = html[index..].chars().next() {
            text_buffer.push(ch);
            index += ch.len_utf8();
        } else {
            break;
        }
    }

    flush_text(&mut text_buffer, &mut output, &scopes);
    output
}

pub fn highlight(code: &str, options: HighlightOptions) -> String {
    let html = highlight_to_hljs_html(code, options.language.as_deref());
    render_highlighted_html(&html, options.theme.as_ref())
}

/// Number of registered grammars. It grows when the deferred grammars load, which can change the highlighting of
/// code that embeds another language, such as HTML in a JavaScript template literal.
pub fn get_highlight_language_count() -> usize {
    if !ALL_LANGUAGES_LOADED.load(Ordering::Acquire) {
        EAGER_LANGUAGES.len()
    } else {
        SYNTAX_SET.syntaxes().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_multiline_spans_per_non_empty_line_and_reports_grammar_count() {
        let theme: HighlightTheme = IndexMap::from([(
            "string".to_owned(),
            Arc::new(|line: &str| format!("<S>{line}</S>")) as HighlightFormatter,
        )]);
        let rendered = render_highlighted_html(r#"<span class="hljs-string">first\n\nsecond</span>"#, Some(&theme));
        // Decoded vs literal newline inside HTML text:
        assert_eq!(rendered, r#"<S>first\n\nsecond</S>"#);

        let multiline = render_highlighted_html("<span class=\"hljs-string\">first\n\nsecond</span>", Some(&theme));
        assert_eq!(multiline, "<S>first</S>\n\n<S>second</S>");
        assert!(get_highlight_language_count() >= EAGER_LANGUAGES.len());
    }
}

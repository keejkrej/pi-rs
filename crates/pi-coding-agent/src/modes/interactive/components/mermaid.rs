//! Port of packages/coding-agent/src/modes/interactive/components/mermaid.ts

use std::sync::Arc;

pub use crate::core::settings_manager::MermaidRenderingMode;
pub use crate::vendor::grok_mermaid::{Cls as MermaidCls, MermaidArt, Span as MermaidSpan};
use pi_tui::vendor::marked::{Marked, Token};

use super::super::theme::theme::{Theme, ThemeColor};
use super::markdown_transform::{MarkdownMessageType, MarkdownTransformContext, MarkdownTransformer};

pub struct MermaidTransformerOptions {
    pub get_mode: Arc<dyn Fn() -> MermaidRenderingMode + Send + Sync>,
    pub theme: Option<Theme>,
}

fn token_raw(token: &Token) -> &str {
    match token {
        Token::Blockquote(t) => &t.raw,
        Token::Br(t) => &t.raw,
        Token::Checkbox(t) => &t.raw,
        Token::Code(t) => &t.raw,
        Token::Codespan(t) => &t.raw,
        Token::Def(t) => &t.raw,
        Token::Del(t) => &t.raw,
        Token::Em(t) => &t.raw,
        Token::Escape(t) => &t.raw,
        Token::Heading(t) => &t.raw,
        Token::Hr(t) => &t.raw,
        Token::Html(t) => &t.raw,
        Token::Image(t) => &t.raw,
        Token::Link(t) => &t.raw,
        Token::List(t) => &t.raw,
        Token::ListItem(t) => &t.raw,
        Token::Paragraph(t) => &t.raw,
        Token::Space(t) => &t.raw,
        Token::Strong(t) => &t.raw,
        Token::Table(t) => &t.raw,
        Token::Tag(t) => &t.raw,
        Token::Text(t) => &t.raw,
        Token::Generic(t) => &t.raw,
    }
}

fn is_mermaid(token: &Token) -> Option<&str> {
    let Token::Code(code) = token else {
        return None;
    };
    let first_word = code.lang.as_deref()?.trim().split_whitespace().next()?;
    first_word.eq_ignore_ascii_case("mermaid").then_some(code.text.as_str())
}

fn code_span(line: &str) -> String {
    let content = if line.is_empty() { "\u{00a0}" } else { line };
    let mut longest_run = 0usize;
    let mut current_run = 0usize;
    for ch in content.chars() {
        if ch == '`' {
            current_run += 1;
            longest_run = longest_run.max(current_run);
        } else {
            current_run = 0;
        }
    }
    let fence = "`".repeat(longest_run + 1);
    let padding = if content.starts_with('`') || content.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{padding}{content}{padding}{fence}")
}

fn style_span(span: &MermaidSpan, t: &Theme) -> String {
    match span.cls {
        MermaidCls::Border => t.fg(ThemeColor::BorderMuted, &span.text),
        MermaidCls::Text => t.fg(ThemeColor::Text, &span.text),
        MermaidCls::Edge => t.fg(ThemeColor::Accent, &span.text),
        MermaidCls::EdgeLabel => t.fg(ThemeColor::Muted, &span.text),
        MermaidCls::Title => t.fg(ThemeColor::Accent, &t.bold(&span.text)),
        MermaidCls::None => span.text.clone(),
    }
}

fn themed_lines(art: &MermaidArt, t: &Theme) -> Vec<String> {
    art.styled
        .iter()
        .map(|row| row.iter().map(|span| style_span(span, t)).collect::<String>())
        .collect()
}

/// Create a transformer that replaces top-level Mermaid code blocks with Unicode terminal diagrams.
pub fn create_mermaid_markdown_transformer(options: MermaidTransformerOptions) -> MarkdownTransformer {
    let marked = Marked::new();
    Arc::new(move |markdown: &str, context: MarkdownTransformContext| {
        let mode = (options.get_mode)();
        if mode == MermaidRenderingMode::Off
            || !matches!(
                context.message_type,
                MarkdownMessageType::User | MarkdownMessageType::Assistant | MarkdownMessageType::Summary
            )
            || (context.is_streaming && mode != MermaidRenderingMode::Streaming)
        {
            return markdown.to_string();
        }

        let tokens = marked.lexer(markdown, None);
        let mut out = String::new();
        for token in &tokens.tokens {
            let Some(code_text) = is_mermaid(token) else {
                out.push_str(token_raw(token));
                continue;
            };
            let Some(art) = crate::vendor::grok_mermaid::render(code_text) else {
                out.push_str(token_raw(token));
                continue;
            };
            if (art.width as i64) > context.available_width {
                out.push_str(token_raw(token));
                continue;
            }
            if !context.is_streaming && !art.warnings.is_empty() {
                let suffix = if art.warnings.len() > 1 {
                    format!(" (+{} more)", art.warnings.len() - 1)
                } else {
                    String::new()
                };
                let warning = format!("Mermaid diagram not rendered: {}{suffix}", art.warnings[0]);
                let styled_warning = match options.theme {
                    Some(ref t) => t.fg(ThemeColor::Warning, &warning),
                    None => warning,
                };
                out.push_str(token_raw(token));
                out.push('\n');
                out.push_str(&code_span(&styled_warning));
                out.push_str("  \n");
                continue;
            }
            let lines = match options.theme {
                Some(ref t) => themed_lines(&art, t),
                None => art.plain,
            };
            let joined: Vec<String> = lines.into_iter().map(|l| code_span(&l)).collect();
            out.push_str(&joined.join("  \n"));
            out.push('\n');
        }
        out
    })
}

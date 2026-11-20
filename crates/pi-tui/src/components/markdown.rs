//! Port of packages/tui/src/components/markdown.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use indexmap::IndexMap;

use crate::latex::{RenderLatexOptions, render_latex};
use crate::terminal_image::{get_capabilities, hyperlink, is_image_line};
use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};
use crate::utils::{
    apply_background_to_line, cjk_punctuation_regex, flatten_lines, visible_width, wrap_text_with_ansi,
};
use crate::vendor::marked::{
    Lexer, Marked, MarkedExtension, MarkedOptions, MarkedTokenizer, Token, Tokenizer, TokenizerExtension,
    TokenizerLevel, TokenizerThis, Tokens, TokensList, cjk_friendly_em_strong,
};

// PORT: `(?=)` and `\1` need fancy_regex. `\s` is the JS whitespace class (PORTING.md §13.2).
static STRICT_STRIKETHROUGH_REGEX: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let ws = pi_js::regex::JS_WS_CHARS;
    let pattern = format!(r"^(~~)(?=[^{ws}~])((?:\\.|[^\\])*?(?:\\.|[^{ws}~\\]))\1(?=[^~]|$)");
    fancy_regex::Regex::new(&pattern).expect("STRICT_STRIKETHROUGH_REGEX")
});

/// `class MarkdownTokenizer extends Tokenizer`.
struct MarkdownTokenizer {
    base: Tokenizer,
}

impl MarkdownTokenizer {
    fn new() -> Self {
        Self { base: Tokenizer::new() }
    }
}

impl MarkedTokenizer for MarkdownTokenizer {
    fn lexer(&self) -> Lexer {
        self.base.lexer()
    }

    fn set_lexer(&self, lexer: Lexer) {
        self.base.set_lexer(lexer);
    }

    fn url(&self, src: &str) -> Option<Tokens::Link> {
        let token = self.base.url(src)?;
        // Bare URLs stop at CJK punctuation; explicit links and angle autolinks
        // use separate tokenizers and can still contain these characters.
        let Some(m) = cjk_punctuation_regex.find(&token.raw).ok().flatten() else {
            return Some(token);
        };
        self.base.url(&token.raw[..m.start()])
    }

    fn em_strong(&self, src: &str, masked_src: &str, prev_char: &str) -> Option<Token> {
        cjk_friendly_em_strong(src, masked_src, prev_char, &self.lexer())
    }

    /// `override del(src)`.
    ///
    /// PORT: marked calls `del(src, maskedSrc, prevChar)`. This override only reads `src`,
    /// matching the TS parameter list. The body uses `STRICT_STRIKETHROUGH_REGEX` and `lexer.inlineTokens`.
    fn del(&self, src: &str, _masked_src: &str, _prev_char: Option<&str>) -> Option<Tokens::Del> {
        let captures = STRICT_STRIKETHROUGH_REGEX.captures(src).ok().flatten()?;
        let raw = captures.get(0)?.as_str().to_string();
        let text = captures.get(2)?.as_str().to_string();
        let tokens = self.lexer().inline_tokens(&text);
        Some(Tokens::Del { raw, text, tokens })
    }
}

/// `"latex" | "latexBlock"`.
enum LatexKind {
    /// `"latex"`.
    Inline,
    /// `"latexBlock"`.
    Block,
}

/// `LatexToken`.
struct LatexToken {
    kind: LatexKind,
    raw: String,
    text: String,
    pending: Option<bool>,
}

impl LatexToken {
    /// Structural cast to `Tokens.Generic` (`interface LatexToken extends Tokens.Generic`).
    fn into_generic(self) -> Tokens::Generic {
        Tokens::Generic {
            r#type: match self.kind {
                LatexKind::Inline => "latex".to_string(),
                LatexKind::Block => "latexBlock".to_string(),
            },
            raw: self.raw,
            tokens: None,
            text: Some(self.text),
            pending: self.pending,
            extra: IndexMap::new(),
        }
    }
}

static PENDING_DOLLAR_MATH_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\\[A-Za-z]+|[_^=+*/<>()\[\]|±≤≥≠≈∈→⇒∞∫∑√\-]").expect("PENDING_DOLLAR_MATH_REGEX")
});
static DOLLAR_WS_START_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    let pattern = format!(r"^\${}", pi_js::regex::JS_WS_CLASS);
    regex::Regex::new(&pattern).expect("DOLLAR_WS_START_REGEX")
});
static TRAILING_WS_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    let pattern = format!(r"{}\z", pi_js::regex::JS_WS_CLASS);
    regex::Regex::new(&pattern).expect("TRAILING_WS_REGEX")
});
static LEADING_DIGIT_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[0-9]").expect("LEADING_DIGIT_REGEX"));
static UPPER_ENV_VAR_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    let ws = pi_js::regex::JS_WS_CHARS;
    let pattern = format!(r"^[A-Z_][A-Z0-9_]*(?:[^A-Za-z0-9_{ws}])?\z");
    regex::Regex::new(&pattern).expect("UPPER_ENV_VAR_REGEX")
});
static LEADING_IDENT_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*").expect("LEADING_IDENT_REGEX"));
static BLOCK_LATEX_DOLLAR_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^ {0,3}\$\$[ \t]*(?:\n)?([\s\S]*?)\$\$[ \t]*(?:\n|\z)").expect("BLOCK_LATEX_DOLLAR_REGEX")
});
static BLOCK_LATEX_BRACKET_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^ {0,3}\\\[[ \t]*(?:\n)?([\s\S]*?)\\\][ \t]*(?:\n|\z)").expect("BLOCK_LATEX_BRACKET_REGEX")
});
static PENDING_BLOCK_BRACKET_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^ {0,3}\\\[[ \t]*(?:\n)?([\s\S]*)\z").expect("PENDING_BLOCK_BRACKET_REGEX"));
static PENDING_BLOCK_DOLLAR_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^ {0,3}\$\$[ \t]*(?:\n)?([\s\S]*)\z").expect("PENDING_BLOCK_DOLLAR_REGEX"));
static LATEX_BLOCK_START_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?:^|\n) {0,3}(?:\$\$|\\\[)").expect("LATEX_BLOCK_START_REGEX"));
static FENCE_MARKER_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^(?:`{3,}|~{3,})").expect("FENCE_MARKER_REGEX"));
static ORDERED_LIST_MARKER_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^(?: {0,3})([0-9]{1,9}[.)])[ \t]+").expect("ORDERED_LIST_MARKER_REGEX"));
static UNORDERED_LIST_MARKER_REGEX: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"^(?: {0,3})([-+*])(?:[ \t]+|(?=\r?\n|\z))").expect("UNORDERED_LIST_MARKER_REGEX")
});

fn is_escaped(source: &str, index: i64) -> bool {
    let len = pi_js::str16::len(source);
    if index <= 0 || (index as usize) > len {
        return false;
    }
    let byte_end = pi_js::str16::utf16_to_byte(source, index as usize);
    let backslashes = source.as_bytes()[..byte_end]
        .iter()
        .rev()
        .take_while(|&&b| b == b'\\')
        .count();
    backslashes % 2 == 1
}

fn find_closing_delimiter(source: &str, closing: &str, start: i64) -> i64 {
    let closing_len = pi_js::str16::len(closing);
    let mut index = pi_js::str16::index_of(source, closing, start.max(0) as usize);
    while let Some(idx) = index {
        if !is_escaped(source, idx as i64) {
            return idx as i64;
        }
        index = pi_js::str16::index_of(source, closing, idx + closing_len);
    }
    -1
}

fn looks_like_pending_dollar_math(source: &str) -> bool {
    PENDING_DOLLAR_MATH_REGEX.is_match(source)
}

fn tokenize_inline_latex(source: &str) -> Option<LatexToken> {
    let (opening, closing) = if source.starts_with("$$") {
        ("$$", "$$")
    } else if source.starts_with("\\(") {
        ("\\(", "\\)")
    } else if source.starts_with("\\[") {
        ("\\[", "\\]")
    } else if source.starts_with('$') && !DOLLAR_WS_START_REGEX.is_match(source) {
        ("$", "$")
    } else {
        return None;
    };

    let opening_len = pi_js::str16::len(opening) as i64;
    let closing_len = pi_js::str16::len(closing) as i64;
    let closing_index = find_closing_delimiter(source, closing, opening_len);

    if closing_index >= 0 && opening == "$" {
        let inner = pi_js::str16::slice(source, opening_len, Some(closing_index));
        let after = pi_js::str16::slice(source, closing_index + 1, None);
        if TRAILING_WS_REGEX.is_match(&inner)
            || LEADING_DIGIT_REGEX.is_match(&after)
            || (UPPER_ENV_VAR_REGEX.is_match(&inner) && LEADING_IDENT_REGEX.is_match(&after))
            || inner.contains('`')
        {
            return None;
        }
    }

    if closing_index < 0 {
        let pending_source = pi_js::str16::slice(source, opening_len, None);
        if opening.starts_with('\\') || looks_like_pending_dollar_math(&pending_source) {
            return Some(LatexToken {
                kind: LatexKind::Inline,
                raw: source.to_string(),
                text: pending_source,
                pending: Some(true),
            });
        }
        return None;
    }

    let text = pi_js::str16::slice(source, opening_len, Some(closing_index));
    if text.is_empty() || text.contains('\n') {
        return None;
    }

    let raw = pi_js::str16::slice(source, 0, Some(closing_index + closing_len));
    Some(LatexToken {
        kind: LatexKind::Inline,
        raw,
        text,
        pending: None,
    })
}

static TABLE_DELIMITER_ROW_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^ {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*\z")
        .expect("TABLE_DELIMITER_ROW_REGEX")
});

// A GFM table starts with a header row followed by a delimiter row such as "|---|---|" or "| --- |".
// The header needs a "|" only when the delimiter has none: a pipe-free delimiter without a pipe in the
// header is a thematic break, not a table.
fn contains_table_start(source: &str) -> bool {
    let lines: Vec<&str> = source.split('\n').collect();
    for index in 0..lines.len().saturating_sub(1) {
        let delimiter_row = lines[index + 1];
        if (delimiter_row.contains('|') || lines[index].contains('|'))
            && TABLE_DELIMITER_ROW_REGEX.is_match(delimiter_row)
        {
            return true;
        }
    }
    false
}

fn tokenize_block_latex(source: &str) -> Option<LatexToken> {
    // Display math never spans a table. Without this check, an unclosed "$$" or "\[" pairs with a later
    // opener or stays pending, swallowing the table. See https://github.com/earendil-works/pi/issues/11159.
    if let Some(captures) = BLOCK_LATEX_DOLLAR_REGEX.captures(source) {
        let raw = captures.get(0)?.as_str();
        let inner = captures.get(1)?.as_str();
        if !inner.is_empty() && !contains_table_start(inner) {
            return Some(LatexToken {
                kind: LatexKind::Block,
                raw: raw.to_string(),
                text: pi_js::str16::trim(inner).to_string(),
                pending: None,
            });
        }
    }

    if let Some(captures) = BLOCK_LATEX_BRACKET_REGEX.captures(source) {
        let raw = captures.get(0)?.as_str();
        let inner = captures.get(1)?.as_str();
        if !inner.is_empty() && !contains_table_start(inner) {
            return Some(LatexToken {
                kind: LatexKind::Block,
                raw: raw.to_string(),
                text: pi_js::str16::trim(inner).to_string(),
                pending: None,
            });
        }
    }

    if let Some(captures) = PENDING_BLOCK_BRACKET_REGEX.captures(source) {
        let raw = captures.get(0)?.as_str();
        let inner = captures.get(1)?.as_str();
        if !inner.is_empty() && !contains_table_start(inner) {
            return Some(LatexToken {
                kind: LatexKind::Block,
                raw: raw.to_string(),
                text: inner.to_string(),
                pending: Some(true),
            });
        }
    }

    if let Some(captures) = PENDING_BLOCK_DOLLAR_REGEX.captures(source) {
        let raw = captures.get(0)?.as_str();
        let inner = captures.get(1)?.as_str();
        if !inner.is_empty() && looks_like_pending_dollar_math(inner) && !contains_table_start(inner) {
            return Some(LatexToken {
                kind: LatexKind::Block,
                raw: raw.to_string(),
                text: inner.to_string(),
                pending: Some(true),
            });
        }
    }

    None
}

fn latex_block_start(source: &str) -> Option<i64> {
    let m = LATEX_BLOCK_START_REGEX.find(source)?;
    let utf16_idx = pi_js::str16::byte_to_utf16(source, m.start()) as i64;
    let offset = if m.as_str().starts_with('\n') { 1 } else { 0 };
    Some(utf16_idx + offset)
}

fn latex_start(source: &str) -> Option<i64> {
    [
        pi_js::str16::index_of(source, "$", 0),
        pi_js::str16::index_of(source, "\\(", 0),
        pi_js::str16::index_of(source, "\\[", 0),
    ]
    .into_iter()
    .flatten()
    .min()
    .map(|index| index as i64)
}

// PORT: marked calls `start`/`tokenizer` with `this` and the token list. The TS functions only take `source`.
static LATEX_MARKDOWN_EXTENSIONS: LazyLock<Vec<TokenizerExtension>> = LazyLock::new(|| {
    vec![
        TokenizerExtension {
            name: "latexBlock".to_string(),
            level: TokenizerLevel::Block,
            start: Some(Arc::new(|_this, source| latex_block_start(source))),
            tokenizer: Arc::new(|_this: &TokenizerThis, source: &str, _tokens: &mut Vec<Token>| {
                tokenize_block_latex(source).map(LatexToken::into_generic)
            }),
            child_tokens: None,
        },
        TokenizerExtension {
            name: "latex".to_string(),
            level: TokenizerLevel::Inline,
            start: Some(Arc::new(|_this, source| latex_start(source))),
            tokenizer: Arc::new(|_this: &TokenizerThis, source: &str, _tokens: &mut Vec<Token>| {
                tokenize_inline_latex(source).map(LatexToken::into_generic)
            }),
            child_tokens: None,
        },
    ]
});

static MARKDOWN_PARSER: LazyLock<Marked> = LazyLock::new(|| {
    let parser = Marked::new();
    parser.set_options(MarkedOptions {
        tokenizer: Some(Arc::new(MarkdownTokenizer::new())),
    });
    parser.r#use(MarkedExtension {
        extensions: Some(LATEX_MARKDOWN_EXTENSIONS.to_vec()),
    });
    parser
});

// Trim streamed partial closing fences so code blocks do not shrink/flicker
// when the final fence character arrives. See https://github.com/earendil-works/pi/issues/5825.
fn trim_partial_closing_fences(tokens: &mut [Token]) {
    let Some(token) = tokens.last_mut() else {
        return;
    };
    match token {
        Token::List(list) => {
            if let Some(last_item) = list.items.last_mut() {
                trim_partial_closing_fences(&mut last_item.tokens);
            }
        }
        Token::Blockquote(quote) => {
            trim_partial_closing_fences(&mut quote.tokens);
        }
        Token::Code(code) => {
            let Some(marker) = FENCE_MARKER_REGEX.find(&code.raw).map(|m| m.as_str()) else {
                return;
            };
            let Some(last_line) = code.raw.split('\n').next_back() else {
                return;
            };
            let Some(marker_char) = marker.chars().next() else {
                return;
            };
            if last_line.is_empty() || last_line.len() >= marker.len() || !last_line.chars().all(|c| c == marker_char) {
                return;
            }
            let sliced = pi_js::str16::slice(&code.text, 0, Some(-(last_line.len() as i64)));
            code.text = sliced.strip_suffix('\n').unwrap_or(&sliced).to_string();
        }
        _ => {}
    }
}

// An unclosed comment runs to the end of an HTML block, as in CommonMark, so streamed comments stay hidden.
static HTML_COMMENT_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"<!--[\s\S]*?(?:-->|\z)").expect("HTML_COMMENT_REGEX"));

fn strip_html_comments(html: &str) -> String {
    HTML_COMMENT_REGEX.replace_all(html, "").into_owned()
}

/// Remove block HTML tokens that hold only comments, so they render no line. A removed comment between two blank
/// lines, or at the start or end, also drops a blank line, so it leaves no extra gap.
/// See <https://github.com/earendil-works/pi/issues/11710>.
fn remove_html_comment_blocks(tokens: &mut Vec<Token>) {
    let mut i = tokens.len();
    while i > 0 {
        i -= 1;
        match &mut tokens[i] {
            Token::List(list) => {
                for item in &mut list.items {
                    remove_html_comment_blocks(&mut item.tokens);
                }
            }
            Token::Blockquote(quote) => {
                remove_html_comment_blocks(&mut quote.tokens);
            }
            Token::Html(html) if pi_js::str16::trim(&strip_html_comments(&html.raw)).is_empty() => {
                tokens.remove(i);
                if matches!(tokens.get(i), Some(Token::Space(_)))
                    && (i == 0 || matches!(tokens.get(i - 1), Some(Token::Space(_))))
                {
                    tokens.remove(i);
                } else if i == tokens.len() && i > 0 && matches!(tokens.get(i - 1), Some(Token::Space(_))) {
                    i -= 1;
                    tokens.remove(i);
                }
            }
            _ => {}
        }
    }
}

/// Default text styling for markdown content.
/// Applied to all text unless overridden by markdown formatting.
#[derive(Clone, Default)]
pub struct DefaultTextStyle {
    /// Foreground color function
    pub color: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
    /// Background color function
    pub bg_color: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
    /// Bold text
    pub bold: Option<bool>,
    /// Italic text
    pub italic: Option<bool>,
    /// Strikethrough text
    pub strikethrough: Option<bool>,
    /// Underline text
    pub underline: Option<bool>,
}

/// Theme functions for markdown elements.
/// Each function takes text and returns styled text with ANSI codes.
#[derive(Clone)]
pub struct MarkdownTheme {
    pub heading: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub link: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub link_url: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub code: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub code_block: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub code_block_border: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub quote: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub quote_border: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub hr: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub list_bullet: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub bold: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub italic: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub strikethrough: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub underline: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub highlight_code: Option<Arc<dyn Fn(&str, Option<&str>) -> Vec<String> + Send + Sync>>,
    /// Prefix applied to each rendered code block line (default: "  ")
    pub code_block_indent: Option<String>,
}

#[derive(Clone, Default)]
pub struct MarkdownOptions {
    /// Preserve source list markers instead of normalizing them.
    pub preserve_ordered_list_markers: Option<bool>,
    /// Preserve source backslash escapes instead of normalizing escaped punctuation.
    pub preserve_backslash_escapes: Option<bool>,
    /// Transform source Markdown before parsing, with the exact width available for content.
    pub transform: Option<Arc<dyn Fn(&str, i64) -> String + Send + Sync>>,
    /// Render supported LaTeX math expressions as Unicode text (default: true).
    pub render_latex: Option<bool>,
}

type StyleFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

struct InlineStyleContext {
    apply_text: StyleFn,
    style_prefix: String,
}

/// `{ source, tokens }` held for token reuse across theme and width invalidation.
struct CachedTokens {
    source: String,
    tokens: TokensList,
}

/// A top-level block of the last render, keyed by its source, reused by the next render.
#[derive(Clone)]
struct RenderedBlock {
    /// Width and type of the following block (which decides the spacing) that the lines were rendered for.
    width: usize,
    next_token_type: Option<String>,
    lines: Vec<String>,
    /// Highlighted lines of the block's code, keyed by language and source; they survive a resize or new spacing.
    highlighted_code: IndexMap<String, Vec<String>>,
}

struct MarkdownFields {
    text: String,
    // Left/right padding
    padding_x: i64,
    // Top/bottom padding
    padding_y: i64,
    default_text_style: Option<DefaultTextStyle>,
    theme: MarkdownTheme,
    options: MarkdownOptions,
    default_style_prefix: Option<String>,
    // Cache for rendered output
    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
    // Parsed tokens depend only on the source, so they survive theme and width invalidation.
    // PORT: Rust has no tracing GC for WeakRef; retain the active cache strongly across invalidations
    // and replace/drop it on source changes or component drop so synchronous bursts keep cached entries.
    cached_tokens: Option<Arc<CachedTokens>>,
    // The last render's top-level blocks, cleared on invalidate() or empty text.
    blocks: Option<IndexMap<String, RenderedBlock>>,
}

struct MarkdownInner {
    fields: Mutex<MarkdownFields>,
}

/// `Markdown` component.
///
/// PORT: TS class with identity. Handle so containers can store `Arc<dyn Component>`.
#[derive(Clone)]
pub struct Markdown {
    inner: Arc<MarkdownInner>,
}

fn apply_default_style_with(
    default_text_style: Option<&DefaultTextStyle>,
    theme: &MarkdownTheme,
    text: &str,
) -> String {
    let Some(style) = default_text_style else {
        return text.to_string();
    };

    let mut styled = text.to_string();
    if let Some(color) = &style.color {
        styled = color(&styled);
    }
    if style.bold.unwrap_or(false) {
        styled = (theme.bold)(&styled);
    }
    if style.italic.unwrap_or(false) {
        styled = (theme.italic)(&styled);
    }
    if style.strikethrough.unwrap_or(false) {
        styled = (theme.strikethrough)(&styled);
    }
    if style.underline.unwrap_or(false) {
        styled = (theme.underline)(&styled);
    }
    styled
}

fn token_type_name(token: &Token) -> &str {
    match token {
        Token::Blockquote(_) => "blockquote",
        Token::Br(_) => "br",
        Token::Checkbox(_) => "checkbox",
        Token::Code(_) => "code",
        Token::Codespan(_) => "codespan",
        Token::Def(_) => "def",
        Token::Del(_) => "del",
        Token::Em(_) => "em",
        Token::Escape(_) => "escape",
        Token::Heading(_) => "heading",
        Token::Hr(_) => "hr",
        Token::Html(_) | Token::Tag(_) => "html",
        Token::Image(_) => "image",
        Token::Link(_) => "link",
        Token::List(_) => "list",
        Token::ListItem(_) => "list_item",
        Token::Paragraph(_) => "paragraph",
        Token::Space(_) => "space",
        Token::Strong(_) => "strong",
        Token::Table(_) => "table",
        Token::Text(_) => "text",
        Token::Generic(g) => &g.r#type,
    }
}

fn token_text(token: &Token) -> Option<&str> {
    match token {
        Token::Blockquote(t) => Some(&t.text),
        Token::Code(t) => Some(&t.text),
        Token::Codespan(t) => Some(&t.text),
        Token::Del(t) => Some(&t.text),
        Token::Em(t) => Some(&t.text),
        Token::Escape(t) => Some(&t.text),
        Token::Heading(t) => Some(&t.text),
        Token::Html(t) => Some(&t.text),
        Token::Image(t) => Some(&t.text),
        Token::Link(t) => Some(&t.text),
        Token::ListItem(t) => Some(&t.text),
        Token::Paragraph(t) => Some(&t.text),
        Token::Strong(t) => Some(&t.text),
        Token::Tag(t) => Some(&t.text),
        Token::Text(t) => Some(&t.text),
        Token::Generic(g) => g.text.as_deref(),
        Token::Br(_)
        | Token::Checkbox(_)
        | Token::Def(_)
        | Token::Hr(_)
        | Token::List(_)
        | Token::Space(_)
        | Token::Table(_) => None,
    }
}

fn has_next_non_space(next_token_type: Option<&str>) -> bool {
    matches!(next_token_type, Some(next) if !next.is_empty() && next != "space")
}

struct MarkdownSnapshot {
    text: String,
    padding_x: i64,
    padding_y: i64,
    default_text_style: Option<DefaultTextStyle>,
    theme: MarkdownTheme,
    options: MarkdownOptions,
    default_style_prefix: Option<String>,
    // Highlighted code of the block being rendered, only set while `render_block` runs.
    block_highlighted_code: Option<IndexMap<String, Vec<String>>>,
}

impl MarkdownSnapshot {
    fn from_fields(fields: &MarkdownFields) -> Self {
        Self {
            text: fields.text.clone(),
            padding_x: fields.padding_x,
            padding_y: fields.padding_y,
            default_text_style: fields.default_text_style.clone(),
            theme: fields.theme.clone(),
            options: fields.options.clone(),
            default_style_prefix: fields.default_style_prefix.clone(),
            block_highlighted_code: None,
        }
    }

    /// Render one top-level block to output lines: wrapped, with margins, and padded or backgrounded to the width.
    fn render_block(
        &mut self,
        block: &mut RenderedBlock,
        token: &Token,
        next_token_type: Option<&str>,
        width: usize,
        content_width: i64,
    ) {
        self.block_highlighted_code = Some(std::mem::take(&mut block.highlighted_code));
        let margin_len = self.padding_x.max(0) as usize;
        let left_margin = " ".repeat(margin_len);
        let right_margin = " ".repeat(margin_len);
        let width_i64 = width as i64;
        let bg_fn = self
            .default_text_style
            .as_ref()
            .and_then(|s| s.bg_color.as_ref())
            .cloned();
        let mut lines: Vec<String> = Vec::new();
        for line in self.render_token(token, content_width, next_token_type, None) {
            if is_image_line(&line) {
                lines.push(line);
                continue;
            }
            for wrapped_line in wrap_text_with_ansi(&line, content_width) {
                let line_with_margins = format!("{left_margin}{wrapped_line}{right_margin}");
                if let Some(bg) = &bg_fn {
                    lines.push(apply_background_to_line(&line_with_margins, width_i64, Arc::clone(bg)));
                } else {
                    let visible_len = visible_width(&line_with_margins);
                    let padding_needed = (width_i64 - visible_len).max(0) as usize;
                    lines.push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
                }
            }
        }
        block.highlighted_code = self.block_highlighted_code.take().unwrap_or_default();
        flatten_lines(&lines);
        block.lines = lines;
        block.width = width;
        block.next_token_type = next_token_type.map(ToString::to_string);
    }

    fn apply_default_style(&self, text: &str) -> String {
        apply_default_style_with(self.default_text_style.as_ref(), &self.theme, text)
    }

    fn get_default_style_prefix(&mut self) -> String {
        let Some(style) = &self.default_text_style else {
            return String::new();
        };
        if let Some(prefix) = &self.default_style_prefix {
            return prefix.clone();
        }

        let sentinel = "\u{0000}";
        let styled = apply_default_style_with(Some(style), &self.theme, sentinel);
        let prefix = match styled.find(sentinel) {
            Some(idx) => styled[..idx].to_string(),
            None => String::new(),
        };
        self.default_style_prefix = Some(prefix.clone());
        prefix
    }

    fn get_style_prefix(&self, style_fn: &StyleFn) -> String {
        let sentinel = "\u{0000}";
        let styled = style_fn(sentinel);
        match styled.find(sentinel) {
            Some(idx) => styled[..idx].to_string(),
            None => String::new(),
        }
    }

    fn get_default_inline_style_context(&mut self) -> InlineStyleContext {
        let style_prefix = self.get_default_style_prefix();
        let default_style = self.default_text_style.clone();
        let theme = self.theme.clone();
        InlineStyleContext {
            apply_text: Arc::new(move |text: &str| apply_default_style_with(default_style.as_ref(), &theme, text)),
            style_prefix,
        }
    }

    fn render_token(
        &mut self,
        token: &Token,
        width: i64,
        next_token_type: Option<&str>,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();

        match token {
            Token::Heading(heading) => {
                let heading_level = heading.depth;
                let heading_prefix = format!("{} ", "#".repeat(heading_level.max(0) as usize));

                let heading_fn = Arc::clone(&self.theme.heading);
                let bold_fn = Arc::clone(&self.theme.bold);
                let underline_fn = Arc::clone(&self.theme.underline);
                let heading_style_fn: StyleFn = if heading_level == 1 {
                    Arc::new(move |text: &str| heading_fn(&bold_fn(&underline_fn(text))))
                } else {
                    Arc::new(move |text: &str| heading_fn(&bold_fn(text)))
                };

                let heading_style_context = InlineStyleContext {
                    style_prefix: self.get_style_prefix(&heading_style_fn),
                    apply_text: Arc::clone(&heading_style_fn),
                };

                let heading_text = self.render_inline_tokens(&heading.tokens, Some(&heading_style_context));
                let styled_heading = if heading_level >= 3 {
                    format!("{}{}", heading_style_fn(&heading_prefix), heading_text)
                } else {
                    heading_text
                };
                lines.push(styled_heading);
                if has_next_non_space(next_token_type) {
                    lines.push(String::new());
                }
            }
            Token::Paragraph(paragraph) => {
                let paragraph_text = self.render_inline_tokens(&paragraph.tokens, style_context);
                lines.push(paragraph_text);
                if matches!(
                    next_token_type,
                    Some(next) if !next.is_empty() && next != "list" && next != "space"
                ) {
                    lines.push(String::new());
                }
            }
            Token::Text(_) => {
                lines.push(self.render_inline_tokens(std::slice::from_ref(token), style_context));
            }
            Token::Generic(generic) if generic.r#type == "latexBlock" => {
                let latex_text = generic.text.as_deref().unwrap_or("");
                let raw_trimmed = pi_js::str16::trim(&generic.raw);
                let mut rendered = if !generic.pending.unwrap_or(false) && self.options.render_latex != Some(false) {
                    render_latex(latex_text, Some(RenderLatexOptions { display: Some(true) }))
                } else {
                    None
                };
                // A 2D layout cannot be wrapped row by row; fall back to the raw source when it does not fit.
                if rendered
                    .as_deref()
                    .is_none_or(|text| text.split('\n').any(|line| visible_width(line) > width))
                {
                    rendered = Some(raw_trimmed.to_string());
                }
                for line in rendered.unwrap_or_default().split('\n') {
                    lines.push(self.apply_default_style(line));
                }
                if has_next_non_space(next_token_type) {
                    lines.push(String::new());
                }
            }
            Token::Code(code) => {
                let indent = self.theme.code_block_indent.as_deref().unwrap_or("  ");
                let lang_str = code.lang.as_deref().unwrap_or("");
                lines.push((self.theme.code_block_border)(&format!("```{lang_str}")));
                if let Some(highlight_code) = &self.theme.highlight_code {
                    let key = format!("{lang_str}\0{}", code.text);
                    if let Some(cached) = self.block_highlighted_code.as_ref().and_then(|m| m.get(&key)).cloned() {
                        for hl_line in cached {
                            lines.push(format!("{indent}{hl_line}"));
                        }
                    } else {
                        let highlighted_lines = highlight_code(&code.text, code.lang.as_deref());
                        for hl_line in &highlighted_lines {
                            lines.push(format!("{indent}{hl_line}"));
                        }
                        if let Some(map) = self.block_highlighted_code.as_mut() {
                            map.insert(key, highlighted_lines);
                        }
                    }
                } else {
                    for code_line in code.text.split('\n') {
                        lines.push(format!("{indent}{}", (self.theme.code_block)(code_line)));
                    }
                }
                lines.push((self.theme.code_block_border)("```"));
                if has_next_non_space(next_token_type) {
                    lines.push(String::new());
                }
            }
            Token::List(list) => {
                lines.extend(self.render_list(list, 0, width, style_context));
            }
            Token::Table(table) => {
                lines.extend(self.render_table(table, width, next_token_type, style_context));
            }
            Token::Blockquote(quote) => {
                // With no room for another border ("│ " plus at least one content column), render the
                // nested content without one. Otherwise each extra level re-wraps every child line at
                // width 1 and the output grows exponentially with nesting depth.
                if width < 3 {
                    for i in 0..quote.tokens.len() {
                        let quote_token = &quote.tokens[i];
                        let next_quote_token_type = quote.tokens.get(i + 1).map(token_type_name);
                        lines.extend(self.render_token(quote_token, width, next_quote_token_type, style_context));
                    }
                    return lines;
                }

                let quote_fn = Arc::clone(&self.theme.quote);
                let italic_fn = Arc::clone(&self.theme.italic);
                let quote_style: StyleFn = Arc::new(move |text: &str| quote_fn(&italic_fn(text)));
                let quote_style_prefix = self.get_style_prefix(&quote_style);
                let apply_quote_style = |line: &str| -> String {
                    if quote_style_prefix.is_empty() {
                        return quote_style(line);
                    }
                    let line_with_reapplied_style = line.replace("\x1b[0m", &format!("\x1b[0m{quote_style_prefix}"));
                    quote_style(&line_with_reapplied_style)
                };

                let quote_content_width = width - 2;
                let quote_inline_style_context = InlineStyleContext {
                    apply_text: Arc::new(|text: &str| text.to_string()),
                    style_prefix: quote_style_prefix.clone(),
                };

                let mut rendered_quote_lines: Vec<String> = Vec::new();
                for i in 0..quote.tokens.len() {
                    let quote_token = &quote.tokens[i];
                    let next_quote_token_type = quote.tokens.get(i + 1).map(token_type_name);
                    rendered_quote_lines.extend(self.render_token(
                        quote_token,
                        quote_content_width,
                        next_quote_token_type,
                        Some(&quote_inline_style_context),
                    ));
                }

                while rendered_quote_lines.last().is_some_and(String::is_empty) {
                    rendered_quote_lines.pop();
                }

                for quote_line in rendered_quote_lines {
                    let styled_line = apply_quote_style(&quote_line);
                    for wrapped_line in wrap_text_with_ansi(&styled_line, quote_content_width) {
                        lines.push(format!("{}{}", (self.theme.quote_border)("│ "), wrapped_line));
                    }
                }
                if has_next_non_space(next_token_type) {
                    lines.push(String::new());
                }
            }
            Token::Hr(_) => {
                lines.push((self.theme.hr)(&"─".repeat(width.max(0).min(80) as usize)));
                if has_next_non_space(next_token_type) {
                    lines.push(String::new());
                }
            }
            Token::Html(html) => {
                lines.push(self.apply_default_style(pi_js::str16::trim(&strip_html_comments(&html.raw))));
            }
            Token::Tag(tag) => {
                lines.push(self.apply_default_style(pi_js::str16::trim(&strip_html_comments(&tag.raw))));
            }
            Token::Space(_) => {
                lines.push(String::new());
            }
            _ => {
                if let Some(text) = token_text(token) {
                    lines.push(text.to_string());
                }
            }
        }

        lines
    }

    fn render_inline_tokens(&mut self, tokens: &[Token], style_context: Option<&InlineStyleContext>) -> String {
        let mut result = String::new();
        let default_ctx;
        let resolved_style_context = match style_context {
            Some(ctx) => ctx,
            None => {
                default_ctx = self.get_default_inline_style_context();
                &default_ctx
            }
        };
        let style_prefix = &resolved_style_context.style_prefix;
        let apply_text_with_newlines = |text: &str| -> String {
            text.split('\n')
                .map(|segment| (resolved_style_context.apply_text)(segment))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // After a skipped HTML comment, drop the next text's leading space if the text before ended with one,
        // so "a <!-- c --> b" renders as "a b".
        let mut drop_leading_space = false;
        for (i, token) in tokens.iter().enumerate() {
            let was_drop_leading_space = drop_leading_space;
            drop_leading_space = false;
            match token {
                Token::Generic(generic) if generic.r#type == "latex" => {
                    let latex_text = generic.text.as_deref().unwrap_or("");
                    let rendered = if !generic.pending.unwrap_or(false) && self.options.render_latex != Some(false) {
                        render_latex(latex_text, None).unwrap_or_else(|| generic.raw.clone())
                    } else {
                        generic.raw.clone()
                    };
                    result.push_str(&apply_text_with_newlines(&rendered));
                }
                Token::Escape(escape) => {
                    let text = if self.options.preserve_backslash_escapes.unwrap_or(false) {
                        &escape.raw
                    } else {
                        &escape.text
                    };
                    result.push_str(&apply_text_with_newlines(text));
                }
                Token::Text(text) => {
                    if let Some(nested) = &text.tokens
                        && !nested.is_empty()
                    {
                        result.push_str(&self.render_inline_tokens(nested, Some(resolved_style_context)));
                    } else {
                        let content = if was_drop_leading_space && text.text.starts_with([' ', '\t']) {
                            text.text.trim_start_matches([' ', '\t'])
                        } else {
                            text.text.as_str()
                        };
                        result.push_str(&apply_text_with_newlines(content));
                    }
                }
                Token::Paragraph(paragraph) => {
                    result.push_str(&self.render_inline_tokens(&paragraph.tokens, Some(resolved_style_context)));
                }
                Token::Strong(strong) => {
                    let bold_content = self.render_inline_tokens(&strong.tokens, Some(resolved_style_context));
                    result.push_str(&(self.theme.bold)(&bold_content));
                    result.push_str(style_prefix);
                }
                Token::Em(em) => {
                    let italic_content = self.render_inline_tokens(&em.tokens, Some(resolved_style_context));
                    result.push_str(&(self.theme.italic)(&italic_content));
                    result.push_str(style_prefix);
                }
                Token::Codespan(codespan) => {
                    result.push_str(&(self.theme.code)(&codespan.text));
                    result.push_str(style_prefix);
                }
                Token::Link(link) => {
                    let link_text = self.render_inline_tokens(&link.tokens, Some(resolved_style_context));
                    let styled_link = (self.theme.link)(&(self.theme.underline)(&link_text));
                    if get_capabilities().hyperlinks {
                        result.push_str(&hyperlink(&styled_link, &link.href));
                        result.push_str(style_prefix);
                    } else {
                        let href_for_comparison = link.href.strip_prefix("mailto:").unwrap_or(&link.href);
                        if link.text == link.href || link.text == href_for_comparison {
                            result.push_str(&styled_link);
                        } else {
                            result.push_str(&styled_link);
                            result.push_str(&(self.theme.link_url)(&format!(" ({})", link.href)));
                        }
                        result.push_str(style_prefix);
                    }
                }
                Token::Br(_) => {
                    result.push('\n');
                }
                Token::Del(del) => {
                    let del_content = self.render_inline_tokens(&del.tokens, Some(resolved_style_context));
                    result.push_str(&(self.theme.strikethrough)(&del_content));
                    result.push_str(style_prefix);
                }
                Token::Html(html) => {
                    if html.raw.starts_with("<!--") {
                        drop_leading_space = i == 0 || tokens[i - 1].raw().ends_with([' ', '\t']);
                    } else {
                        result.push_str(&apply_text_with_newlines(&html.raw));
                    }
                }
                Token::Tag(tag) => {
                    if tag.raw.starts_with("<!--") {
                        drop_leading_space = i == 0 || tokens[i - 1].raw().ends_with([' ', '\t']);
                    } else {
                        result.push_str(&apply_text_with_newlines(&tag.raw));
                    }
                }
                _ => {
                    if let Some(text) = token_text(token) {
                        result.push_str(&apply_text_with_newlines(text));
                    }
                }
            }
        }

        if !style_prefix.is_empty() {
            while result.ends_with(style_prefix) {
                result.truncate(result.len() - style_prefix.len());
            }
        }

        result
    }

    fn get_ordered_list_marker(&self, item: &Tokens::ListItem) -> Option<String> {
        let captures = ORDERED_LIST_MARKER_REGEX.captures(&item.raw)?;
        Some(format!("{} ", captures.get(1)?.as_str()))
    }

    fn get_unordered_list_marker(&self, item: &Tokens::ListItem) -> Option<String> {
        let captures = UNORDERED_LIST_MARKER_REGEX.captures(&item.raw).ok().flatten()?;
        Some(format!("{} ", captures.get(1)?.as_str()))
    }

    fn render_list(
        &mut self,
        token: &Tokens::List,
        depth: i64,
        width: i64,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let indent = "    ".repeat(depth.max(0) as usize);
        let start_number = match token.start {
            Tokens::ListStart::Number(n) => n,
            Tokens::ListStart::Empty => 1,
        };
        let preserve = self.options.preserve_ordered_list_markers.unwrap_or(false);

        for (i, item) in token.items.iter().enumerate() {
            let is_last_item = i == token.items.len() - 1;
            let bullet = if token.ordered {
                if preserve {
                    self.get_ordered_list_marker(item)
                        .unwrap_or_else(|| format!("{}. ", start_number + i as i64))
                } else {
                    format!("{}. ", start_number + i as i64)
                }
            } else if preserve {
                self.get_unordered_list_marker(item).unwrap_or_else(|| "- ".to_string())
            } else {
                "- ".to_string()
            };
            let task_marker = if item.task {
                if item.checked.unwrap_or(false) { "[x] " } else { "[ ] " }
            } else {
                ""
            };
            let marker = format!("{bullet}{task_marker}");
            let first_prefix = format!("{indent}{}", (self.theme.list_bullet)(&marker));
            let continuation_prefix = format!("{indent}{}", " ".repeat(visible_width(&marker).max(0) as usize));
            let item_width = (width - visible_width(&first_prefix)).max(1);
            let mut rendered_any_line = false;

            for item_token in &item.tokens {
                if let Token::List(nested) = item_token {
                    lines.extend(self.render_list(nested, depth + 1, width, style_context));
                    rendered_any_line = true;
                    continue;
                }

                let item_lines = self.render_token(item_token, item_width, None, style_context);
                for line in item_lines {
                    for wrapped_line in wrap_text_with_ansi(&line, item_width) {
                        let line_prefix = if rendered_any_line {
                            &continuation_prefix
                        } else {
                            &first_prefix
                        };
                        lines.push(format!("{line_prefix}{wrapped_line}"));
                        rendered_any_line = true;
                    }
                }
            }

            if !rendered_any_line {
                lines.push(first_prefix);
            }

            if token.loose && !is_last_item {
                lines.push(String::new());
            }
        }

        lines
    }

    fn get_longest_word_width(&self, text: &str, max_width: Option<i64>) -> i64 {
        let longest = text
            .split(pi_js::str16::is_js_whitespace)
            .filter(|word| !word.is_empty())
            .map(visible_width)
            .max()
            .unwrap_or(0);
        match max_width {
            Some(max) => longest.min(max),
            None => longest,
        }
    }

    fn wrap_cell_text(&self, text: &str, max_width: i64, style_prefix: Option<&str>) -> Vec<String> {
        let prefix = style_prefix.unwrap_or("");
        let lines = wrap_text_with_ansi(text, max_width.max(1));
        let len = lines.len();
        lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let style_reset = if index + 1 < len {
                    "\x1b[22;23;24;25;27;28;29;39m"
                } else {
                    ""
                };
                format!("{line}{style_reset}{prefix}")
            })
            .collect()
    }

    fn render_table(
        &mut self,
        token: &Tokens::Table,
        available_width: i64,
        next_token_type: Option<&str>,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let num_cols = token.header.len();
        if num_cols == 0 {
            return lines;
        }

        let num_cols_i64 = num_cols as i64;
        let border_overhead = 3 * num_cols_i64 + 1;
        let available_for_cells = available_width - border_overhead;
        if available_for_cells < num_cols_i64 {
            let mut fallback_lines = if !token.raw.is_empty() {
                wrap_text_with_ansi(&token.raw, available_width)
            } else {
                Vec::new()
            };
            if has_next_non_space(next_token_type) {
                fallback_lines.push(String::new());
            }
            return fallback_lines;
        }

        let max_unbroken_word_width = 30i64;
        let mut natural_widths = vec![0i64; num_cols];
        let mut min_word_widths = vec![1i64; num_cols];

        for (i, cell) in token.header.iter().enumerate() {
            let header_text = self.render_inline_tokens(&cell.tokens, style_context);
            natural_widths[i] = visible_width(&header_text);
            min_word_widths[i] = 1.max(self.get_longest_word_width(&header_text, Some(max_unbroken_word_width)));
        }

        for row in &token.rows {
            for (i, cell) in row.iter().enumerate() {
                if i >= natural_widths.len() {
                    natural_widths.resize(i + 1, 0);
                    min_word_widths.resize(i + 1, 1);
                }
                let cell_text = self.render_inline_tokens(&cell.tokens, style_context);
                natural_widths[i] = natural_widths[i].max(visible_width(&cell_text));
                min_word_widths[i] = min_word_widths[i]
                    .max(1)
                    .max(self.get_longest_word_width(&cell_text, Some(max_unbroken_word_width)));
            }
        }

        let mut min_column_widths = min_word_widths.clone();
        let mut min_cells_width: i64 = min_column_widths.iter().sum();

        if min_cells_width > available_for_cells {
            min_column_widths = vec![1i64; num_cols];
            let remaining = available_for_cells - num_cols_i64;

            if remaining > 0 {
                let total_weight: i64 = min_word_widths.iter().take(num_cols).map(|&w| (w - 1).max(0)).sum();
                let growth: Vec<i64> = min_word_widths
                    .iter()
                    .take(num_cols)
                    .map(|&w| {
                        let weight = (w - 1).max(0);
                        if total_weight > 0 {
                            ((weight as f64 / total_weight as f64) * (remaining as f64)).floor() as i64
                        } else {
                            0
                        }
                    })
                    .collect();

                for i in 0..num_cols {
                    min_column_widths[i] += growth[i];
                }

                let allocated: i64 = growth.iter().sum();
                let mut leftover = remaining - allocated;
                for width in min_column_widths.iter_mut() {
                    if leftover <= 0 {
                        break;
                    }
                    *width += 1;
                    leftover -= 1;
                }
            }

            min_cells_width = min_column_widths.iter().sum();
        }

        let total_natural_width: i64 = natural_widths.iter().sum::<i64>() + border_overhead;
        let column_widths: Vec<i64> = if total_natural_width <= available_width {
            (0..num_cols)
                .map(|i| natural_widths[i].max(min_column_widths[i]))
                .collect()
        } else {
            let total_grow_potential: i64 = (0..num_cols)
                .map(|i| (natural_widths[i] - min_column_widths[i]).max(0))
                .sum();
            let extra_width = (available_for_cells - min_cells_width).max(0);
            let mut cols: Vec<i64> = (0..num_cols)
                .map(|i| {
                    let natural_width = natural_widths[i];
                    let min_width = min_column_widths[i];
                    let min_width_delta = (natural_width - min_width).max(0);
                    let grow = if total_grow_potential > 0 {
                        ((min_width_delta as f64 / total_grow_potential as f64) * (extra_width as f64)).floor() as i64
                    } else {
                        0
                    };
                    min_width + grow
                })
                .collect();

            let allocated: i64 = cols.iter().sum();
            let mut remaining = available_for_cells - allocated;
            while remaining > 0 {
                let mut grew = false;
                for i in 0..num_cols {
                    if remaining <= 0 {
                        break;
                    }
                    if cols[i] < natural_widths[i] {
                        cols[i] += 1;
                        remaining -= 1;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            cols
        };

        let top_border_cells: Vec<String> = column_widths.iter().map(|&w| "─".repeat(w.max(0) as usize)).collect();
        lines.push(format!("┌─{}─┐", top_border_cells.join("─┬─")));

        let style_prefix = style_context.map(|c| c.style_prefix.as_str());
        let header_cell_lines: Vec<Vec<String>> = token
            .header
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let text = self.render_inline_tokens(&cell.tokens, style_context);
                self.wrap_cell_text(&text, column_widths[i], style_prefix)
            })
            .collect();
        let header_line_count = header_cell_lines.iter().map(Vec::len).max().unwrap_or(0);

        for line_idx in 0..header_line_count {
            let row_parts: Vec<String> = header_cell_lines
                .iter()
                .enumerate()
                .map(|(col_idx, cell_lines)| {
                    let text = cell_lines.get(line_idx).map(String::as_str).unwrap_or("");
                    let pad = (column_widths[col_idx] - visible_width(text)).max(0) as usize;
                    let padded = format!("{text}{}", " ".repeat(pad));
                    (self.theme.bold)(&padded)
                })
                .collect();
            lines.push(format!("│ {} │", row_parts.join(" │ ")));
        }

        let separator_cells: Vec<String> = column_widths.iter().map(|&w| "─".repeat(w.max(0) as usize)).collect();
        let separator_line = format!("├─{}─┤", separator_cells.join("─┼─"));
        lines.push(separator_line.clone());

        for (row_index, row) in token.rows.iter().enumerate() {
            let row_cell_lines: Vec<Vec<String>> = row
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    let col_w = column_widths.get(i).copied().unwrap_or(1);
                    let text = self.render_inline_tokens(&cell.tokens, style_context);
                    self.wrap_cell_text(&text, col_w, style_prefix)
                })
                .collect();
            let row_line_count = row_cell_lines.iter().map(Vec::len).max().unwrap_or(0);

            for line_idx in 0..row_line_count {
                let row_parts: Vec<String> = row_cell_lines
                    .iter()
                    .enumerate()
                    .map(|(col_idx, cell_lines)| {
                        let col_w = column_widths.get(col_idx).copied().unwrap_or(1);
                        let text = cell_lines.get(line_idx).map(String::as_str).unwrap_or("");
                        let pad = (col_w - visible_width(text)).max(0) as usize;
                        format!("{text}{}", " ".repeat(pad))
                    })
                    .collect();
                lines.push(format!("│ {} │", row_parts.join(" │ ")));
            }

            if row_index + 1 < token.rows.len() {
                lines.push(separator_line.clone());
            }
        }

        let bottom_border_cells: Vec<String> = column_widths.iter().map(|&w| "─".repeat(w.max(0) as usize)).collect();
        lines.push(format!("└─{}─┘", bottom_border_cells.join("─┴─")));

        if has_next_non_space(next_token_type) {
            lines.push(String::new());
        }
        lines
    }
}

impl Markdown {
    fn fields(&self) -> MutexGuard<'_, MarkdownFields> {
        self.inner.fields.lock().unwrap()
    }

    fn snapshot(&self) -> MarkdownSnapshot {
        let fields = self.fields();
        MarkdownSnapshot::from_fields(&fields)
    }

    pub fn new(
        text: &str,
        padding_x: i64,
        padding_y: i64,
        theme: MarkdownTheme,
        default_text_style: Option<DefaultTextStyle>,
        options: Option<MarkdownOptions>,
    ) -> Self {
        Self {
            inner: Arc::new(MarkdownInner {
                fields: Mutex::new(MarkdownFields {
                    text: text.to_string(),
                    padding_x,
                    padding_y,
                    default_text_style,
                    theme,
                    options: options.unwrap_or_default(),
                    default_style_prefix: None,
                    cached_text: None,
                    cached_width: None,
                    cached_lines: None,
                    cached_tokens: None,
                    blocks: None,
                }),
            }),
        }
    }

    pub fn set_text(&self, text: &str) {
        self.fields().text = text.to_string();
        self.clear_render_cache();
    }

    pub fn invalidate(&self) {
        let mut fields = self.fields();
        fields.cached_text = None;
        fields.cached_width = None;
        fields.cached_lines = None;
        // The theme may have changed, which changes every styled line and the default style's prefix.
        fields.blocks = None;
        fields.default_style_prefix = None;
    }

    fn clear_render_cache(&self) {
        let mut fields = self.fields();
        fields.cached_text = None;
        fields.cached_width = None;
        fields.cached_lines = None;
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let (mut snapshot, cached_tokens, mut previous_blocks) = {
            let mut fields = self.fields();
            if let (Some(cached_lines), Some(cached_text), Some(cached_width)) =
                (&fields.cached_lines, &fields.cached_text, fields.cached_width)
                && cached_text == &fields.text
                && cached_width == width
            {
                return cached_lines.clone();
            }
            let snapshot = MarkdownSnapshot::from_fields(&fields);
            let cached_tokens = fields.cached_tokens.clone();
            let previous_blocks = fields.blocks.take();
            (snapshot, cached_tokens, previous_blocks)
        };

        let width_i64 = width as i64;
        let content_width = (width_i64 - snapshot.padding_x * 2).max(1);
        let text = match &snapshot.options.transform {
            Some(transform) => transform(&snapshot.text, content_width),
            None => snapshot.text.clone(),
        };

        if text.is_empty() || pi_js::str16::trim(&text).is_empty() {
            let mut fields = self.fields();
            fields.blocks = None;
            if fields.text == snapshot.text {
                fields.cached_text = Some(snapshot.text);
                fields.cached_width = Some(width);
                fields.cached_lines = Some(Vec::new());
            }
            return Vec::new();
        }

        let normalized_text = text.replace('\t', "   ");
        let parsed_tokens = match cached_tokens {
            Some(cached) if cached.source == normalized_text => cached,
            _ => {
                let mut tokens = MARKDOWN_PARSER.lexer(&normalized_text, None);
                trim_partial_closing_fences(&mut tokens.tokens);
                remove_html_comment_blocks(&mut tokens.tokens);
                Arc::new(CachedTokens {
                    source: normalized_text,
                    tokens,
                })
            }
        };

        // A reference link resolves through a definition anywhere in the text, so blocks with the same source can
        // render differently; such texts are rare in practice and render without reusing blocks.
        let reuse_blocks = parsed_tokens.tokens.links.is_empty();
        let mut blocks: IndexMap<String, RenderedBlock> = IndexMap::new();
        let mut content_lines: Vec<String> = Vec::new();
        let token_slice = &parsed_tokens.tokens.tokens;
        for i in 0..token_slice.len() {
            let token = &token_slice[i];
            let next_token_type = token_slice.get(i + 1).map(token_type_name);
            let raw = token.raw();
            let mut block = if reuse_blocks {
                blocks
                    .get(raw)
                    .cloned()
                    .or_else(|| previous_blocks.as_mut().and_then(|prev| prev.shift_remove(raw)))
                    .unwrap_or(RenderedBlock {
                        width: usize::MAX,
                        next_token_type: None,
                        lines: Vec::new(),
                        highlighted_code: IndexMap::new(),
                    })
            } else {
                RenderedBlock {
                    width: usize::MAX,
                    next_token_type: None,
                    lines: Vec::new(),
                    highlighted_code: IndexMap::new(),
                }
            };
            if block.width != width || block.next_token_type.as_deref() != next_token_type {
                snapshot.render_block(&mut block, token, next_token_type, width, content_width);
            }
            if reuse_blocks {
                blocks.insert(raw.to_string(), block.clone());
            }
            content_lines.extend(block.lines);
        }

        let bg_fn = snapshot.default_text_style.as_ref().and_then(|s| s.bg_color.as_ref());
        let empty_line = " ".repeat(width);
        let padding_y_count = snapshot.padding_y.max(0) as usize;
        let mut empty_lines: Vec<String> = Vec::with_capacity(padding_y_count);
        for _ in 0..padding_y_count {
            let line = match bg_fn {
                Some(bg) => apply_background_to_line(&empty_line, width_i64, Arc::clone(bg)),
                None => empty_line.clone(),
            };
            empty_lines.push(line);
        }
        flatten_lines(&empty_lines);

        let mut result = Vec::with_capacity(empty_lines.len() * 2 + content_lines.len());
        result.extend(empty_lines.iter().cloned());
        result.extend(content_lines);
        result.extend(empty_lines);

        {
            let mut fields = self.fields();
            if fields.default_style_prefix.is_none() && snapshot.default_style_prefix.is_some() {
                fields.default_style_prefix = snapshot.default_style_prefix;
            }
            if fields.text == snapshot.text {
                fields.cached_text = Some(snapshot.text);
                fields.cached_width = Some(width);
                fields.cached_lines = Some(result.clone());
                fields.cached_tokens = Some(parsed_tokens);
                fields.blocks = Some(blocks);
            }
        }

        if result.is_empty() { vec![String::new()] } else { result }
    }

    /// Apply default text style to a string.
    /// This is the base styling applied to all text content.
    /// NOTE: Background color is NOT applied here - it's applied at the padding stage
    /// to ensure it extends to the full line width.
    fn apply_default_style(&self, text: &str) -> String {
        self.snapshot().apply_default_style(text)
    }

    fn get_default_style_prefix(&self) -> String {
        let mut snapshot = self.snapshot();
        let prefix = snapshot.get_default_style_prefix();
        if snapshot.default_style_prefix.is_some() {
            let mut fields = self.fields();
            if fields.default_style_prefix.is_none() {
                fields.default_style_prefix = snapshot.default_style_prefix;
            }
        }
        prefix
    }

    fn get_style_prefix(&self, style_fn: &StyleFn) -> String {
        self.snapshot().get_style_prefix(style_fn)
    }

    fn get_default_inline_style_context(&self) -> InlineStyleContext {
        let mut snapshot = self.snapshot();
        let ctx = snapshot.get_default_inline_style_context();
        if snapshot.default_style_prefix.is_some() {
            let mut fields = self.fields();
            if fields.default_style_prefix.is_none() {
                fields.default_style_prefix = snapshot.default_style_prefix;
            }
        }
        ctx
    }

    fn render_token(
        &self,
        token: &Token,
        width: i64,
        next_token_type: Option<&str>,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        self.snapshot()
            .render_token(token, width, next_token_type, style_context)
    }

    fn render_inline_tokens(&self, tokens: &[Token], style_context: Option<&InlineStyleContext>) -> String {
        self.snapshot().render_inline_tokens(tokens, style_context)
    }

    fn get_ordered_list_marker(&self, item: &Tokens::ListItem) -> Option<String> {
        self.snapshot().get_ordered_list_marker(item)
    }

    fn get_unordered_list_marker(&self, item: &Tokens::ListItem) -> Option<String> {
        self.snapshot().get_unordered_list_marker(item)
    }

    /// Render a list with proper nesting support
    fn render_list(
        &self,
        token: &Tokens::List,
        depth: i64,
        width: i64,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        self.snapshot().render_list(token, depth, width, style_context)
    }

    /// Get the visible width of the longest word in a string.
    fn get_longest_word_width(&self, text: &str, max_width: Option<i64>) -> i64 {
        self.snapshot().get_longest_word_width(text, max_width)
    }

    /// Wrap a table cell to fit into a column.
    ///
    /// Delegates to wrapTextWithAnsi() so ANSI codes + long tokens are handled
    /// consistently with the rest of the renderer.
    fn wrap_cell_text(&self, text: &str, max_width: i64, style_prefix: Option<&str>) -> Vec<String> {
        self.snapshot().wrap_cell_text(text, max_width, style_prefix)
    }

    /// Render a table with width-aware cell wrapping.
    /// Cells that don't fit are wrapped to multiple lines.
    fn render_table(
        &self,
        token: &Tokens::Table,
        available_width: i64,
        next_token_type: Option<&str>,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        self.snapshot()
            .render_table(token, available_width, next_token_type, style_context)
    }
}

impl Component for Markdown {
    fn render(&self, width: usize) -> Vec<String> {
        Markdown::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        Markdown::invalidate(self)
    }
}

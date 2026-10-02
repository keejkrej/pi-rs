//! Port of packages/tui/src/components/markdown.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

use indexmap::IndexMap;

use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};
use crate::vendor::marked::{
    Lexer, Marked, MarkedTokenizer, Token, Tokenizer, TokenizerExtension, TokenizerLevel, TokenizerThis, Tokens,
    TokensList,
};

// PORT: `(?=)` and `\1` need fancy_regex. `\s` is the JS whitespace class (PORTING.md §13.2).
static STRICT_STRIKETHROUGH_REGEX: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let ws = pi_js::regex::JS_WS_CHARS;
    let pattern = format!(r"^(~~)(?=[^{ws}~])((?:\\.|[^\\])*?(?:\\.|[^{ws}~\\]))\1(?=[^~]|$)");
    fancy_regex::Regex::new(&pattern).expect("STRICT_STRIKETHROUGH_REGEX")
});

/// `class StrictStrikethroughTokenizer extends Tokenizer`.
struct StrictStrikethroughTokenizer {
    base: Tokenizer,
}

impl StrictStrikethroughTokenizer {
    fn new() -> Self {
        todo!("port: StrictStrikethroughTokenizer::new")
    }
}

impl MarkedTokenizer for StrictStrikethroughTokenizer {
    fn lexer(&self) -> Lexer {
        self.base.lexer()
    }

    fn set_lexer(&self, lexer: Lexer) {
        self.base.set_lexer(lexer);
    }

    /// `override del(src)`.
    ///
    /// PORT: marked calls `del(src, maskedSrc, prevChar)`. This override only reads `src`,
    /// matching the TS parameter list. The body uses `STRICT_STRIKETHROUGH_REGEX` and `lexer.inlineTokens`.
    fn del(&self, src: &str, masked_src: &str, prev_char: Option<&str>) -> Option<Tokens::Del> {
        todo!("port: StrictStrikethroughTokenizer::del")
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

fn is_escaped(source: &str, index: i64) -> bool {
    todo!("port: is_escaped")
}

fn find_closing_delimiter(source: &str, closing: &str, start: i64) -> i64 {
    todo!("port: find_closing_delimiter")
}

fn looks_like_pending_dollar_math(source: &str) -> bool {
    todo!("port: looks_like_pending_dollar_math")
}

fn tokenize_inline_latex(source: &str) -> Option<LatexToken> {
    todo!("port: tokenize_inline_latex")
}

fn tokenize_block_latex(source: &str) -> Option<LatexToken> {
    todo!("port: tokenize_block_latex")
}

fn latex_block_start(source: &str) -> Option<i64> {
    todo!("port: latex_block_start")
}

fn latex_start(source: &str) -> Option<i64> {
    todo!("port: latex_start")
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

static MARKDOWN_PARSER: LazyLock<Marked> = LazyLock::new(|| todo!("port: markdown_parser"));

// Trim streamed partial closing fences so code blocks do not shrink/flicker
// when the final fence character arrives. See https://github.com/earendil-works/pi/issues/5825.
fn trim_partial_closing_fences(tokens: &mut [Token]) {
    todo!("port: trim_partial_closing_fences")
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

/// `{ source, tokens }` held by the token `WeakRef`.
struct CachedTokens {
    source: String,
    tokens: TokensList,
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
    // Parsed tokens depend only on the source, so they survive theme and width invalidation. Held weakly: a token tree is
    // about ten times the size of its source, and every message of a long transcript keeps a Markdown component. The
    // tokens survive a burst of re-renders, such as a theme preview, and are collected afterwards.
    //
    // PORT: a Rust `Weak` is dropped as soon as its `Arc` is. JS `WeakRef` stays alive until GC.
    cached_tokens: Option<Weak<CachedTokens>>,
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

impl Markdown {
    fn fields(&self) -> MutexGuard<'_, MarkdownFields> {
        self.inner.fields.lock().unwrap()
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
                }),
            }),
        }
    }

    pub fn set_text(&self, text: &str) {
        self.fields().text = text.to_string();
        self.invalidate();
    }

    pub fn invalidate(&self) {
        let mut fields = self.fields();
        fields.cached_text = None;
        fields.cached_width = None;
        fields.cached_lines = None;
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: Markdown::render")
    }

    /// Apply default text style to a string.
    /// This is the base styling applied to all text content.
    /// NOTE: Background color is NOT applied here - it's applied at the padding stage
    /// to ensure it extends to the full line width.
    fn apply_default_style(&self, text: &str) -> String {
        todo!("port: Markdown::apply_default_style")
    }

    fn get_default_style_prefix(&self) -> String {
        todo!("port: Markdown::get_default_style_prefix")
    }

    fn get_style_prefix(&self, style_fn: &StyleFn) -> String {
        todo!("port: Markdown::get_style_prefix")
    }

    fn get_default_inline_style_context(&self) -> InlineStyleContext {
        todo!("port: Markdown::get_default_inline_style_context")
    }

    fn render_token(
        &self,
        token: &Token,
        width: i64,
        next_token_type: Option<&str>,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        todo!("port: Markdown::render_token")
    }

    fn render_inline_tokens(&self, tokens: &[Token], style_context: Option<&InlineStyleContext>) -> String {
        todo!("port: Markdown::render_inline_tokens")
    }

    fn get_ordered_list_marker(&self, item: &Tokens::ListItem) -> Option<String> {
        todo!("port: Markdown::get_ordered_list_marker")
    }

    fn get_unordered_list_marker(&self, item: &Tokens::ListItem) -> Option<String> {
        todo!("port: Markdown::get_unordered_list_marker")
    }

    /// Render a list with proper nesting support
    fn render_list(
        &self,
        token: &Tokens::List,
        depth: i64,
        width: i64,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        todo!("port: Markdown::render_list")
    }

    /// Get the visible width of the longest word in a string.
    fn get_longest_word_width(&self, text: &str, max_width: Option<i64>) -> i64 {
        todo!("port: Markdown::get_longest_word_width")
    }

    /// Wrap a table cell to fit into a column.
    ///
    /// Delegates to wrapTextWithAnsi() so ANSI codes + long tokens are handled
    /// consistently with the rest of the renderer.
    fn wrap_cell_text(&self, text: &str, max_width: i64, style_prefix: Option<&str>) -> Vec<String> {
        todo!("port: Markdown::wrap_cell_text")
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
        todo!("port: Markdown::render_table")
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

//! Port of npm `marked` 18.0.11 (node_modules/marked); vendored per PORTING.md §2.6.
//!
//! Lexer surface that `packages/tui/src/components/markdown.ts` calls (`Marked`, `Token`,
//! `Tokens`, `Tokenizer`, `TokenizerExtension`).
//!
//! PORT: the HTML renderer (`Renderer`, `TextRenderer`, `Parser`, `parse`, `parseInline`) is not
//! declared. Tokenizer rules other than `del`, and `Tokenizer.rules`, are not declared.
//! `MarkedOptions` only carries `tokenizer`. `MarkedExtension` only carries `extensions`.
//! `Lexer.inlineTokens` is the one-argument form `StrictStrikethroughTokenizer` calls.

#![allow(dead_code, non_snake_case, unused_variables)]

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

/// `marked` `Token`.
///
/// PORT: the `type` discriminant is the variant, not a field. Extension tokens (`"latex"`,
/// `"latexBlock"`) are [`Token::Generic`].
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Blockquote(Tokens::Blockquote),
    Br(Tokens::Br),
    Checkbox(Tokens::Checkbox),
    Code(Tokens::Code),
    Codespan(Tokens::Codespan),
    Def(Tokens::Def),
    Del(Tokens::Del),
    Em(Tokens::Em),
    Escape(Tokens::Escape),
    Heading(Tokens::Heading),
    Hr(Tokens::Hr),
    Html(Tokens::Html),
    Image(Tokens::Image),
    Link(Tokens::Link),
    List(Tokens::List),
    ListItem(Tokens::ListItem),
    Paragraph(Tokens::Paragraph),
    Space(Tokens::Space),
    Strong(Tokens::Strong),
    Table(Tokens::Table),
    Tag(Tokens::Tag),
    Text(Tokens::Text),
    Generic(Tokens::Generic),
}

#[allow(non_snake_case)]
pub mod Tokens {
    use indexmap::IndexMap;

    use super::Token;

    /// `"indented"`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum CodeBlockStyle {
        Indented,
    }

    /// `"center" | "left" | "right"`. `null` is [`Option::None`] on the field.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum TableAlign {
        Center,
        Left,
        Right,
    }

    /// `number | ""` on [`List::start`].
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum ListStart {
        Number(i64),
        /// `""`.
        Empty,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Blockquote {
        pub raw: String,
        pub text: String,
        pub tokens: Vec<Token>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Br {
        pub raw: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Checkbox {
        pub raw: String,
        pub checked: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Code {
        pub raw: String,
        pub code_block_style: Option<CodeBlockStyle>,
        pub lang: Option<String>,
        pub text: String,
        pub escaped: Option<bool>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Codespan {
        pub raw: String,
        pub text: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Def {
        pub raw: String,
        pub tag: String,
        pub href: String,
        pub title: String,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Del {
        pub raw: String,
        pub text: String,
        pub tokens: Vec<Token>,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Em {
        pub raw: String,
        pub text: String,
        pub tokens: Vec<Token>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Escape {
        pub raw: String,
        pub text: String,
    }

    /// `Tokens.Generic`.
    ///
    /// PORT: `text` and `pending` are the latex fields `markdown.ts` reads off the index
    /// signature. Every other extra key is [`Generic::extra`].
    #[derive(Clone, Debug, PartialEq)]
    pub struct Generic {
        pub r#type: String,
        pub raw: String,
        pub tokens: Option<Vec<Token>>,
        pub text: Option<String>,
        pub pending: Option<bool>,
        pub extra: IndexMap<String, serde_json::Value>,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Heading {
        pub raw: String,
        pub depth: i64,
        pub text: String,
        pub tokens: Vec<Token>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Hr {
        pub raw: String,
    }

    /// `type: "html"` block token (`Tokens.HTML`).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Html {
        pub raw: String,
        pub pre: bool,
        pub text: String,
        pub block: bool,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Image {
        pub raw: String,
        pub href: String,
        /// `string | null`.
        pub title: Option<String>,
        pub text: String,
        pub tokens: Vec<Token>,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Link {
        pub raw: String,
        pub href: String,
        /// `title?: string | null`. Outer `None` is absent, inner `None` is `null`.
        pub title: Option<Option<String>>,
        pub text: String,
        pub tokens: Vec<Token>,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct List {
        pub raw: String,
        pub ordered: bool,
        pub start: ListStart,
        pub loose: bool,
        pub items: Vec<ListItem>,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct ListItem {
        pub raw: String,
        pub task: bool,
        pub checked: Option<bool>,
        pub loose: bool,
        pub text: String,
        pub tokens: Vec<Token>,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Paragraph {
        pub raw: String,
        pub pre: Option<bool>,
        pub text: String,
        pub tokens: Vec<Token>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Space {
        pub raw: String,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Strong {
        pub raw: String,
        pub text: String,
        pub tokens: Vec<Token>,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Table {
        pub raw: String,
        pub align: Vec<Option<TableAlign>>,
        pub header: Vec<TableCell>,
        pub rows: Vec<Vec<TableCell>>,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct TableCell {
        pub text: String,
        pub tokens: Vec<Token>,
        pub header: bool,
        pub align: Option<TableAlign>,
    }

    /// `type: "html"` inline tag (`Tokens.Tag`). Distinct from [`Html`].
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Tag {
        pub raw: String,
        pub in_link: bool,
        pub in_raw_block: bool,
        pub text: String,
        pub block: bool,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Text {
        pub raw: String,
        pub text: String,
        pub tokens: Option<Vec<Token>>,
        pub escaped: Option<bool>,
    }
}

/// `Pick<Tokens.Link | Tokens.Image, "href" | "title">`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkLabel {
    pub href: String,
    /// `string | null | undefined`. Outer `None` is absent, inner `None` is `null`.
    pub title: Option<Option<String>>,
}

/// `Links`.
pub type Links = IndexMap<String, LinkLabel>;

/// `TokensList` (`Token[] & { links }`).
#[derive(Clone, Debug, PartialEq)]
pub struct TokensList {
    pub tokens: Vec<Token>,
    pub links: Links,
}

struct LexerState {
    in_link: bool,
    in_raw_block: bool,
    /// a link was produced in the inline run currently being scanned
    link_emitted: bool,
    top: bool,
}

struct InlineQueueEntry {
    src: String,
    tokens: Vec<Token>,
}

struct LexerInner {
    tokens: Mutex<TokensList>,
    options: Mutex<MarkedOptions>,
    state: Mutex<LexerState>,
    inline_queue: Mutex<Vec<InlineQueueEntry>>,
}

/// marked `Lexer`. `markdown.ts` only calls [`Lexer::inline_tokens`].
#[derive(Clone)]
pub struct Lexer {
    inner: Arc<LexerInner>,
}

impl Lexer {
    pub fn new() -> Self {
        todo!("port: Lexer::new")
    }

    /// `inlineTokens(src)`.
    ///
    /// PORT: the optional `tokens` out-array is not declared. The strikethrough override calls the one-arg form.
    pub fn inline_tokens(&self, src: &str) -> Vec<Token> {
        todo!("port: Lexer::inline_tokens")
    }
}

/// `"block" | "inline"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenizerLevel {
    /// `"block"`.
    Block,
    /// `"inline"`.
    Inline,
}

/// `this` value of a tokenizer extension (`TokenizerThis`).
pub struct TokenizerThis {
    pub lexer: Lexer,
}

/// `(this, src, tokens) => Tokens.Generic | undefined`.
pub type TokenizerExtensionFunction =
    Arc<dyn Fn(&TokenizerThis, &str, &mut Vec<Token>) -> Option<Tokens::Generic> + Send + Sync>;

/// `(this, src) => number | void`.
pub type TokenizerStartFunction = Arc<dyn Fn(&TokenizerThis, &str) -> Option<i64> + Send + Sync>;

/// `TokenizerExtension`. Renderer extensions are not part of this surface.
#[derive(Clone)]
pub struct TokenizerExtension {
    pub name: String,
    pub level: TokenizerLevel,
    pub start: Option<TokenizerStartFunction>,
    pub tokenizer: TokenizerExtensionFunction,
    pub child_tokens: Option<Vec<String>>,
}

/// Partial `MarkedOptions` passed to [`Marked::set_options`].
///
/// PORT: only `tokenizer`. `gfm` stays marked's default (`true`); `breaks` and `pedantic` stay `false`.
#[derive(Clone, Default)]
pub struct MarkedOptions {
    pub tokenizer: Option<Arc<dyn MarkedTokenizer>>,
}

/// Partial `MarkedExtension` passed to [`Marked::r#use`].
///
/// PORT: only `extensions`.
#[derive(Clone, Default)]
pub struct MarkedExtension {
    pub extensions: Option<Vec<TokenizerExtension>>,
}

/// marked `Tokenizer`, as a value `setOptions({ tokenizer })` can store.
///
/// PORT: TS subclasses `Tokenizer` and overrides `del`. Rust has no subclassing, so the stock
/// [`Tokenizer`] and `StrictStrikethroughTokenizer` both implement this trait. `lexer` is
/// `undefined` until the lexer assigns it; [`MarkedTokenizer::lexer`] still returns the class
/// field type.
pub trait MarkedTokenizer: Send + Sync {
    fn lexer(&self) -> Lexer;
    fn set_lexer(&self, lexer: Lexer);
    fn del(&self, src: &str, masked_src: &str, prev_char: Option<&str>) -> Option<Tokens::Del>;
}

struct TokenizerInner {
    options: Mutex<MarkedOptions>,
    /// `undefined` until [`MarkedTokenizer::set_lexer`].
    lexer: Mutex<Option<Lexer>>,
}

/// marked `Tokenizer` class.
///
/// PORT: `rules` and every rule method except `del` are not declared.
#[derive(Clone)]
pub struct Tokenizer {
    inner: Arc<TokenizerInner>,
}

impl Tokenizer {
    pub fn new() -> Self {
        todo!("port: Tokenizer::new")
    }
}

impl MarkedTokenizer for Tokenizer {
    fn lexer(&self) -> Lexer {
        todo!("port: Tokenizer::lexer")
    }

    fn set_lexer(&self, lexer: Lexer) {
        todo!("port: Tokenizer::set_lexer")
    }

    fn del(&self, src: &str, masked_src: &str, prev_char: Option<&str>) -> Option<Tokens::Del> {
        todo!("port: Tokenizer::del")
    }
}

struct MarkedInner {
    options: Mutex<MarkedOptions>,
    extensions: Mutex<Vec<TokenizerExtension>>,
}

/// marked `Marked`. `markdown.ts` constructs it, then calls `setOptions`, `use`, and `lexer`.
#[derive(Clone)]
pub struct Marked {
    inner: Arc<MarkedInner>,
}

impl Marked {
    pub fn new() -> Self {
        todo!("port: Marked::new")
    }

    pub fn set_options(&self, opt: MarkedOptions) -> Self {
        todo!("port: Marked::set_options")
    }

    /// Registers extensions with this Marked instance.
    ///
    /// The `renderer`, `tokenizer`, and `hooks` objects may each contain only the
    /// methods that should be overridden. Their methods are merged with built-in
    /// behavior and extensions registered by earlier calls.
    ///
    /// Use this method when supplying only some hook methods.
    pub fn r#use(&self, extension: MarkedExtension) -> Self {
        todo!("port: Marked::use")
    }

    pub fn lexer(&self, src: &str, options: Option<MarkedOptions>) -> TokensList {
        todo!("port: Marked::lexer")
    }
}

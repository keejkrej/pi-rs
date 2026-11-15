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

use std::sync::{Arc, LazyLock, Mutex};

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

impl Token {
    fn raw_mut(&mut self) -> Option<&mut String> {
        match self {
            Token::Blockquote(t) => Some(&mut t.raw),
            Token::Br(t) => Some(&mut t.raw),
            Token::Checkbox(t) => Some(&mut t.raw),
            Token::Code(t) => Some(&mut t.raw),
            Token::Codespan(t) => Some(&mut t.raw),
            Token::Def(t) => Some(&mut t.raw),
            Token::Del(t) => Some(&mut t.raw),
            Token::Em(t) => Some(&mut t.raw),
            Token::Escape(t) => Some(&mut t.raw),
            Token::Heading(t) => Some(&mut t.raw),
            Token::Hr(t) => Some(&mut t.raw),
            Token::Html(t) => Some(&mut t.raw),
            Token::Image(t) => Some(&mut t.raw),
            Token::Link(t) => Some(&mut t.raw),
            Token::List(t) => Some(&mut t.raw),
            Token::ListItem(t) => Some(&mut t.raw),
            Token::Paragraph(t) => Some(&mut t.raw),
            Token::Space(t) => Some(&mut t.raw),
            Token::Strong(t) => Some(&mut t.raw),
            Token::Table(t) => Some(&mut t.raw),
            Token::Tag(t) => Some(&mut t.raw),
            Token::Text(t) => Some(&mut t.raw),
            Token::Generic(t) => Some(&mut t.raw),
        }
    }

    fn raw(&self) -> &str {
        match self {
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

// =============================================================================
// ECMA-262 regex wrapper & grammar rules (port of marked/src/rules.ts)
// =============================================================================

type EcmaTestFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;
type EcmaExecFn = Arc<dyn Fn(&str) -> Option<pi_js::regex::JsMatch> + Send + Sync>;
type EcmaMatchAllFn = Arc<dyn Fn(&str) -> Vec<pi_js::regex::JsMatch> + Send + Sync>;
type EcmaReplaceFn = Arc<dyn Fn(&str, &str) -> String + Send + Sync>;
type EcmaReplaceCbFn = Arc<dyn Fn(&str, &mut dyn FnMut(&pi_js::regex::JsMatch) -> String) -> String + Send + Sync>;

#[derive(Clone)]
struct EcmaRegex {
    source: &'static str,
    is_noop: bool,
    test_fn: EcmaTestFn,
    exec_fn: EcmaExecFn,
    match_all_fn: EcmaMatchAllFn,
    replace_fn: EcmaReplaceFn,
    replace_cb_fn: EcmaReplaceCbFn,
}

impl EcmaRegex {
    fn compile(pattern: &str, flags: &str) -> Self {
        let re =
            Arc::new(pi_js::regex::ecma(pattern, flags).unwrap_or_else(|e| panic!("marked regex compile failed: {e}")));
        let global = flags.contains('g');
        let re_test = Arc::clone(&re);
        let re_exec = Arc::clone(&re);
        let re_all = Arc::clone(&re);
        let re_rep = Arc::clone(&re);
        let re_cb = re;
        Self {
            source: Box::leak(pattern.to_owned().into_boxed_str()),
            is_noop: false,
            test_fn: Arc::new(move |s| pi_js::regex::test(&re_test, s)),
            exec_fn: Arc::new(move |s| pi_js::regex::exec(&re_exec, s)),
            match_all_fn: Arc::new(move |s| pi_js::regex::match_all(&re_all, s)),
            replace_fn: Arc::new(move |s, rep| pi_js::regex::replace(&re_rep, s, rep, global)),
            replace_cb_fn: Arc::new(move |s, cb| pi_js::regex::replace_with(&re_cb, s, global, |m| cb(m))),
        }
    }

    fn noop() -> Self {
        Self {
            source: "",
            is_noop: true,
            test_fn: Arc::new(|_| false),
            exec_fn: Arc::new(|_| None),
            match_all_fn: Arc::new(|_| Vec::new()),
            replace_fn: Arc::new(|s, _| s.to_string()),
            replace_cb_fn: Arc::new(|s, _| s.to_string()),
        }
    }

    fn test(&self, text: &str) -> bool {
        (self.test_fn)(text)
    }

    fn exec(&self, text: &str) -> Option<pi_js::regex::JsMatch> {
        (self.exec_fn)(text)
    }

    fn match_all(&self, text: &str) -> Vec<pi_js::regex::JsMatch> {
        (self.match_all_fn)(text)
    }

    fn replace(&self, text: &str, rep: &str) -> String {
        (self.replace_fn)(text, rep)
    }

    fn replace_with(&self, text: &str, mut f: impl FnMut(&pi_js::regex::JsMatch) -> String) -> String {
        (self.replace_cb_fn)(text, &mut f)
    }
}

static CARET_RE: LazyLock<EcmaRegex> = LazyLock::new(|| EcmaRegex::compile(r"(^|[^\[])\^", "g"));

struct RegexEditor {
    source: String,
    flags: String,
}

impl RegexEditor {
    fn new(pattern: &str, flags: &str) -> Self {
        Self {
            source: pattern.to_string(),
            flags: flags.to_string(),
        }
    }

    fn replace(mut self, name: &str, val: &str) -> Self {
        let stripped = CARET_RE.replace(val, "$1");
        if let Some(idx) = self.source.find(name) {
            self.source.replace_range(idx..idx + name.len(), &stripped);
        }
        self
    }

    fn replace_all(mut self, name: &str, val: &str) -> Self {
        let stripped = CARET_RE.replace(val, "$1");
        self.source = self.source.replace(name, &stripped);
        self
    }

    fn build(self) -> EcmaRegex {
        EcmaRegex::compile(&self.source, &self.flags)
    }
}

struct OtherRules {
    code_remove_indent: EcmaRegex,
    output_link_replace: EcmaRegex,
    indent_code_compensation: EcmaRegex,
    beginning_space: EcmaRegex,
    ending_hash: EcmaRegex,
    starting_space_char: EcmaRegex,
    ending_space_char: EcmaRegex,
    non_space_char: EcmaRegex,
    new_line_char_global: EcmaRegex,
    tab_char_global: EcmaRegex,
    multiple_space_global: EcmaRegex,
    blank_line: EcmaRegex,
    double_blank_line: EcmaRegex,
    blockquote_start: EcmaRegex,
    blockquote_setext_replace: EcmaRegex,
    blockquote_setext_replace2: EcmaRegex,
    list_replace_nesting: EcmaRegex,
    list_is_task: EcmaRegex,
    list_replace_task: EcmaRegex,
    list_task_checkbox: EcmaRegex,
    any_line: EcmaRegex,
    href_brackets: EcmaRegex,
    table_delimiter: EcmaRegex,
    table_align_chars: EcmaRegex,
    table_row_blank_line: EcmaRegex,
    table_align_right: EcmaRegex,
    table_align_center: EcmaRegex,
    table_align_left: EcmaRegex,
    start_a_tag: EcmaRegex,
    end_a_tag: EcmaRegex,
    start_pre_script_tag: EcmaRegex,
    end_pre_script_tag: EcmaRegex,
    start_angle_bracket: EcmaRegex,
    end_angle_bracket: EcmaRegex,
    pedantic_href_title: EcmaRegex,
    unicode_alpha_numeric: EcmaRegex,
    find_pipe: EcmaRegex,
    slash_pipe: EcmaRegex,
    space_line: EcmaRegex,
    next_bullet_cache: [EcmaRegex; 4],
    hr_cache: [EcmaRegex; 4],
    fences_begin_cache: [EcmaRegex; 4],
    heading_begin_cache: [EcmaRegex; 4],
    html_begin_cache: [EcmaRegex; 4],
    blockquote_begin_cache: [EcmaRegex; 4],
    list_item_ast: EcmaRegex,
    list_item_plus: EcmaRegex,
    list_item_minus: EcmaRegex,
    list_item_dot: EcmaRegex,
    list_item_paren: EcmaRegex,
}

impl OtherRules {
    fn cached_slot(cache: &[EcmaRegex; 4], indent: usize) -> &EcmaRegex {
        let idx = indent.saturating_sub(1).min(3);
        &cache[idx]
    }

    fn list_item_regex(&self, bull: &str) -> EcmaRegex {
        match bull {
            r"\*" => self.list_item_ast.clone(),
            r"\+" => self.list_item_plus.clone(),
            r"\-" => self.list_item_minus.clone(),
            r"\d{1,9}\." => self.list_item_dot.clone(),
            r"\d{1,9}\)" => self.list_item_paren.clone(),
            "[*+-]" => EcmaRegex::compile(r"^( {0,3}[*+-])((?:[\t ][^\n]*)?(?:\n|$))", ""),
            other => EcmaRegex::compile(&format!(r"^( {{0,3}}{other})((?:[\t ][^\n]*)?(?:\n|$))"), ""),
        }
    }
}

struct BlockRules {
    blockquote: EcmaRegex,
    code: EcmaRegex,
    def: EcmaRegex,
    fences: EcmaRegex,
    heading: EcmaRegex,
    hr: EcmaRegex,
    html: EcmaRegex,
    lheading: EcmaRegex,
    list: EcmaRegex,
    newline: EcmaRegex,
    paragraph: EcmaRegex,
    table: EcmaRegex,
    text: EcmaRegex,
}

struct InlineRules {
    _backpedal: EcmaRegex,
    any_punctuation: EcmaRegex,
    autolink: EcmaRegex,
    block_skip: EcmaRegex,
    br: EcmaRegex,
    code: EcmaRegex,
    del: EcmaRegex,
    del_l_delim: EcmaRegex,
    del_r_delim: EcmaRegex,
    em_strong_l_delim: EcmaRegex,
    em_strong_r_delim_ast: EcmaRegex,
    em_strong_r_delim_und: EcmaRegex,
    escape: EcmaRegex,
    link: EcmaRegex,
    nolink: EcmaRegex,
    punctuation: EcmaRegex,
    reflink: EcmaRegex,
    reflink_search: EcmaRegex,
    tag: EcmaRegex,
    text: EcmaRegex,
    url: EcmaRegex,
}

struct GrammarSet {
    other: OtherRules,
    block_gfm: BlockRules,
    inline_gfm: InlineRules,
}

static GRAMMAR: LazyLock<GrammarSet> = LazyLock::new(|| {
    let make_4 = |f: fn(usize) -> EcmaRegex| -> [EcmaRegex; 4] { [f(0), f(1), f(2), f(3)] };

    let other = OtherRules {
        code_remove_indent: EcmaRegex::compile(r"^(?: {1,4}| {0,3}\t)", "gm"),
        output_link_replace: EcmaRegex::compile(r"\\([\[\]])", "g"),
        indent_code_compensation: EcmaRegex::compile(r"^(\s+)(?:```)", ""),
        beginning_space: EcmaRegex::compile(r"^\s+", ""),
        ending_hash: EcmaRegex::compile(r"#$", ""),
        starting_space_char: EcmaRegex::compile(r"^ ", ""),
        ending_space_char: EcmaRegex::compile(r" $", ""),
        non_space_char: EcmaRegex::compile(r"[^ ]", ""),
        new_line_char_global: EcmaRegex::compile(r"\n", "g"),
        tab_char_global: EcmaRegex::compile(r"\t", "g"),
        multiple_space_global: EcmaRegex::compile(r"\s+", "g"),
        blank_line: EcmaRegex::compile(r"^[ \t]*$", ""),
        double_blank_line: EcmaRegex::compile(r"\n[ \t]*\n[ \t]*$", ""),
        blockquote_start: EcmaRegex::compile(r"^ {0,3}>", ""),
        blockquote_setext_replace: EcmaRegex::compile(r"\n {0,3}((?:=+|-+) *)(?=\n|$)", "g"),
        blockquote_setext_replace2: EcmaRegex::compile(r"^ {0,3}>[ \t]?", "gm"),
        list_replace_nesting: EcmaRegex::compile(r"^ {1,4}(?=( {4})*[^ ])", "g"),
        list_is_task: EcmaRegex::compile(r"^\[[ xX]\] +\S", ""),
        list_replace_task: EcmaRegex::compile(r"^\[[ xX]\] +", ""),
        list_task_checkbox: EcmaRegex::compile(r"\[[ xX]\]", ""),
        any_line: EcmaRegex::compile(r"\n.*\n", ""),
        href_brackets: EcmaRegex::compile(r"^<(.*)>$", ""),
        table_delimiter: EcmaRegex::compile(r"[:|]", ""),
        table_align_chars: EcmaRegex::compile(r"^\||\| *$", "g"),
        table_row_blank_line: EcmaRegex::compile(r"\n[ \t]*$", ""),
        table_align_right: EcmaRegex::compile(r"^ *-+: *$", ""),
        table_align_center: EcmaRegex::compile(r"^ *:-+: *$", ""),
        table_align_left: EcmaRegex::compile(r"^ *:-+ *$", ""),
        start_a_tag: EcmaRegex::compile(r"^<a ", "i"),
        end_a_tag: EcmaRegex::compile(r"^</a>", "i"),
        start_pre_script_tag: EcmaRegex::compile(r"^<(pre|code|kbd|script)(\s|>)", "i"),
        end_pre_script_tag: EcmaRegex::compile(r"^</(pre|code|kbd|script)(\s|>)", "i"),
        start_angle_bracket: EcmaRegex::compile(r"^<", ""),
        end_angle_bracket: EcmaRegex::compile(r">$", ""),
        pedantic_href_title: EcmaRegex::compile(r#"^([^'"]*[^\s])\s+(['"])(.*)\2"#, ""),
        unicode_alpha_numeric: EcmaRegex::compile(r"[\p{L}\p{N}]", "u"),
        find_pipe: EcmaRegex::compile(r"\|", "g"),
        slash_pipe: EcmaRegex::compile(r"\\\|", "g"),
        space_line: EcmaRegex::compile(r"^ +$", "gm"),
        next_bullet_cache: make_4(|indent| {
            EcmaRegex::compile(
                &format!(r"^ {{0,{indent}}}(?:[*+-]|\d{{1,9}}[.)])((?:[ \t][^\n]*)?(?:\n|$))"),
                "",
            )
        }),
        hr_cache: make_4(|indent| {
            EcmaRegex::compile(
                &format!(r"^ {{0,{indent}}}((?:- *){{3,}}|(?:_ *){{3,}}|(?:\* *){{3,}})(?:\n+|$)"),
                "",
            )
        }),
        fences_begin_cache: make_4(|indent| EcmaRegex::compile(&format!(r"^ {{0,{indent}}}(?:```|~~~)"), "")),
        heading_begin_cache: make_4(|indent| EcmaRegex::compile(&format!(r"^ {{0,{indent}}}#"), "")),
        html_begin_cache: make_4(|indent| EcmaRegex::compile(&format!(r"^ {{0,{indent}}}<(?:[a-z].*>|!--)"), "i")),
        blockquote_begin_cache: make_4(|indent| EcmaRegex::compile(&format!(r"^ {{0,{indent}}}>"), "")),
        list_item_ast: EcmaRegex::compile(r"^( {0,3}\*)((?:[\t ][^\n]*)?(?:\n|$))", ""),
        list_item_plus: EcmaRegex::compile(r"^( {0,3}\+)((?:[\t ][^\n]*)?(?:\n|$))", ""),
        list_item_minus: EcmaRegex::compile(r"^( {0,3}\-)((?:[\t ][^\n]*)?(?:\n|$))", ""),
        list_item_dot: EcmaRegex::compile(r"^( {0,3}\d{1,9}\.)((?:[\t ][^\n]*)?(?:\n|$))", ""),
        list_item_paren: EcmaRegex::compile(r"^( {0,3}\d{1,9}\))((?:[\t ][^\n]*)?(?:\n|$))", ""),
    };

    // Block patterns
    let newline = r"^(?:[ \t]*(?:\n|$))+";
    let block_code = r"^((?: {4}| {0,3}\t)[^\n]+(?:\n(?:[ \t]*(?:\n|$))*)?)+";
    let fences =
        r"^ {0,3}(`{3,}(?=[^`\n]*(?:\n|$))|~{3,})([^\n]*)(?:\n|$)(?:|([\s\S]*?)(?:\n|$))(?: {0,3}\1[~`]* *(?=\n|$)|$)";
    let hr = r"^ {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)";
    let heading = r"^ {0,3}(#{1,6})(?=\s|$)(.*)(?:\n+|$)";
    let bullet = r" {0,3}(?:[*+-]|\d{1,9}[.)])";
    let lheading_core = r"^(?!bull |blockCode|fences|blockquote|heading|html|table)((?:.|\n(?!\s*?\n|bull |blockCode|fences|blockquote|heading|html|table))+?)\n {0,3}(=+|-+) *(?:\n+|$)";
    let lheading_gfm = RegexEditor::new(lheading_core, "")
        .replace_all("bull", bullet)
        .replace_all("blockCode", r"(?: {4}| {0,3}\t)")
        .replace_all("fences", r" {0,3}(?:`{3,}|~{3,})")
        .replace_all("blockquote", r" {0,3}>")
        .replace_all("heading", r" {0,3}#{1,6}(?:\s|$)")
        .replace_all("html", r" {0,3}<[^\n>]+>\n")
        .replace_all("table", r" {0,3}\|?(?:[:\- ]*\|)+[\:\- ]*\n")
        .build();

    let paragraph_core = r"^([^\n]+(?:\n(?!hr|heading|lheading|blockquote|fences|list|html|table|[ \t]+\n)[^\n]+)*)";
    let block_label = r"(?!\s*\])(?:\\[\s\S]|[^\[\]\\])+";
    let def = RegexEditor::new(
        r"^ {0,3}\[(label)\]: *(?:\n[ \t]*)?([^<\s][^\s]*|<.*?>)(?:(?: +(?:\n[ \t]*)?| *\n[ \t]*)(title))? *(?:\n+|$)",
        "",
    )
    .replace("label", block_label)
    .replace(
        "title",
        r##"(?:"(?:\\"?|[^"\\])*"|'[^'\n]*(?:\n[^'\n]+)*\n?'|\([^()]*\))"##,
    )
    .build();

    let list = RegexEditor::new(r"^(bull)([ \t][^\n]*?)?(?:\n|$)", "")
        .replace_all("bull", bullet)
        .build();

    let tag_names = concat!(
        "address|article|aside|base|basefont|blockquote|body|caption",
        "|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption",
        "|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe",
        "|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option",
        "|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title",
        "|tr|track|ul"
    );
    let comment = r"<!--(?:-?>|[\s\S]*?(?:-->|$))";
    let html_pattern = concat!(
        r"^ {0,3}(?:",
        r"<(script|pre|style|textarea)[\s>][\s\S]*?(?:</\1>[^\n]*\n*|$)",
        r"|comment[^\n]*(\n+|$)",
        r"|<\?[\s\S]*?(?:\?>[^\n]*\n*|$)",
        r"|<![A-Z][\s\S]*?(?:>[^\n]*\n*|$)",
        r"|<!\[CDATA\[[\s\S]*?(?:\]\]>[^\n]*\n*|$)",
        r"|</?(tag)(?: +|\n|/?>)[\s\S]*?(?:(?:\n[ \t]*)+\n|$)",
        r"|<(?!script|pre|style|textarea)([a-z][\w-]*)(?:attribute)*? */?>(?=[ \t]*(?:\n|$))[\s\S]*?(?:(?:\n[ \t]*)+\n|$)",
        r"|</(?!script|pre|style|textarea)[a-z][\w-]*\s*>(?=[ \t]*(?:\n|$))[\s\S]*?(?:(?:\n[ \t]*)+\n|$)",
        r")"
    );
    let html = RegexEditor::new(html_pattern, "i")
        .replace("comment", comment)
        .replace("tag", tag_names)
        .replace(
            "attribute",
            r##" +[a-zA-Z:_][\w.:-]*(?: *= *"[^"\n]*"| *= *'[^'\n]*'| *= *[^\s"'=<>`]+)?"##,
        )
        .build();

    let blockquote_paragraph = RegexEditor::new(paragraph_core, "")
        .replace("hr", hr)
        .replace("heading", r" {0,3}#{1,6}(?:\s|$)")
        .replace("|lheading", "")
        .replace("|table", "")
        .replace("blockquote", r" {0,3}>")
        .replace("fences", r" {0,3}(?:`{3,}(?=[^`\n]*(?:\n|$))|~~~)[^\n]*(?:\n|$)")
        .replace("list", r" {0,3}(?:[*+-]|\d{1,9}[.)])(?:[ \t]|\n|$)")
        .replace("html", r"</?(?:tag)(?: +|\n|/?>)|<(?:script|pre|style|textarea|!--)")
        .replace("tag", tag_names)
        .build();

    let blockquote = RegexEditor::new(r"^( {0,3}> ?(paragraph|[^\n]*)(?:\n|$))+", "")
        .replace("paragraph", blockquote_paragraph.source)
        .build();

    let gfm_table = RegexEditor::new(
        concat!(
            r"^ *([^\n ].*)\n",
            r" {0,3}((?:\| *)?:?-+:? *(?:\| *:?-+:? *)*(?:\| *)?)",
            r"(?:\n((?:(?! *\n|hr|heading|blockquote|code|fences|list|html).*(?:\n|$))*)\n*|$)"
        ),
        "",
    )
    .replace("hr", hr)
    .replace("heading", r" {0,3}#{1,6}(?:\s|$)")
    .replace("blockquote", r" {0,3}>")
    .replace("code", r"(?: {4}| {0,3}\t)[^\n]")
    .replace("fences", r" {0,3}(?:`{3,}(?=[^`\n]*(?:\n|$))|~~~)[^\n]*(?:\n|$)")
    .replace("list", r" {0,3}(?:[*+-]|1[.)])[ \t]")
    .replace("html", r"</?(?:tag)(?: +|\n|/?>)|<(?:script|pre|style|textarea|!--)")
    .replace("tag", tag_names)
    .build();

    let gfm_paragraph = RegexEditor::new(paragraph_core, "")
        .replace("hr", hr)
        .replace("heading", r" {0,3}#{1,6}(?:\s|$)")
        .replace("|lheading", "")
        .replace("table", gfm_table.source)
        .replace("blockquote", r" {0,3}>")
        .replace("fences", r" {0,3}(?:`{3,}(?=[^`\n]*(?:\n|$))|~~~)[^\n]*(?:\n|$)")
        .replace("list", r" {0,3}(?:[*+-]|1[.)])[ \t]+[^ \t\n]")
        .replace("html", r"</?(?:tag)(?: +|\n|/?>)|<(?:script|pre|style|textarea|!--)")
        .replace("tag", tag_names)
        .build();

    let block_gfm = BlockRules {
        blockquote,
        code: EcmaRegex::compile(block_code, ""),
        def,
        fences: EcmaRegex::compile(fences, ""),
        heading: EcmaRegex::compile(heading, ""),
        hr: EcmaRegex::compile(hr, ""),
        html,
        lheading: lheading_gfm,
        list,
        newline: EcmaRegex::compile(newline, ""),
        paragraph: gfm_paragraph,
        table: gfm_table,
        text: EcmaRegex::compile(r"^[^\n]+", ""),
    };

    // Inline patterns
    let punctuation_class = r"[\p{P}\p{S}]";
    let punctuation_or_space_class = r"[\s\p{P}\p{S}]";
    let punctuation_gfm_se = r"(?!~)[\p{P}\p{S}]";
    let punctuation_or_space_gfm_se = r"(?!~)[\s\p{P}\p{S}]";
    let not_punctuation_or_space_gfm_se = r"(?:[^\s\p{P}\p{S}]|~)";
    let not_punctuation_or_space = r"[^\s\p{P}\p{S}]";

    let punctuation = RegexEditor::new(r"^((?![*_])punctSpace)", "u")
        .replace_all("punctSpace", punctuation_or_space_class)
        .build();

    let block_skip = RegexEditor::new("link|precode-code|html", "g")
        .replace(
            "link",
            r"\[(?:[^\[\]`]|(?<a>`+)[^`]+\k<a>(?!`))*?\]\((?:\\[\s\S]|[^\\\(\)]|\((?:\\[\s\S]|[^\\\(\)])*\))*\)",
        )
        .replace("precode-", "(?<!`)()")
        .replace("code", r"(?<b>`+)[^`]+\k<b>(?!`)")
        .replace("html", r"<(?! )[^<>]*?>")
        .build();

    let em_strong_l_delim_core = r"^(?:\*+(?:((?!\*)punct)|([^\s*]))?)|^_+(?:((?!_)punct)|([^\s_]))?";
    let em_strong_l_delim_gfm = RegexEditor::new(em_strong_l_delim_core, "u")
        .replace_all("punct", punctuation_gfm_se)
        .build();

    let em_strong_r_delim_ast_core = concat!(
        r"^[^_*]*?__[^_*]*?\*[^_*]*?(?=__)",
        r"|[^*]+(?=[^*])",
        r"|(?!\*)punct(\*+)(?=[\s]|$)",
        r"|notPunctSpace(\*+)(?!\*)(?=punctSpace|$)",
        r"|(?!\*)punctSpace(\*+)(?=notPunctSpace)",
        r"|[\s](\*+)(?!\*)(?=punct)",
        r"|(?!\*)punct(\*+)(?!\*)(?=punct)",
        r"|notPunctSpace(\*+)(?=notPunctSpace)"
    );
    let em_strong_r_delim_ast_gfm = RegexEditor::new(em_strong_r_delim_ast_core, "gu")
        .replace_all("notPunctSpace", not_punctuation_or_space_gfm_se)
        .replace_all("punctSpace", punctuation_or_space_gfm_se)
        .replace_all("punct", punctuation_gfm_se)
        .build();

    let em_strong_r_delim_und = RegexEditor::new(
        concat!(
            r"^[^_*]*?\*\*[^_*]*?_[^_*]*?(?=\*\*)",
            r"|[^_]+(?=[^_])",
            r"|(?!_)punct(_+)(?=[\s]|$)",
            r"|notPunctSpace(_+)(?!_)(?=punctSpace|$)",
            r"|(?!_)punctSpace(_+)(?=notPunctSpace)",
            r"|[\s](_+)(?!_)(?=punct)",
            r"|(?!_)punct(_+)(?!_)(?=punct)"
        ),
        "gu",
    )
    .replace_all("notPunctSpace", not_punctuation_or_space)
    .replace_all("punctSpace", punctuation_or_space_class)
    .replace_all("punct", punctuation_class)
    .build();

    let del_l_delim = RegexEditor::new(r"^~~?(?:((?!~)punct)|[^\s~])", "u")
        .replace_all("punct", punctuation_class)
        .build();

    let del_r_delim = RegexEditor::new(
        concat!(
            r"^[^~]+(?=[^~])",
            r"|(?!~)punct(~~?)(?=[\s]|$)",
            r"|notPunctSpace(~~?)(?!~)(?=punctSpace|$)",
            r"|(?!~)punctSpace(~~?)(?=notPunctSpace)",
            r"|[\s](~~?)(?!~)(?=punct)",
            r"|(?!~)punct(~~?)(?!~)(?=punct)",
            r"|notPunctSpace(~~?)(?=notPunctSpace)"
        ),
        "gu",
    )
    .replace_all("notPunctSpace", not_punctuation_or_space)
    .replace_all("punctSpace", punctuation_or_space_class)
    .replace_all("punct", punctuation_class)
    .build();

    let any_punctuation = RegexEditor::new(r"\\(punct)", "gu")
        .replace_all("punct", punctuation_class)
        .build();

    let autolink = RegexEditor::new(r"^<(scheme:[^\s\x00-\x1f<>]*|email)>", "")
        .replace("scheme", r"[a-zA-Z][a-zA-Z0-9+.-]{1,31}")
        .replace(
            "email",
            r"[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+(@)[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)+(?![-_])",
        )
        .build();

    let inline_comment = r"<!--(?:-?>|[\s\S]*?-->)";
    let tag = RegexEditor::new(
        concat!(
            r"^comment",
            r"|^</[a-zA-Z][\w:-]*\s*>",
            r"|^<[a-zA-Z][\w-]*(?:attribute)*?\s*/?>",
            r"|^<\?[\s\S]*?\?>",
            r"|^<![a-zA-Z]+\s[\s\S]*?>",
            r"|^<!\[CDATA\[[\s\S]*?\]\]>"
        ),
        "",
    )
    .replace("comment", inline_comment)
    .replace(
        "attribute",
        r##"\s+[a-zA-Z:_][\w.:-]*(?:\s*=\s*"[^"]*"|\s*=\s*'[^']*'|\s*=\s*[^\s"'=<>`]+)?"##,
    )
    .build();

    let inline_label = r"(?:\[(?:\\[\s\S]|[^\[\]\\])*\]|\\[\s\S]|`+(?!`)[^`]*?`+(?!`)|``+(?=\])|[^\[\]\\`])*?";
    let link = RegexEditor::new(
        r"^!?\[(label)\]\(\s*(href)(?:(?:[ \t]+(?:\n[ \t]*)?|\n[ \t]*)(title))?\s*\)",
        "",
    )
    .replace("label", inline_label)
    .replace("href", r"<(?:\\.|[^\n<>\\])+>|[^ \t\n\x00-\x1f]+|(?=\))")
    .replace(
        "title",
        r##""(?:\\"?|[^"\\])*"|'(?:\\'?|[^'\\])*'|\((?:\\\)?|[^)\\])*\)"##,
    )
    .build();

    let reflink = RegexEditor::new(r"^!?\[(label)\]\[(ref)\]", "")
        .replace("label", inline_label)
        .replace("ref", block_label)
        .build();

    let nolink = RegexEditor::new(r"^!?\[(ref)\](?:\[\])?", "")
        .replace("ref", block_label)
        .build();

    let reflink_search = RegexEditor::new(r"reflink|nolink(?!\()", "g")
        .replace("reflink", reflink.source)
        .replace("nolink", nolink.source)
        .build();

    let case_insensitive_protocol = r"[hH][tT][tT][pP][sS]?|[fF][tT][pP]";
    let gfm_url = RegexEditor::new(r"^((?:protocol):\/\/|www\.)(?:[a-zA-Z0-9\-]+\.?)+[^\s<]*|^email", "")
        .replace("protocol", case_insensitive_protocol)
        .replace(
            "email",
            r"[A-Za-z0-9._+-]+(@)[a-zA-Z0-9-_]+(?:\.[a-zA-Z0-9-_]*[a-zA-Z0-9])+(?![-_])",
        )
        .build();

    let gfm_text = RegexEditor::new(
        r"^(`+|~+|[^`~])(?:(?=[`~])|(?= {2,}\n)|(?=[a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-]+@)|[\s\S]*?(?:(?=[\\<!\[`*~_]|\b_|protocol:\/\/|www\.|$)|[^ ](?= {2,}\n)|[^a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-](?=[a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-]+@)))",
        "",
    )
    .replace("protocol", case_insensitive_protocol)
    .build();

    let inline_gfm = InlineRules {
        _backpedal: EcmaRegex::compile(
            r#"(?:[^?!.,:;*_'"~()&]+|\([^)]*\)|&(?![a-zA-Z0-9]+;$)|[?!.,:;*_'"~)]+(?!$))+"#,
            "",
        ),
        any_punctuation,
        autolink,
        block_skip,
        br: EcmaRegex::compile(r"^( {2,}|\\)\n(?!\s*$)", ""),
        code: EcmaRegex::compile(r"^(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)", ""),
        del: EcmaRegex::compile(
            r"^(~~?)(?=[^\s~])((?:\\[\s\S]|[^\\])*?(?:\\[\s\S]|[^\s~\\]))\1(?=[^~]|$)",
            "",
        ),
        del_l_delim,
        del_r_delim,
        em_strong_l_delim: em_strong_l_delim_gfm,
        em_strong_r_delim_ast: em_strong_r_delim_ast_gfm,
        em_strong_r_delim_und,
        escape: EcmaRegex::compile(r##"^\\([!"#$%&'()*+,\-./:;<=>?@\[\]\\^_`{|}~])"##, ""),
        link,
        nolink,
        punctuation,
        reflink,
        reflink_search,
        tag,
        text: gfm_text,
        url: gfm_url,
    };

    GrammarSet {
        other,
        block_gfm,
        inline_gfm,
    }
});

// =============================================================================
// Helpers (port of marked/src/helpers.ts & marked/src/Tokenizer.ts)
// =============================================================================

fn rtrim(s: &str, c: char) -> &str {
    let mut end = s.len();
    for (idx, ch) in s.char_indices().rev() {
        if ch == c {
            end = idx;
        } else {
            break;
        }
    }
    &s[..end]
}

fn trim_trailing_blank_lines<'a>(s: &'a str, blank_line: &EcmaRegex) -> &'a str {
    let lines: Vec<&str> = s.split('\n').collect();
    let mut end = lines.len() as isize - 1;
    while end >= 0 && blank_line.test(lines[end as usize]) {
        end -= 1;
    }
    if (lines.len() as isize - end) <= 2 {
        return s;
    }
    let keep = (end + 1) as usize;
    if keep == 0 {
        return "";
    }
    let byte_len: usize = lines[..keep].iter().map(|l| l.len()).sum::<usize>() + (keep - 1);
    &s[..byte_len]
}

fn expand_tabs(line: &str, indent: usize) -> String {
    let mut col = indent;
    let mut expanded = String::with_capacity(line.len());
    for ch in line.chars() {
        if ch == '\t' {
            let added = 4 - (col % 4);
            for _ in 0..added {
                expanded.push(' ');
            }
            col += added;
        } else {
            expanded.push(ch);
            col += 1;
        }
    }
    expanded
}

fn split_cells(table_row: &str, count: Option<usize>, rules: &GrammarSet) -> Vec<String> {
    let row = rules.other.find_pipe.replace_with(table_row, |m| {
        let mut escaped = false;
        let bytes = table_row.as_bytes();
        let mut curr = m.byte_start;
        while curr > 0 {
            curr -= 1;
            if bytes[curr] == b'\\' {
                escaped = !escaped;
            } else {
                break;
            }
        }
        if escaped { "|".to_string() } else { " |".to_string() }
    });

    let mut cells: Vec<String> = row.split(" |").map(|s| s.to_string()).collect();
    if cells.first().is_some_and(|c| pi_js::str16::trim(c).is_empty()) {
        cells.remove(0);
    }
    if !cells.is_empty() && cells.last().is_some_and(|c| pi_js::str16::trim(c).is_empty()) {
        cells.pop();
    }

    if let Some(target) = count.filter(|&c| c > 0) {
        if cells.len() > target {
            cells.truncate(target);
        } else {
            while cells.len() < target {
                cells.push(String::new());
            }
        }
    }

    for cell in &mut cells {
        let trimmed = pi_js::str16::trim(cell);
        *cell = rules.other.slash_pipe.replace(trimmed, "|");
    }
    cells
}

/// Returns closing bracket UTF-16 index, `-1` if none closed cleanly, `-2` if unclosed `(`.
fn find_closing_bracket(s: &str, open: u16, close: u16) -> i64 {
    let units: Vec<u16> = s.encode_utf16().collect();
    if !units.contains(&close) {
        return -1;
    }
    let mut level: i64 = 0;
    let mut i = 0;
    while i < units.len() {
        if units[i] == b'\\' as u16 {
            i += 1;
        } else if units[i] == open {
            level += 1;
        } else if units[i] == close {
            level -= 1;
            if level < 0 {
                return i as i64;
            }
        }
        i += 1;
    }
    if level > 0 { -2 } else { -1 }
}

fn indent_code_compensation(raw: &str, text: &str, rules: &GrammarSet) -> String {
    let Some(m) = rules.other.indent_code_compensation.exec(raw) else {
        return text.to_string();
    };
    let indent_to_code_len = pi_js::str16::len(m.captures.first().and_then(|c| c.as_deref()).unwrap_or(""));
    text.split('\n')
        .map(|node| {
            let Some(nm) = rules.other.beginning_space.exec(node) else {
                return node.to_string();
            };
            if pi_js::str16::len(&nm.matched) >= indent_to_code_len {
                pi_js::str16::slice(node, indent_to_code_len as i64, None)
            } else {
                node.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn advance_str<'a>(src: &'a str, raw: &str) -> &'a str {
    if src.starts_with(raw) {
        &src[raw.len()..]
    } else {
        let byte = pi_js::str16::utf16_to_byte(src, pi_js::str16::len(raw));
        &src[byte..]
    }
}

fn advance_past_line<'a>(src: &'a str, line_byte_len: usize) -> &'a str {
    if src.as_bytes().get(line_byte_len) == Some(&b'\n') {
        &src[line_byte_len + 1..]
    } else {
        ""
    }
}

fn non_space_char_u16(line: &str, re: &EcmaRegex) -> isize {
    re.exec(line).map_or(-1, |m| m.index as isize)
}

fn last_char_for_prev_char(raw: &str) -> String {
    match raw.chars().next_back() {
        None => String::new(),
        Some('_') => String::new(),
        Some(c) if c.len_utf16() == 1 => c.to_string(),
        // Astral char: JS `slice(-1)` yields the lone low surrogate, which matches neither `\p{L}\p{N}`
        // nor punctuation while remaining non-empty (`!prevChar` is false). Use U+0007 (Cc) to preserve both.
        Some(_) => "\u{0007}".to_string(),
    }
}

// =============================================================================
// Lexer & Inline Queue Slot Resolution
// =============================================================================

const INLINE_SLOT_PREFIX: &str = "\0__pi_marked_slot:";

struct LexerState {
    in_link: bool,
    in_raw_block: bool,
    /// a link was produced in the inline run currently being scanned
    link_emitted: bool,
    top: bool,
}

struct InlineQueueEntry {
    id: usize,
    src: String,
}

struct CompiledExtensions {
    block: Vec<TokenizerExtensionFunction>,
    inline: Vec<TokenizerExtensionFunction>,
    start_block: Vec<TokenizerStartFunction>,
    start_inline: Vec<TokenizerStartFunction>,
}

struct LexerInner {
    tokens: Mutex<TokensList>,
    options: Mutex<MarkedOptions>,
    state: Mutex<LexerState>,
    inline_queue: Mutex<Vec<InlineQueueEntry>>,
    next_inline_id: Mutex<usize>,
    extensions: Mutex<CompiledExtensions>,
}

/// marked `Lexer`. `markdown.ts` only calls [`Lexer::inline_tokens`].
#[derive(Clone)]
pub struct Lexer {
    inner: Arc<LexerInner>,
}

impl Lexer {
    pub fn new() -> Self {
        Self::with_config(MarkedOptions::default(), &[])
    }

    fn with_config(mut options: MarkedOptions, extensions: &[TokenizerExtension]) -> Self {
        if options.tokenizer.is_none() {
            options.tokenizer = Some(Arc::new(Tokenizer::new()));
        }
        let mut block = Vec::new();
        let mut inline = Vec::new();
        let mut start_block = Vec::new();
        let mut start_inline = Vec::new();
        for ext in extensions {
            match ext.level {
                TokenizerLevel::Block => {
                    block.insert(0, Arc::clone(&ext.tokenizer));
                    if let Some(start) = &ext.start {
                        start_block.push(Arc::clone(start));
                    }
                }
                TokenizerLevel::Inline => {
                    inline.insert(0, Arc::clone(&ext.tokenizer));
                    if let Some(start) = &ext.start {
                        start_inline.push(Arc::clone(start));
                    }
                }
            }
        }
        let lexer = Self {
            inner: Arc::new(LexerInner {
                tokens: Mutex::new(TokensList {
                    tokens: Vec::new(),
                    links: IndexMap::new(),
                }),
                options: Mutex::new(options.clone()),
                state: Mutex::new(LexerState {
                    in_link: false,
                    in_raw_block: false,
                    link_emitted: false,
                    top: true,
                }),
                inline_queue: Mutex::new(Vec::new()),
                next_inline_id: Mutex::new(0),
                extensions: Mutex::new(CompiledExtensions {
                    block,
                    inline,
                    start_block,
                    start_inline,
                }),
            }),
        };
        if let Some(tok) = &options.tokenizer {
            tok.set_lexer(lexer.clone());
        }
        lexer
    }

    fn active_tokenizer(&self) -> Arc<dyn MarkedTokenizer> {
        let tok = self
            .inner
            .options
            .lock()
            .expect("lexer options lock")
            .tokenizer
            .clone()
            .unwrap_or_else(|| Arc::new(Tokenizer::new()));
        tok.set_lexer(self.clone());
        tok
    }

    fn lex(&self, src: &str) -> TokensList {
        let normalized = src.replace("\r\n", "\n").replace('\r', "\n");

        let mut top_tokens = Vec::new();
        self.block_tokens(&normalized, &mut top_tokens, false);

        let queue = std::mem::take(&mut *self.inner.inline_queue.lock().expect("inline_queue lock"));
        let mut resolved: IndexMap<usize, Vec<Token>> = IndexMap::new();
        for entry in queue {
            let inline = self.inline_tokens(&entry.src);
            resolved.insert(entry.id, inline);
        }
        resolve_inline_slots(&mut top_tokens, &mut resolved);

        let mut out = self.inner.tokens.lock().expect("lexer tokens lock");
        out.tokens = top_tokens;
        out.clone()
    }

    fn inline(&self, src: &str) -> Vec<Token> {
        let id = {
            let mut next = self.inner.next_inline_id.lock().expect("next_inline_id lock");
            let id = *next;
            *next += 1;
            id
        };
        self.inner
            .inline_queue
            .lock()
            .expect("inline_queue lock")
            .push(InlineQueueEntry {
                id,
                src: src.to_string(),
            });
        vec![Token::Space(Tokens::Space {
            raw: format!("{INLINE_SLOT_PREFIX}{id}"),
        })]
    }

    fn block_tokens(&self, mut src: &str, tokens: &mut Vec<Token>, mut last_paragraph_clipped: bool) {
        let _ = self.active_tokenizer();
        let rules = &*GRAMMAR;
        let (ext_block, start_block) = {
            let exts = self.inner.extensions.lock().expect("extensions lock");
            (exts.block.clone(), exts.start_block.clone())
        };

        let mut src_len = usize::MAX;
        while !src.is_empty() {
            let cur_len = src.len();
            if cur_len < src_len {
                src_len = cur_len;
            } else {
                break;
            }

            let mut matched_ext = false;
            if !ext_block.is_empty() {
                let this = TokenizerThis { lexer: self.clone() };
                for ext_fn in &ext_block {
                    if let Some(tok) = ext_fn(&this, src, tokens) {
                        if tok.raw.is_empty() {
                            break;
                        }
                        src = advance_str(src, &tok.raw);
                        tokens.push(Token::Generic(tok));
                        matched_ext = true;
                        break;
                    }
                }
            }
            if matched_ext {
                continue;
            }

            // newline
            if let Some(tok) = stock_space(src, rules) {
                src = advance_str(src, &tok.raw);
                if pi_js::str16::len(&tok.raw) == 1
                    && let Some(last_token) = tokens.last_mut()
                    && let Some(raw_mut) = last_token.raw_mut()
                {
                    raw_mut.push('\n');
                } else {
                    tokens.push(Token::Space(tok));
                }
                continue;
            }

            // code
            if let Some(tok) = stock_code(src, rules) {
                src = advance_str(src, &tok.raw);
                let merged = match tokens.last_mut() {
                    Some(Token::Paragraph(p)) => {
                        if !p.raw.ends_with('\n') {
                            p.raw.push('\n');
                        }
                        p.raw.push_str(&tok.raw);
                        p.text.push('\n');
                        p.text.push_str(&tok.text);
                        if let Some(last_q) = self.inner.inline_queue.lock().expect("inline_queue lock").last_mut() {
                            last_q.src = p.text.clone();
                        }
                        true
                    }
                    Some(Token::Text(t)) => {
                        if !t.raw.ends_with('\n') {
                            t.raw.push('\n');
                        }
                        t.raw.push_str(&tok.raw);
                        t.text.push('\n');
                        t.text.push_str(&tok.text);
                        if let Some(last_q) = self.inner.inline_queue.lock().expect("inline_queue lock").last_mut() {
                            last_q.src = t.text.clone();
                        }
                        true
                    }
                    _ => false,
                };
                if !merged {
                    tokens.push(Token::Code(tok));
                }
                continue;
            }

            // fences
            if let Some(tok) = stock_fences(src, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Code(tok));
                continue;
            }

            // heading
            if let Some(tok) = stock_heading(src, self, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Heading(tok));
                continue;
            }

            // hr
            if let Some(tok) = stock_hr(src, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Hr(tok));
                continue;
            }

            // blockquote
            if let Some(tok) = stock_blockquote(src, self, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Blockquote(tok));
                continue;
            }

            // list
            if let Some(tok) = stock_list(src, self, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::List(tok));
                continue;
            }

            // html
            if let Some(tok) = stock_html(src, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Html(tok));
                continue;
            }

            // def
            if let Some(tok) = stock_def(src, rules) {
                src = advance_str(src, &tok.raw);
                let merged = match tokens.last_mut() {
                    Some(Token::Paragraph(p)) => {
                        if !p.raw.ends_with('\n') {
                            p.raw.push('\n');
                        }
                        p.raw.push_str(&tok.raw);
                        p.text.push('\n');
                        p.text.push_str(&tok.raw);
                        if let Some(last_q) = self.inner.inline_queue.lock().expect("inline_queue lock").last_mut() {
                            last_q.src = p.text.clone();
                        }
                        true
                    }
                    Some(Token::Text(t)) => {
                        if !t.raw.ends_with('\n') {
                            t.raw.push('\n');
                        }
                        t.raw.push_str(&tok.raw);
                        t.text.push('\n');
                        t.text.push_str(&tok.raw);
                        if let Some(last_q) = self.inner.inline_queue.lock().expect("inline_queue lock").last_mut() {
                            last_q.src = t.text.clone();
                        }
                        true
                    }
                    _ => false,
                };
                if !merged {
                    let mut links_guard = self.inner.tokens.lock().expect("lexer tokens lock");
                    if !links_guard.links.contains_key(&tok.tag) {
                        links_guard.links.insert(
                            tok.tag.clone(),
                            LinkLabel {
                                href: tok.href.clone(),
                                title: if tok.title.is_empty() {
                                    None
                                } else {
                                    Some(Some(tok.title.clone()))
                                },
                            },
                        );
                        drop(links_guard);
                        tokens.push(Token::Def(tok));
                    }
                }
                continue;
            }

            // table (gfm)
            if let Some(tok) = stock_table(src, self, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Table(tok));
                continue;
            }

            // lheading
            if let Some(tok) = stock_lheading(src, self, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Heading(tok));
                continue;
            }

            // top-level paragraph
            let cut_src = clip_to_extension_start(src, &start_block, self);
            let was_clipped = cut_src.len() != src.len();
            let is_top = self.inner.state.lock().expect("lexer state lock").top;
            if is_top && let Some(tok) = stock_paragraph(cut_src, self, rules) {
                src = advance_str(src, &tok.raw);
                let mut did_merge = false;
                if last_paragraph_clipped && let Some(Token::Paragraph(last_p)) = tokens.last_mut() {
                    if !last_p.raw.ends_with('\n') {
                        last_p.raw.push('\n');
                    }
                    last_p.raw.push_str(&tok.raw);
                    last_p.text.push('\n');
                    last_p.text.push_str(&tok.text);
                    let mut q = self.inner.inline_queue.lock().expect("inline_queue lock");
                    q.pop();
                    if let Some(prev_q) = q.last_mut() {
                        prev_q.src = last_p.text.clone();
                    }
                    did_merge = true;
                }
                if !did_merge {
                    tokens.push(Token::Paragraph(tok));
                }
                last_paragraph_clipped = was_clipped;
                continue;
            }

            // text
            if let Some(tok) = stock_block_text(src, self, rules) {
                src = advance_str(src, &tok.raw);
                if let Some(Token::Text(last_t)) = tokens.last_mut() {
                    if !last_t.raw.ends_with('\n') {
                        last_t.raw.push('\n');
                    }
                    last_t.raw.push_str(&tok.raw);
                    last_t.text.push('\n');
                    last_t.text.push_str(&tok.text);
                    let mut q = self.inner.inline_queue.lock().expect("inline_queue lock");
                    q.pop();
                    if let Some(prev_q) = q.last_mut() {
                        prev_q.src = last_t.text.clone();
                    }
                } else {
                    tokens.push(Token::Text(tok));
                }
                continue;
            }

            break;
        }

        self.inner.state.lock().expect("lexer state lock").top = true;
    }

    fn link_in_text(&self, text: &str, links: &Links, rules: &GrammarSet) -> bool {
        if !text.contains('[') {
            return false;
        }
        for m in rules.inline_gfm.block_skip.match_all(text) {
            if rules.inline_gfm.link.test(&m.matched) {
                let prev_is_bang = m.byte_start > 0 && text.as_bytes()[m.byte_start - 1] == b'!';
                if !prev_is_bang {
                    return true;
                }
            }
        }
        for m in rules.inline_gfm.reflink_search.match_all(text) {
            let match0 = &m.matched;
            if let Some(ref_start_byte) = match0.rfind('[') {
                let key = &match0[ref_start_byte + 1..match0.len().saturating_sub(1)];
                if match0.starts_with('!') || !links.contains_key(key) {
                    continue;
                }
                if ref_start_byte > 1 && self.link_in_text(&match0[1..ref_start_byte - 1], links, rules) {
                    continue;
                }
                return true;
            }
        }
        false
    }

    fn mask_reflinks(&self, src: &str, links: &Links, rules: &GrammarSet) -> String {
        rules.inline_gfm.reflink_search.replace_with(src, |m| {
            let match0 = &m.matched;
            let Some(ref_start_byte) = match0.rfind('[') else {
                return match0.clone();
            };
            let key = &match0[ref_start_byte + 1..match0.len().saturating_sub(1)];
            if !links.contains_key(key) {
                return match0.clone();
            }
            let match0_u16 = pi_js::str16::len(match0);
            let ref_start_u16 = pi_js::str16::len(&match0[..ref_start_byte]);
            if ref_start_byte > 1 && !match0.starts_with('!') {
                let inner_text = &match0[1..ref_start_byte - 1];
                if self.link_in_text(inner_text, links, rules) {
                    return format!(
                        "[{}][{}]",
                        self.mask_reflinks(inner_text, links, rules),
                        "a".repeat(match0_u16.saturating_sub(ref_start_u16).saturating_sub(2))
                    );
                }
            }
            format!("[{}]", "a".repeat(match0_u16.saturating_sub(2)))
        })
    }

    /// `inlineTokens(src)`.
    ///
    /// PORT: the optional `tokens` out-array is not declared. The strikethrough override calls the one-arg form.
    pub fn inline_tokens(&self, mut src: &str) -> Vec<Token> {
        let tokenizer = self.active_tokenizer();
        let rules = &*GRAMMAR;
        let links = self.inner.tokens.lock().expect("lexer tokens lock").links.clone();
        let (ext_inline, start_inline) = {
            let exts = self.inner.extensions.lock().expect("extensions lock");
            (exts.inline.clone(), exts.start_inline.clone())
        };

        let mut masked_src = if !links.is_empty() && src.contains('[') {
            self.mask_reflinks(src, &links, rules)
        } else {
            src.to_string()
        };

        // Mask out escaped characters (keep UTF-16 length exact).
        masked_src = rules
            .inline_gfm
            .any_punctuation
            .replace_with(&masked_src, |m| "+".repeat(pi_js::str16::len(&m.matched)));

        // Mask out other blocks (links, code spans, inline HTML tags).
        masked_src = rules.inline_gfm.block_skip.replace_with(&masked_src, |m| {
            let context_u16 = m
                .captures
                .get(1)
                .and_then(|c| c.as_deref())
                .map_or(0, pi_js::str16::len);
            let total_u16 = pi_js::str16::len(&m.matched);
            let prefix = pi_js::str16::slice(&m.matched, 0, Some(context_u16 as i64));
            format!(
                "{prefix}[{}]",
                "a".repeat(total_u16.saturating_sub(context_u16).saturating_sub(2))
            )
        });

        let mut tokens = Vec::new();
        let mut keep_prev_char = false;
        let mut prev_char = String::new();
        let mut src_len = usize::MAX;

        while !src.is_empty() {
            if src.len() < src_len {
                src_len = src.len();
            } else {
                break;
            }

            if !keep_prev_char {
                prev_char.clear();
            }
            keep_prev_char = false;

            // extensions
            let mut matched_ext = false;
            if !ext_inline.is_empty() {
                let this = TokenizerThis { lexer: self.clone() };
                for ext_fn in &ext_inline {
                    if let Some(tok) = ext_fn(&this, src, &mut tokens) {
                        if tok.raw.is_empty() {
                            break;
                        }
                        src = advance_str(src, &tok.raw);
                        tokens.push(Token::Generic(tok));
                        matched_ext = true;
                        break;
                    }
                }
            }
            if matched_ext {
                continue;
            }

            // escape
            if let Some(tok) = stock_escape(src, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Escape(tok));
                continue;
            }

            // tag
            if let Some(tok) = stock_tag(src, self, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Tag(tok));
                continue;
            }

            // link
            if let Some(tok) = stock_link(src, self, rules) {
                src = advance_str(src, tok.raw());
                tokens.push(tok);
                continue;
            }

            // reflink, nolink
            if let Some(tok) = stock_reflink(src, &links, self, rules) {
                src = advance_str(src, tok.raw());
                if let Token::Text(new_t) = tok {
                    if let Some(Token::Text(last_t)) = tokens.last_mut() {
                        last_t.raw.push_str(&new_t.raw);
                        last_t.text.push_str(&new_t.text);
                    } else {
                        tokens.push(Token::Text(new_t));
                    }
                } else {
                    tokens.push(tok);
                }
                continue;
            }

            // em & strong
            if let Some(tok) = stock_em_strong(src, &masked_src, &prev_char, self, rules) {
                src = advance_str(src, tok.raw());
                tokens.push(tok);
                continue;
            }

            // code
            if let Some(tok) = stock_codespan(src, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Codespan(tok));
                continue;
            }

            // br
            if let Some(tok) = stock_br(src, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Br(tok));
                continue;
            }

            // del (gfm)
            let prev_opt = (!prev_char.is_empty()).then_some(prev_char.as_str());
            if let Some(tok) = tokenizer.del(src, &masked_src, prev_opt) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Del(tok));
                continue;
            }

            // autolink
            if let Some(tok) = stock_autolink(src, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Link(tok));
                continue;
            }

            // url (gfm)
            let in_link = self.inner.state.lock().expect("lexer state lock").in_link;
            if !in_link && let Some(tok) = stock_url(src, rules) {
                src = advance_str(src, &tok.raw);
                tokens.push(Token::Link(tok));
                continue;
            }

            // text
            let cut_src = clip_to_extension_start(src, &start_inline, self);
            if let Some(tok) = stock_inline_text(cut_src, self, rules) {
                src = advance_str(src, &tok.raw);
                prev_char = last_char_for_prev_char(&tok.raw);
                keep_prev_char = true;
                if let Some(Token::Text(last_t)) = tokens.last_mut() {
                    last_t.raw.push_str(&tok.raw);
                    last_t.text.push_str(&tok.text);
                } else {
                    tokens.push(Token::Text(tok));
                }
                continue;
            }

            break;
        }

        tokens
    }
}

fn clip_to_extension_start<'a>(src: &'a str, start_fns: &[TokenizerStartFunction], lexer: &Lexer) -> &'a str {
    if start_fns.is_empty() {
        return src;
    }
    let temp_src = pi_js::str16::slice(src, 1, None);
    let this = TokenizerThis { lexer: lexer.clone() };
    let mut min_start: Option<i64> = None;
    for get_start in start_fns {
        if let Some(idx) = get_start(&this, &temp_src)
            && idx >= 0
        {
            min_start = Some(min_start.map_or(idx, |m| m.min(idx)));
        }
    }
    if let Some(start_index) = min_start {
        let byte_end = pi_js::str16::utf16_to_byte(src, (start_index + 1) as usize);
        &src[..byte_end]
    } else {
        src
    }
}

fn resolve_inline_slots(tokens: &mut Vec<Token>, resolved: &mut IndexMap<usize, Vec<Token>>) {
    let take_slot = |tok: &Token| -> Option<usize> {
        if let Token::Space(s) = tok
            && let Some(id_str) = s.raw.strip_prefix(INLINE_SLOT_PREFIX)
        {
            return id_str.parse::<usize>().ok();
        }
        None
    };

    if tokens
        .last()
        .and_then(take_slot)
        .is_some_and(|id| resolved.contains_key(&id))
    {
        let id = take_slot(tokens.last().expect("checked above")).expect("checked above");
        tokens.pop();
        if let Some(inlines) = resolved.shift_remove(&id) {
            tokens.extend(inlines);
        }
        return;
    }

    for token in tokens.iter_mut() {
        match token {
            Token::Blockquote(b) => resolve_inline_slots(&mut b.tokens, resolved),
            Token::Heading(h) => resolve_inline_slots(&mut h.tokens, resolved),
            Token::Paragraph(p) => resolve_inline_slots(&mut p.tokens, resolved),
            Token::Text(t) => {
                if let Some(inner) = &mut t.tokens {
                    resolve_inline_slots(inner, resolved);
                }
            }
            Token::List(list) => {
                for item in &mut list.items {
                    resolve_inline_slots(&mut item.tokens, resolved);
                }
            }
            Token::ListItem(item) => resolve_inline_slots(&mut item.tokens, resolved),
            Token::Table(table) => {
                for cell in &mut table.header {
                    resolve_inline_slots(&mut cell.tokens, resolved);
                }
                for row in &mut table.rows {
                    for cell in row {
                        resolve_inline_slots(&mut cell.tokens, resolved);
                    }
                }
            }
            Token::Del(d) => resolve_inline_slots(&mut d.tokens, resolved),
            Token::Em(e) => resolve_inline_slots(&mut e.tokens, resolved),
            Token::Strong(s) => resolve_inline_slots(&mut s.tokens, resolved),
            Token::Link(l) => resolve_inline_slots(&mut l.tokens, resolved),
            Token::Image(i) => resolve_inline_slots(&mut i.tokens, resolved),
            Token::Generic(g) => {
                if let Some(inner) = &mut g.tokens {
                    resolve_inline_slots(inner, resolved);
                }
            }
            Token::Br(_)
            | Token::Checkbox(_)
            | Token::Code(_)
            | Token::Codespan(_)
            | Token::Def(_)
            | Token::Escape(_)
            | Token::Hr(_)
            | Token::Html(_)
            | Token::Space(_)
            | Token::Tag(_) => {}
        }
    }
}

// =============================================================================
// Stock Tokenizer functions (port of marked/src/Tokenizer.ts)
// =============================================================================

fn stock_space(src: &str, rules: &GrammarSet) -> Option<Tokens::Space> {
    let cap = rules.block_gfm.newline.exec(src)?;
    if !cap.matched.is_empty() {
        Some(Tokens::Space { raw: cap.matched })
    } else {
        None
    }
}

fn stock_code(src: &str, rules: &GrammarSet) -> Option<Tokens::Code> {
    let cap = rules.block_gfm.code.exec(src)?;
    let raw = trim_trailing_blank_lines(&cap.matched, &rules.other.blank_line).to_string();
    let text = rules.other.code_remove_indent.replace(&raw, "");
    Some(Tokens::Code {
        raw,
        code_block_style: Some(Tokens::CodeBlockStyle::Indented),
        lang: None,
        text,
        escaped: None,
    })
}

fn stock_fences(src: &str, rules: &GrammarSet) -> Option<Tokens::Code> {
    let cap = rules.block_gfm.fences.exec(src)?;
    let raw = cap.matched;
    let info = cap.captures.get(1).and_then(|c| c.as_deref());
    let body = cap.captures.get(2).and_then(|c| c.as_deref()).unwrap_or("");
    let text = indent_code_compensation(&raw, body, rules);
    let lang = info.map(|s| {
        if s.is_empty() {
            String::new()
        } else {
            let trimmed = pi_js::str16::trim(s);
            rules.inline_gfm.any_punctuation.replace(trimmed, "$1")
        }
    });
    Some(Tokens::Code {
        raw,
        code_block_style: None,
        lang,
        text,
        escaped: None,
    })
}

fn stock_heading(src: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Tokens::Heading> {
    let cap = rules.block_gfm.heading.exec(src)?;
    let hashes = cap.captures.first().and_then(|c| c.as_deref()).unwrap_or("");
    let raw_text = cap.captures.get(1).and_then(|c| c.as_deref()).unwrap_or("");
    let mut text = pi_js::str16::trim(raw_text).to_string();
    if rules.other.ending_hash.test(&text) {
        let trimmed = rtrim(&text, '#');
        if trimmed.is_empty() || rules.other.ending_space_char.test(trimmed) {
            text = pi_js::str16::trim(trimmed).to_string();
        }
    }
    let tokens = lexer.inline(&text);
    Some(Tokens::Heading {
        raw: rtrim(&cap.matched, '\n').to_string(),
        depth: pi_js::str16::len(hashes) as i64,
        text,
        tokens,
    })
}

fn stock_hr(src: &str, rules: &GrammarSet) -> Option<Tokens::Hr> {
    let cap = rules.block_gfm.hr.exec(src)?;
    Some(Tokens::Hr {
        raw: rtrim(&cap.matched, '\n').to_string(),
    })
}

fn stock_blockquote(src: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Tokens::Blockquote> {
    let cap = rules.block_gfm.blockquote.exec(src)?;
    let mut lines: Vec<String> = rtrim(&cap.matched, '\n').split('\n').map(str::to_string).collect();
    let mut raw = String::new();
    let mut text = String::new();
    let mut tokens: Vec<Token> = Vec::new();

    while !lines.is_empty() {
        let mut in_blockquote = false;
        let mut current_lines = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            if rules.other.blockquote_start.test(&lines[i]) {
                current_lines.push(lines[i].clone());
                in_blockquote = true;
            } else if !in_blockquote {
                current_lines.push(lines[i].clone());
            } else {
                break;
            }
            i += 1;
        }
        lines = lines.split_off(i);

        let current_raw = current_lines.join("\n");
        let after_setext = rules.other.blockquote_setext_replace.replace(&current_raw, "\n    $1");
        let current_text = rules.other.blockquote_setext_replace2.replace(&after_setext, "");

        if raw.is_empty() {
            raw = current_raw;
        } else {
            raw.push('\n');
            raw.push_str(&current_raw);
        }
        if text.is_empty() {
            text = current_text.clone();
        } else {
            text.push('\n');
            text.push_str(&current_text);
        }

        let top = {
            let mut st = lexer.inner.state.lock().expect("lexer state lock");
            let prev = st.top;
            st.top = true;
            prev
        };
        lexer.block_tokens(&current_text, &mut tokens, true);
        lexer.inner.state.lock().expect("lexer state lock").top = top;

        if lines.is_empty() {
            break;
        }

        match tokens.last_mut() {
            Some(Token::Code(_)) => break,
            Some(Token::Blockquote(old_token)) => {
                let continuation = lines.join("\n");
                let stripped = rules.other.blockquote_setext_replace2.replace(&continuation, "");
                let new_text = format!("{}\n{stripped}", old_token.raw);
                if let Some(new_token) = stock_blockquote(&new_text, lexer, rules) {
                    let old_text_u16 = pi_js::str16::len(&old_token.text);
                    raw = format!("{raw}\n{continuation}");
                    let prefix_u16 = pi_js::str16::len(&text).saturating_sub(old_text_u16);
                    text = format!(
                        "{}{}",
                        pi_js::str16::substring(&text, 0, Some(prefix_u16)),
                        new_token.text
                    );
                    *old_token = new_token;
                }
                break;
            }
            Some(Token::List(old_token)) => {
                let new_text = format!("{}\n{}", old_token.raw, lines.join("\n"));
                if let Some(new_token) = stock_list(&new_text, lexer, rules) {
                    let old_raw_u16 = pi_js::str16::len(&old_token.raw);
                    let raw_prefix_u16 = pi_js::str16::len(&raw).saturating_sub(old_raw_u16);
                    raw = format!(
                        "{}{}",
                        pi_js::str16::substring(&raw, 0, Some(raw_prefix_u16)),
                        new_token.raw
                    );
                    let text_prefix_u16 = pi_js::str16::len(&text).saturating_sub(old_raw_u16);
                    text = format!(
                        "{}{}",
                        pi_js::str16::substring(&text, 0, Some(text_prefix_u16)),
                        new_token.raw
                    );
                    let consumed_byte = pi_js::str16::utf16_to_byte(&new_text, pi_js::str16::len(&new_token.raw));
                    lines = new_text[consumed_byte..].split('\n').map(str::to_string).collect();
                    *old_token = new_token;
                    continue;
                }
                break;
            }
            _ => {}
        }
    }

    Some(Tokens::Blockquote { raw, text, tokens })
}

fn stock_list(mut src: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Tokens::List> {
    let cap = rules.block_gfm.list.exec(src)?;
    let mut bull = pi_js::str16::trim(cap.captures.first().and_then(|c| c.as_deref()).unwrap_or("")).to_string();
    let is_ordered = bull.len() > 1;
    let start = if is_ordered {
        let num_str = &bull[..bull.len() - 1];
        Tokens::ListStart::Number(num_str.parse::<i64>().unwrap_or(1))
    } else {
        Tokens::ListStart::Empty
    };
    let mut list = Tokens::List {
        raw: String::new(),
        ordered: is_ordered,
        start,
        loose: false,
        items: Vec::new(),
    };

    bull = if is_ordered {
        let sep = bull.chars().next_back().unwrap_or('.');
        format!(r"\d{{1,9}}\{sep}")
    } else {
        format!(r"\{bull}")
    };

    let item_regex = rules.other.list_item_regex(&bull);
    let mut ends_with_blank_line = false;

    while !src.is_empty() {
        let mut end_early = false;
        let mut item_contents = String::new();
        let Some(item_cap) = item_regex.exec(src) else {
            break;
        };
        if rules.block_gfm.hr.test(src) {
            break;
        }

        let mut raw = item_cap.matched;
        src = advance_str(src, &raw);

        let cap1 = item_cap.captures.first().and_then(|c| c.as_deref()).unwrap_or("");
        let cap2 = item_cap.captures.get(1).and_then(|c| c.as_deref()).unwrap_or("");
        let cap1_u16_len = pi_js::str16::len(cap1);
        let first_part = cap2.split('\n').next().unwrap_or("");
        let mut line = expand_tabs(first_part, cap1_u16_len);
        let next_line_first = src.split('\n').next().unwrap_or("");
        let mut blank_line = pi_js::str16::trim(&line).is_empty();

        let indent: usize = if blank_line {
            cap1_u16_len + 1
        } else {
            let mut ind = non_space_char_u16(&line, &rules.other.non_space_char).max(0) as usize;
            if ind > 4 {
                ind = 1;
            }
            item_contents = pi_js::str16::slice(&line, ind as i64, None);
            ind + cap1_u16_len
        };

        if blank_line && rules.other.blank_line.test(next_line_first) {
            raw.push_str(next_line_first);
            raw.push('\n');
            src = advance_past_line(src, next_line_first.len());
            end_early = true;
        }

        if !end_early {
            let next_bullet_re = OtherRules::cached_slot(&rules.other.next_bullet_cache, indent);
            let hr_re = OtherRules::cached_slot(&rules.other.hr_cache, indent);
            let fences_begin_re = OtherRules::cached_slot(&rules.other.fences_begin_cache, indent);
            let heading_begin_re = OtherRules::cached_slot(&rules.other.heading_begin_cache, indent);
            let html_begin_re = OtherRules::cached_slot(&rules.other.html_begin_cache, indent);
            let blockquote_begin_re = OtherRules::cached_slot(&rules.other.blockquote_begin_cache, indent);

            while !src.is_empty() {
                let raw_line = src.split('\n').next().unwrap_or("");
                let next_line = raw_line;
                let next_line_without_tabs = rules.other.tab_char_global.replace(next_line, "    ");

                if fences_begin_re.test(next_line)
                    || heading_begin_re.test(next_line)
                    || html_begin_re.test(next_line)
                    || blockquote_begin_re.test(next_line)
                    || next_bullet_re.test(next_line)
                    || hr_re.test(next_line)
                {
                    break;
                }

                if non_space_char_u16(&next_line_without_tabs, &rules.other.non_space_char) >= indent as isize
                    || pi_js::str16::trim(next_line).is_empty()
                {
                    item_contents.push('\n');
                    item_contents.push_str(&pi_js::str16::slice(&next_line_without_tabs, indent as i64, None));
                } else {
                    if blank_line {
                        break;
                    }
                    let line_tabs = rules.other.tab_char_global.replace(&line, "    ");
                    if non_space_char_u16(&line_tabs, &rules.other.non_space_char) >= 4
                        || fences_begin_re.test(&line)
                        || heading_begin_re.test(&line)
                        || hr_re.test(&line)
                    {
                        break;
                    }
                    item_contents.push('\n');
                    item_contents.push_str(next_line);
                }

                blank_line = pi_js::str16::trim(next_line).is_empty();
                raw.push_str(raw_line);
                raw.push('\n');
                src = advance_past_line(src, raw_line.len());
                line = pi_js::str16::slice(&next_line_without_tabs, indent as i64, None);
            }
        }

        if !list.loose {
            if ends_with_blank_line {
                list.loose = true;
            } else if rules.other.double_blank_line.test(&raw) {
                ends_with_blank_line = true;
            }
        }

        let task = rules.other.list_is_task.test(&item_contents);
        list.items.push(Tokens::ListItem {
            raw: raw.clone(),
            task,
            checked: None,
            loose: false,
            text: item_contents,
            tokens: Vec::new(),
        });
        list.raw.push_str(&raw);
    }

    if let Some(last_item) = list.items.last_mut() {
        last_item.raw = pi_js::str16::trim_end(&last_item.raw).to_string();
        last_item.text = pi_js::str16::trim_end(&last_item.text).to_string();
    } else {
        return None;
    }
    list.raw = pi_js::str16::trim_end(&list.raw).to_string();

    // First pass: tokenize items and finalize list.loose from spacers before placing checkboxes.
    for item in &mut list.items {
        lexer.inner.state.lock().expect("lexer state lock").top = false;
        let mut item_tokens = Vec::new();
        lexer.block_tokens(&item.text, &mut item_tokens, false);
        item.tokens = item_tokens;

        if !list.loose {
            let has_multiple_line_breaks = item
                .tokens
                .iter()
                .any(|t| matches!(t, Token::Space(s) if rules.other.any_line.test(&s.raw)));
            list.loose = has_multiple_line_breaks;
        }
    }

    // Second pass: place task checkboxes using the final list.loose.
    for item in &mut list.items {
        let first_is_text_or_para = matches!(item.tokens.first(), Some(Token::Text(_) | Token::Paragraph(_)));
        if item.task && first_is_text_or_para {
            item.text = rules.other.list_replace_task.replace(&item.text, "");
            match item.tokens.first_mut() {
                Some(Token::Paragraph(p)) => {
                    p.raw = rules.other.list_replace_task.replace(&p.raw, "");
                    p.text = rules.other.list_replace_task.replace(&p.text, "");
                }
                Some(Token::Text(t)) => {
                    t.raw = rules.other.list_replace_task.replace(&t.raw, "");
                    t.text = rules.other.list_replace_task.replace(&t.text, "");
                }
                _ => {}
            }
            {
                let mut q = lexer.inner.inline_queue.lock().expect("inline_queue lock");
                for entry in q.iter_mut().rev() {
                    if rules.other.list_is_task.test(&entry.src) {
                        entry.src = rules.other.list_replace_task.replace(&entry.src, "");
                        break;
                    }
                }
            }

            if let Some(task_m) = rules.other.list_task_checkbox.exec(&item.raw) {
                let checkbox_token = Tokens::Checkbox {
                    raw: format!("{} ", task_m.matched),
                    checked: task_m.matched != "[ ]",
                };
                item.checked = Some(checkbox_token.checked);
                if list.loose {
                    match item.tokens.first_mut() {
                        Some(Token::Paragraph(p)) => {
                            p.raw = format!("{}{}", checkbox_token.raw, p.raw);
                            p.text = format!("{}{}", checkbox_token.raw, p.text);
                            p.tokens.insert(0, Token::Checkbox(checkbox_token));
                        }
                        Some(Token::Text(t)) => {
                            if let Some(inner) = &mut t.tokens {
                                t.raw = format!("{}{}", checkbox_token.raw, t.raw);
                                t.text = format!("{}{}", checkbox_token.raw, t.text);
                                inner.insert(0, Token::Checkbox(checkbox_token));
                            } else {
                                item.tokens.insert(
                                    0,
                                    Token::Paragraph(Tokens::Paragraph {
                                        raw: checkbox_token.raw.clone(),
                                        pre: None,
                                        text: checkbox_token.raw.clone(),
                                        tokens: vec![Token::Checkbox(checkbox_token)],
                                    }),
                                );
                            }
                        }
                        _ => {
                            item.tokens.insert(
                                0,
                                Token::Paragraph(Tokens::Paragraph {
                                    raw: checkbox_token.raw.clone(),
                                    pre: None,
                                    text: checkbox_token.raw.clone(),
                                    tokens: vec![Token::Checkbox(checkbox_token)],
                                }),
                            );
                        }
                    }
                } else {
                    item.tokens.insert(0, Token::Checkbox(checkbox_token));
                }
            }
        } else if item.task {
            item.task = false;
        }
    }

    if list.loose {
        for item in &mut list.items {
            item.loose = true;
            for tok in &mut item.tokens {
                if let Token::Text(t) = tok {
                    *tok = Token::Paragraph(Tokens::Paragraph {
                        raw: std::mem::take(&mut t.raw),
                        pre: None,
                        text: std::mem::take(&mut t.text),
                        tokens: t.tokens.take().unwrap_or_default(),
                    });
                }
            }
        }
    }

    Some(list)
}

fn stock_html(src: &str, rules: &GrammarSet) -> Option<Tokens::Html> {
    let cap = rules.block_gfm.html.exec(src)?;
    let raw = trim_trailing_blank_lines(&cap.matched, &rules.other.blank_line).to_string();
    let tag_name = cap.captures.first().and_then(|c| c.as_deref());
    let pre = matches!(tag_name, Some("pre" | "script" | "style"));
    Some(Tokens::Html {
        text: raw.clone(),
        raw,
        pre,
        block: true,
    })
}

fn stock_def(src: &str, rules: &GrammarSet) -> Option<Tokens::Def> {
    let cap = rules.block_gfm.def.exec(src)?;
    let cap1 = cap.captures.first().and_then(|c| c.as_deref()).unwrap_or("");
    let cap2 = cap.captures.get(1).and_then(|c| c.as_deref());
    let cap3 = cap.captures.get(2).and_then(|c| c.as_deref());

    let tag = rules.other.multiple_space_global.replace(&cap1.to_lowercase(), " ");
    let href = match cap2 {
        Some(h) if !h.is_empty() => {
            let unbracketed = rules.other.href_brackets.replace(h, "$1");
            rules.inline_gfm.any_punctuation.replace(&unbracketed, "$1")
        }
        _ => String::new(),
    };
    let title = match cap3 {
        Some(t) if !t.is_empty() => {
            let inner = pi_js::str16::substring(t, 1, Some(pi_js::str16::len(t).saturating_sub(1)));
            rules.inline_gfm.any_punctuation.replace(&inner, "$1")
        }
        _ => String::new(),
    };
    Some(Tokens::Def {
        raw: rtrim(&cap.matched, '\n').to_string(),
        tag,
        href,
        title,
    })
}

fn stock_table(src: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Tokens::Table> {
    let cap = rules.block_gfm.table.exec(src)?;
    let cap1 = cap.captures.first().and_then(|c| c.as_deref()).unwrap_or("");
    let cap2 = cap.captures.get(1).and_then(|c| c.as_deref()).unwrap_or("");
    let cap3 = cap.captures.get(2).and_then(|c| c.as_deref()).unwrap_or("");

    if !rules.other.table_delimiter.test(cap2) {
        return None;
    }

    let headers = split_cells(cap1, None, rules);
    let align_str = rules.other.table_align_chars.replace(cap2, "");
    let aligns_raw: Vec<&str> = align_str.split('|').collect();

    if headers.len() != aligns_raw.len() {
        return None;
    }

    let rows_raw: Vec<String> = if !pi_js::str16::trim(cap3).is_empty() {
        rules
            .other
            .table_row_blank_line
            .replace(cap3, "")
            .split('\n')
            .map(str::to_string)
            .collect()
    } else {
        Vec::new()
    };

    let mut align = Vec::with_capacity(aligns_raw.len());
    for a in aligns_raw {
        if rules.other.table_align_right.test(a) {
            align.push(Some(Tokens::TableAlign::Right));
        } else if rules.other.table_align_center.test(a) {
            align.push(Some(Tokens::TableAlign::Center));
        } else if rules.other.table_align_left.test(a) {
            align.push(Some(Tokens::TableAlign::Left));
        } else {
            align.push(None);
        }
    }

    let mut header = Vec::with_capacity(headers.len());
    for (i, h) in headers.into_iter().enumerate() {
        let tokens = lexer.inline(&h);
        header.push(Tokens::TableCell {
            text: h,
            tokens,
            header: true,
            align: align[i],
        });
    }

    let header_len = header.len();
    let mut rows = Vec::with_capacity(rows_raw.len());
    for row in rows_raw {
        let cells = split_cells(&row, Some(header_len), rules);
        let parsed_row = cells
            .into_iter()
            .enumerate()
            .map(|(i, cell)| {
                let tokens = lexer.inline(&cell);
                Tokens::TableCell {
                    text: cell,
                    tokens,
                    header: false,
                    align: align[i],
                }
            })
            .collect();
        rows.push(parsed_row);
    }

    Some(Tokens::Table {
        raw: rtrim(&cap.matched, '\n').to_string(),
        align,
        header,
        rows,
    })
}

fn stock_lheading(src: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Tokens::Heading> {
    let cap = rules.block_gfm.lheading.exec(src)?;
    let cap1 = cap.captures.first().and_then(|c| c.as_deref()).unwrap_or("");
    let cap2 = cap.captures.get(1).and_then(|c| c.as_deref()).unwrap_or("");
    let text = pi_js::str16::trim(cap1).to_string();
    let tokens = lexer.inline(&text);
    Some(Tokens::Heading {
        raw: rtrim(&cap.matched, '\n').to_string(),
        depth: if cap2.starts_with('=') { 1 } else { 2 },
        text,
        tokens,
    })
}

fn stock_paragraph(src: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Tokens::Paragraph> {
    let cap = rules.block_gfm.paragraph.exec(src)?;
    let cap1 = cap.captures.first().and_then(|c| c.as_deref()).unwrap_or("");
    let text = cap1.strip_suffix('\n').unwrap_or(cap1).to_string();
    let tokens = lexer.inline(&text);
    Some(Tokens::Paragraph {
        raw: cap.matched,
        pre: None,
        text,
        tokens,
    })
}

fn stock_block_text(src: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Tokens::Text> {
    let cap = rules.block_gfm.text.exec(src)?;
    let tokens = lexer.inline(&cap.matched);
    Some(Tokens::Text {
        text: cap.matched.clone(),
        raw: cap.matched,
        tokens: Some(tokens),
        escaped: None,
    })
}

fn stock_escape(src: &str, rules: &GrammarSet) -> Option<Tokens::Escape> {
    let cap = rules.inline_gfm.escape.exec(src)?;
    let text = cap
        .captures
        .first()
        .and_then(|c| c.as_deref())
        .unwrap_or("")
        .to_string();
    Some(Tokens::Escape { raw: cap.matched, text })
}

fn stock_tag(src: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Tokens::Tag> {
    let cap = rules.inline_gfm.tag.exec(src)?;
    let mut state = lexer.inner.state.lock().expect("lexer state lock");
    if !state.in_link && rules.other.start_a_tag.test(&cap.matched) {
        state.in_link = true;
    } else if state.in_link && rules.other.end_a_tag.test(&cap.matched) {
        state.in_link = false;
    }
    if !state.in_raw_block && rules.other.start_pre_script_tag.test(&cap.matched) {
        state.in_raw_block = true;
    } else if state.in_raw_block && rules.other.end_pre_script_tag.test(&cap.matched) {
        state.in_raw_block = false;
    }
    let in_link = state.in_link;
    let in_raw_block = state.in_raw_block;
    drop(state);

    Some(Tokens::Tag {
        text: cap.matched.clone(),
        raw: cap.matched,
        in_link,
        in_raw_block,
        block: false,
    })
}

fn output_link(
    label: &str,
    whole_raw_starts_with_bang: bool,
    href: String,
    title: String,
    raw: String,
    lexer: &Lexer,
    rules: &GrammarSet,
) -> Option<Token> {
    let text = rules.other.output_link_replace.replace(label, "$1");
    let is_image = whole_raw_starts_with_bang;

    let (outer_link_emitted, outer_in_raw_block) = {
        let mut st = lexer.inner.state.lock().expect("lexer state lock");
        let ole = st.link_emitted;
        let oirb = st.in_raw_block;
        st.in_link = true;
        st.link_emitted = false;
        (ole, oirb)
    };
    let tokens = lexer.inline_tokens(&text);
    let text_has_link = {
        let mut st = lexer.inner.state.lock().expect("lexer state lock");
        let thl = st.link_emitted;
        st.link_emitted = outer_link_emitted;
        st.in_link = false;
        thl
    };

    if !is_image {
        if text_has_link {
            lexer.inner.state.lock().expect("lexer state lock").in_raw_block = outer_in_raw_block;
            return None;
        }
        lexer.inner.state.lock().expect("lexer state lock").link_emitted = true;
    }

    if is_image {
        Some(Token::Image(Tokens::Image {
            raw,
            href,
            title: (!title.is_empty()).then_some(title),
            text,
            tokens,
        }))
    } else {
        Some(Token::Link(Tokens::Link {
            raw,
            href,
            title: Some((!title.is_empty()).then_some(title)),
            text,
            tokens,
        }))
    }
}

fn stock_link(src: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Token> {
    let cap = rules.inline_gfm.link.exec(src)?;
    let mut cap0 = cap.matched;
    let cap1 = cap
        .captures
        .first()
        .and_then(|c| c.as_deref())
        .unwrap_or("")
        .to_string();
    let mut cap2 = cap.captures.get(1).and_then(|c| c.as_deref()).unwrap_or("").to_string();
    let mut cap3 = cap.captures.get(2).and_then(|c| c.as_deref()).unwrap_or("").to_string();

    let trimmed_url = pi_js::str16::trim(&cap2);
    if rules.other.start_angle_bracket.test(trimmed_url) {
        if !rules.other.end_angle_bracket.test(trimmed_url) {
            return None;
        }
        let rtrim_slash = rtrim(&trimmed_url[..trimmed_url.len() - 1], '\\');
        if (pi_js::str16::len(trimmed_url) - pi_js::str16::len(rtrim_slash)).is_multiple_of(2) {
            return None;
        }
    } else {
        let last_paren_index = find_closing_bracket(&cap2, b'(' as u16, b')' as u16);
        if last_paren_index == -2 {
            return None;
        }
        if last_paren_index > -1 {
            let start: usize = if cap0.starts_with('!') { 5 } else { 4 };
            let link_len = start + pi_js::str16::len(&cap1) + last_paren_index as usize;
            cap2 = pi_js::str16::substring(&cap2, 0, Some(last_paren_index as usize));
            cap0 = pi_js::str16::trim(&pi_js::str16::substring(&cap0, 0, Some(link_len))).to_string();
            cap3.clear();
        }
    }

    let title = if !cap3.is_empty() {
        pi_js::str16::slice(&cap3, 1, Some(-1))
    } else {
        String::new()
    };
    let mut href = pi_js::str16::trim(&cap2).to_string();
    if rules.other.start_angle_bracket.test(&href) {
        href = pi_js::str16::slice(&href, 1, Some(-1));
    }

    let clean_href = if href.is_empty() {
        href
    } else {
        rules.inline_gfm.any_punctuation.replace(&href, "$1")
    };
    let clean_title = if title.is_empty() {
        title
    } else {
        rules.inline_gfm.any_punctuation.replace(&title, "$1")
    };
    let starts_with_bang = cap0.starts_with('!');
    output_link(&cap1, starts_with_bang, clean_href, clean_title, cap0, lexer, rules)
}

fn stock_reflink(src: &str, links: &Links, lexer: &Lexer, rules: &GrammarSet) -> Option<Token> {
    let cap = rules
        .inline_gfm
        .reflink
        .exec(src)
        .or_else(|| rules.inline_gfm.nolink.exec(src))?;
    let cap1 = cap.captures.first().and_then(|c| c.as_deref()).unwrap_or("");
    let cap2 = cap.captures.get(1).and_then(|c| c.as_deref());
    let chosen = cap2.filter(|s| !s.is_empty()).unwrap_or(cap1);
    let link_string = rules.other.multiple_space_global.replace(chosen, " ");
    let Some(link) = links.get(&link_string.to_lowercase()) else {
        let ch = cap.matched.chars().next().map_or(String::new(), |c| c.to_string());
        return Some(Token::Text(Tokens::Text {
            raw: ch.clone(),
            text: ch,
            tokens: None,
            escaped: None,
        }));
    };
    let title = match &link.title {
        Some(Some(t)) => t.clone(),
        _ => String::new(),
    };
    let starts_with_bang = cap.matched.starts_with('!');
    output_link(
        cap1,
        starts_with_bang,
        link.href.clone(),
        title,
        cap.matched,
        lexer,
        rules,
    )
}

fn stock_em_strong(src: &str, masked_src: &str, prev_char: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Token> {
    let m = rules.inline_gfm.em_strong_l_delim.exec(src)?;
    let c1 = m.captures.first().and_then(|c| c.as_deref()).unwrap_or("");
    let c2 = m.captures.get(1).and_then(|c| c.as_deref()).unwrap_or("");
    let c3 = m.captures.get(2).and_then(|c| c.as_deref()).unwrap_or("");
    let c4 = m.captures.get(3).and_then(|c| c.as_deref()).unwrap_or("");
    if c1.is_empty() && c2.is_empty() && c3.is_empty() && c4.is_empty() {
        return None;
    }

    if !c4.is_empty() && rules.other.unicode_alpha_numeric.test(prev_char) {
        return None;
    }

    let next_char = if !c1.is_empty() { c1 } else { c3 };
    if next_char.is_empty() || prev_char.is_empty() || rules.inline_gfm.punctuation.test(prev_char) {
        let l_length = m.matched.chars().count().saturating_sub(1);
        let delim_char = m.matched.chars().next()?;
        let mid_run = prev_char.chars().count() == 1 && prev_char.starts_with(delim_char);
        let end_reg = if delim_char == '*' {
            &rules.inline_gfm.em_strong_r_delim_ast
        } else {
            &rules.inline_gfm.em_strong_r_delim_und
        };

        let src_u16 = pi_js::str16::len(src);
        let masked_u16 = pi_js::str16::len(masked_src);
        let start_u16 = (masked_u16 as isize - src_u16 as isize + l_length as isize).max(0) as usize;
        let clipped_masked = &masked_src[pi_js::str16::utf16_to_byte(masked_src, start_u16)..];

        let mut delim_total = l_length as isize;
        let mut mid_delim_total: isize = 0;

        for em in end_reg.match_all(clipped_masked) {
            let g1 = em.captures.first().and_then(|c| c.as_deref()).filter(|s| !s.is_empty());
            let g2 = em.captures.get(1).and_then(|c| c.as_deref()).filter(|s| !s.is_empty());
            let g3 = em.captures.get(2).and_then(|c| c.as_deref()).filter(|s| !s.is_empty());
            let g4 = em.captures.get(3).and_then(|c| c.as_deref()).filter(|s| !s.is_empty());
            let g5 = em.captures.get(4).and_then(|c| c.as_deref()).filter(|s| !s.is_empty());
            let g6 = em.captures.get(5).and_then(|c| c.as_deref()).filter(|s| !s.is_empty());

            let Some(r_delim) = g1.or(g2).or(g3).or(g4).or(g5).or(g6) else {
                continue;
            };
            let mut r_length = r_delim.chars().count() as isize;

            if g3.is_some() || g4.is_some() {
                delim_total += r_length;
                continue;
            } else if g5.is_some() || g6.is_some() {
                if l_length % 3 != 0 && (l_length as isize + r_length) % 3 == 0 {
                    mid_delim_total += r_length;
                    continue;
                }
                if mid_run {
                    break;
                }
            }

            delim_total -= r_length;
            if delim_total > 0 {
                continue;
            }

            r_length = r_length.min(r_length + delim_total + mid_delim_total);
            let last_char_u16 = em.matched.chars().next().map_or(0, char::len_utf16);
            let raw_u16 = l_length + em.index + last_char_u16 + r_length.max(0) as usize;
            let raw = pi_js::str16::slice(src, 0, Some(raw_u16 as i64));

            if (l_length as isize).min(r_length) % 2 != 0 {
                let text = pi_js::str16::slice(&raw, 1, Some(-1));
                let tokens = lexer.inline_tokens(&text);
                return Some(Token::Em(Tokens::Em { raw, text, tokens }));
            } else {
                let text = pi_js::str16::slice(&raw, 2, Some(-2));
                let tokens = lexer.inline_tokens(&text);
                return Some(Token::Strong(Tokens::Strong { raw, text, tokens }));
            }
        }
    }
    None
}

fn stock_codespan(src: &str, rules: &GrammarSet) -> Option<Tokens::Codespan> {
    let cap = rules.inline_gfm.code.exec(src)?;
    let cap2 = cap.captures.get(1).and_then(|c| c.as_deref()).unwrap_or("");
    let mut text = rules.other.new_line_char_global.replace(cap2, " ");
    let has_non_space = rules.other.non_space_char.test(&text);
    let has_spaces_both_ends = rules.other.starting_space_char.test(&text) && rules.other.ending_space_char.test(&text);
    if has_non_space && has_spaces_both_ends {
        text = text[1..text.len() - 1].to_string();
    }
    Some(Tokens::Codespan { raw: cap.matched, text })
}

fn stock_br(src: &str, rules: &GrammarSet) -> Option<Tokens::Br> {
    let cap = rules.inline_gfm.br.exec(src)?;
    Some(Tokens::Br { raw: cap.matched })
}

fn stock_del(src: &str, masked_src: &str, prev_char: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Tokens::Del> {
    let m = rules.inline_gfm.del_l_delim.exec(src)?;
    let next_char = m.captures.first().and_then(|c| c.as_deref()).unwrap_or("");
    if next_char.is_empty() || prev_char.is_empty() || rules.inline_gfm.punctuation.test(prev_char) {
        let l_length = m.matched.chars().count().saturating_sub(1);
        let src_u16 = pi_js::str16::len(src);
        let masked_u16 = pi_js::str16::len(masked_src);
        let start_u16 = (masked_u16 as isize - src_u16 as isize + l_length as isize).max(0) as usize;
        let clipped_masked = &masked_src[pi_js::str16::utf16_to_byte(masked_src, start_u16)..];

        let mut delim_total = l_length as isize;
        for em in rules.inline_gfm.del_r_delim.match_all(clipped_masked) {
            let g1 = em.captures.first().and_then(|c| c.as_deref()).filter(|s| !s.is_empty());
            let g2 = em.captures.get(1).and_then(|c| c.as_deref()).filter(|s| !s.is_empty());
            let g3 = em.captures.get(2).and_then(|c| c.as_deref()).filter(|s| !s.is_empty());
            let g4 = em.captures.get(3).and_then(|c| c.as_deref()).filter(|s| !s.is_empty());
            let g5 = em.captures.get(4).and_then(|c| c.as_deref()).filter(|s| !s.is_empty());
            let g6 = em.captures.get(5).and_then(|c| c.as_deref()).filter(|s| !s.is_empty());

            let Some(r_delim) = g1.or(g2).or(g3).or(g4).or(g5).or(g6) else {
                continue;
            };
            let r_length = r_delim.chars().count() as isize;
            if r_length != l_length as isize {
                continue;
            }

            if g3.is_some() || g4.is_some() {
                delim_total += r_length;
                continue;
            }

            delim_total -= r_length;
            if delim_total > 0 {
                continue;
            }

            let clamped_r = r_length.min(r_length + delim_total);
            let last_char_u16 = em.matched.chars().next().map_or(0, char::len_utf16);
            let raw_u16 = l_length + em.index + last_char_u16 + clamped_r.max(0) as usize;
            let raw = pi_js::str16::slice(src, 0, Some(raw_u16 as i64));
            let text = pi_js::str16::slice(&raw, l_length as i64, Some(-(l_length as i64)));
            let tokens = lexer.inline_tokens(&text);
            return Some(Tokens::Del { raw, text, tokens });
        }
    }
    None
}

fn stock_autolink(src: &str, rules: &GrammarSet) -> Option<Tokens::Link> {
    let cap = rules.inline_gfm.autolink.exec(src)?;
    let cap1 = cap
        .captures
        .first()
        .and_then(|c| c.as_deref())
        .unwrap_or("")
        .to_string();
    let cap2 = cap.captures.get(1).and_then(|c| c.as_deref()).unwrap_or("");
    let text = cap1;
    let href = if cap2 == "@" {
        format!("mailto:{text}")
    } else {
        text.clone()
    };
    Some(Tokens::Link {
        raw: cap.matched,
        text: text.clone(),
        href,
        title: None,
        tokens: vec![Token::Text(Tokens::Text {
            raw: text.clone(),
            text,
            tokens: None,
            escaped: None,
        })],
    })
}

fn stock_url(src: &str, rules: &GrammarSet) -> Option<Tokens::Link> {
    let cap = rules.inline_gfm.url.exec(src)?;
    let mut cap0 = cap.matched;
    let cap1 = cap.captures.first().and_then(|c| c.as_deref()).unwrap_or("");
    let cap2 = cap.captures.get(1).and_then(|c| c.as_deref()).unwrap_or("");
    let (text, href) = if cap2 == "@" {
        let t = cap0.clone();
        let h = format!("mailto:{t}");
        (t, h)
    } else {
        loop {
            let prev = cap0.clone();
            cap0 = rules
                .inline_gfm
                ._backpedal
                .exec(&cap0)
                .map_or_else(String::new, |m| m.matched);
            if prev == cap0 {
                break;
            }
        }
        if cap0.is_empty() {
            return None;
        }
        let h = if cap1 == "www." {
            format!("http://{cap0}")
        } else {
            cap0.clone()
        };
        (cap0.clone(), h)
    };
    Some(Tokens::Link {
        raw: cap0,
        text: text.clone(),
        href,
        title: None,
        tokens: vec![Token::Text(Tokens::Text {
            raw: text.clone(),
            text,
            tokens: None,
            escaped: None,
        })],
    })
}

fn stock_inline_text(src: &str, lexer: &Lexer, rules: &GrammarSet) -> Option<Tokens::Text> {
    let cap = rules.inline_gfm.text.exec(src)?;
    let escaped = lexer.inner.state.lock().expect("lexer state lock").in_raw_block;
    Some(Tokens::Text {
        text: cap.matched.clone(),
        raw: cap.matched,
        tokens: None,
        escaped: Some(escaped),
    })
}

// =============================================================================
// Public Marked / Tokenizer / Extension Surface
// =============================================================================

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
        Self {
            inner: Arc::new(TokenizerInner {
                options: Mutex::new(MarkedOptions::default()),
                lexer: Mutex::new(None),
            }),
        }
    }
}

impl Default for Tokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkedTokenizer for Tokenizer {
    fn lexer(&self) -> Lexer {
        self.inner
            .lexer
            .lock()
            .expect("tokenizer lexer lock")
            .clone()
            .unwrap_or_else(Lexer::new)
    }

    fn set_lexer(&self, lexer: Lexer) {
        *self.inner.lexer.lock().expect("tokenizer lexer lock") = Some(lexer);
    }

    fn del(&self, src: &str, masked_src: &str, prev_char: Option<&str>) -> Option<Tokens::Del> {
        let lexer = self.lexer();
        stock_del(src, masked_src, prev_char.unwrap_or(""), &lexer, &GRAMMAR)
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
        Self {
            inner: Arc::new(MarkedInner {
                options: Mutex::new(MarkedOptions::default()),
                extensions: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn set_options(&self, opt: MarkedOptions) -> Self {
        let mut guard = self.inner.options.lock().expect("marked options lock");
        *guard = opt;
        drop(guard);
        self.clone()
    }

    /// Registers extensions with this Marked instance.
    ///
    /// The `renderer`, `tokenizer`, and `hooks` objects may each contain only the
    /// methods that should be overridden. Their methods are merged with built-in
    /// behavior and extensions registered by earlier calls.
    ///
    /// Use this method when supplying only some hook methods.
    pub fn r#use(&self, extension: MarkedExtension) -> Self {
        if let Some(exts) = extension.extensions {
            let mut guard = self.inner.extensions.lock().expect("marked extensions lock");
            guard.extend(exts);
        }
        self.clone()
    }

    pub fn lexer(&self, src: &str, options: Option<MarkedOptions>) -> TokensList {
        let (opts, exts) = match options {
            Some(opt) => (opt, Vec::new()),
            None => (
                self.inner.options.lock().expect("marked options lock").clone(),
                self.inner.extensions.lock().expect("marked extensions lock").clone(),
            ),
        };
        Lexer::with_config(opts, &exts).lex(src)
    }
}

impl Default for Marked {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn headings_setext_and_fences_with_indent_compensation() {
        let m = Marked::new();
        let list = m.lexer(
            "# Hello ##\r\n\r\nSub\n---\n\n  ```rs\n  let x = 1;\n    let y = 2;\n  ```",
            None,
        );
        assert!(matches!(
            &list.tokens[0],
            Token::Heading(Tokens::Heading { depth: 1, text, .. }) if text == "Hello"
        ));
        assert!(matches!(&list.tokens[1], Token::Space(_)));
        assert!(matches!(
            &list.tokens[2],
            Token::Heading(Tokens::Heading { depth: 2, text, .. }) if text == "Sub"
        ));
        assert!(matches!(
            &list.tokens[4],
            Token::Code(Tokens::Code { lang: Some(lang), code_block_style: None, text, .. })
                if lang == "rs" && text == "let x = 1;\n  let y = 2;"
        ));
    }

    #[test]
    fn gfm_tables_with_escaped_pipes_and_alignment() {
        let m = Marked::new();
        let list = m.lexer(
            "| Left | Center | Right | Def |\n| :--- | :---: | ---: | --- |\n| a\\|b | *c* | d |\n",
            None,
        );
        let Token::Table(table) = &list.tokens[0] else {
            panic!("expected Table, got {:?}", list.tokens);
        };
        assert_eq!(
            table.align,
            vec![
                Some(Tokens::TableAlign::Left),
                Some(Tokens::TableAlign::Center),
                Some(Tokens::TableAlign::Right),
                None,
            ]
        );
        assert_eq!(table.header[0].text, "Left");
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0][0].text, "a|b");
        assert!(matches!(&table.rows[0][1].tokens[0], Token::Em(e) if e.text == "c"));
        assert_eq!(table.rows[0][3].text, "");
    }

    #[test]
    fn tight_and_loose_task_lists() {
        let m = Marked::new();
        let tight = m.lexer("- [ ] todo\n- [x] done", None);
        let Token::List(list) = &tight.tokens[0] else {
            panic!("expected List");
        };
        assert!(!list.loose);
        assert_eq!(list.items[0].checked, Some(false));
        assert_eq!(list.items[1].checked, Some(true));
        assert!(matches!(&list.items[0].tokens[0], Token::Checkbox(c) if !c.checked));
        assert!(matches!(&list.items[1].tokens[1], Token::Text(t) if t.text == "done"));

        let loose = m.lexer("- [ ] one\n\n- [x] *two*", None);
        let Token::List(loose_list) = &loose.tokens[0] else {
            panic!("expected List");
        };
        assert!(loose_list.loose);
        assert!(matches!(
            &loose_list.items[1].tokens[0],
            Token::Paragraph(p) if matches!(p.tokens.as_slice(), [Token::Checkbox(c), Token::Em(e)] if c.checked && e.text == "two")
        ));
    }

    #[test]
    fn links_reflinks_nested_prevention_and_urls() {
        let m = Marked::new();
        let list = m.lexer(
            "[outer [inner](https://a)](https://b) and [ref][k] and [![alt](img.png)](https://pic) and https://example.com/a(b).",
            None,
        );
        let mut combined = list.clone();
        assert!(matches!(combined.tokens[0], Token::Paragraph(_)));
        let _ = &mut combined;

        let list2 = m.lexer("[ok][k] and [missing][m]\n\n[k]: https://k.test \"Title\"", None);
        assert!(list2.links.contains_key("k"));
        let Token::Paragraph(p) = &list2.tokens[0] else {
            panic!("expected Paragraph");
        };
        assert!(matches!(
            &p.tokens[0],
            Token::Link(Tokens::Link { href, title: Some(Some(t)), text, .. })
                if href == "https://k.test" && t == "Title" && text == "ok"
        ));
        assert!(matches!(&p.tokens[1], Token::Text(t) if t.text == " and [missing][m]"));

        // GFM bare url backpedals trailing period and matches nested parens
        let Token::Paragraph(p_url) = &list.tokens[0] else {
            panic!("expected Paragraph");
        };
        assert!(p_url.tokens.iter().any(|tok| matches!(
            tok,
            Token::Link(Tokens::Link { href, title: None, .. }) if href == "https://example.com/a(b)"
        )));
    }

    #[test]
    fn emphasis_and_emoji_surrogate_alignment() {
        let m = Marked::new();
        let list = m.lexer("*🎉* and **bold** and `inline *code*` and a_b_c", None);
        let Token::Paragraph(p) = &list.tokens[0] else {
            panic!("expected Paragraph");
        };
        assert!(matches!(&p.tokens[0], Token::Em(e) if e.text == "🎉"));
        assert!(matches!(
            &p.tokens[2],
            Token::Strong(s) if s.text == "bold"
        ));
        assert!(matches!(
            &p.tokens[4],
            Token::Codespan(c) if c.text == "inline *code*"
        ));
        assert!(matches!(
            &p.tokens[5],
            Token::Text(t) if t.text == " and a_b_c"
        ));
    }

    #[test]
    fn stock_and_overridden_strikethrough_and_latex_extensions() {
        let stock = Marked::new();
        let res = stock.lexer("~single~ and ~~double~~", None);
        let Token::Paragraph(p) = &res.tokens[0] else {
            panic!("expected Paragraph");
        };
        assert!(matches!(&p.tokens[0], Token::Del(d) if d.text == "single"));
        assert!(matches!(&p.tokens[2], Token::Del(d) if d.text == "double"));

        struct StrictDelTokenizer {
            base: Tokenizer,
            re: fancy_regex::Regex,
        }
        impl MarkedTokenizer for StrictDelTokenizer {
            fn lexer(&self) -> Lexer {
                self.base.lexer()
            }
            fn set_lexer(&self, lexer: Lexer) {
                self.base.set_lexer(lexer);
            }
            fn del(&self, src: &str, _masked_src: &str, _prev_char: Option<&str>) -> Option<Tokens::Del> {
                let cap = self.re.captures(src).ok().flatten()?;
                let raw = cap.get(0)?.as_str().to_string();
                let text = cap.get(2)?.as_str().to_string();
                let tokens = self.lexer().inline_tokens(&text);
                Some(Tokens::Del { raw, text, tokens })
            }
        }

        let strict_re = fancy_regex::Regex::new(&format!(
            r"^(~~)(?=[^{ws}~])((?:\\.|[^\\])*?(?:\\.|[^{ws}~\\]))\1(?=[^~]|$)",
            ws = pi_js::regex::JS_WS_CHARS
        ))
        .expect("strict re");
        let custom = Marked::new();
        custom.set_options(MarkedOptions {
            tokenizer: Some(Arc::new(StrictDelTokenizer {
                base: Tokenizer::new(),
                re: strict_re,
            })),
        });
        custom.r#use(MarkedExtension {
            extensions: Some(vec![TokenizerExtension {
                name: "inlineDollar".to_string(),
                level: TokenizerLevel::Inline,
                start: Some(Arc::new(|_, s| {
                    s.find('$').map(|i| pi_js::str16::byte_to_utf16(s, i) as i64)
                })),
                tokenizer: Arc::new(|_, src, _| {
                    let rest = src.strip_prefix('$')?;
                    let close = rest.find('$')?;
                    Some(Tokens::Generic {
                        r#type: "latex".to_string(),
                        raw: src[..close + 2].to_string(),
                        tokens: None,
                        text: Some(rest[..close].to_string()),
                        pending: None,
                        extra: IndexMap::new(),
                    })
                }),
                child_tokens: None,
            }]),
        });

        let custom_res = custom.lexer("~single~ ~~*em* $x$~~ 🎉$y$", None);
        let Token::Paragraph(cp) = &custom_res.tokens[0] else {
            panic!("expected Paragraph");
        };
        assert!(matches!(&cp.tokens[0], Token::Text(t) if t.text == "~single~ "));
        let Token::Del(del_tok) = &cp.tokens[1] else {
            panic!("expected Del, got {:?}", cp.tokens);
        };
        assert!(matches!(&del_tok.tokens[0], Token::Em(e) if e.text == "em"));
        assert!(matches!(
            &del_tok.tokens[2],
            Token::Generic(g) if g.r#type == "latex" && g.text.as_deref() == Some("x")
        ));
        assert!(matches!(&cp.tokens[2], Token::Text(t) if t.text == " 🎉"));
        assert!(matches!(
            &cp.tokens[3],
            Token::Generic(g) if g.r#type == "latex" && g.text.as_deref() == Some("y")
        ));
    }

    #[test]
    fn blockquotes_html_autolinks_escapes_br_and_block_ext_clipping() {
        let m = Marked::new();
        m.r#use(MarkedExtension {
            extensions: Some(vec![TokenizerExtension {
                name: "mathBlock".to_string(),
                level: TokenizerLevel::Block,
                start: Some(Arc::new(|_, s| {
                    s.find("$$").map(|i| pi_js::str16::byte_to_utf16(s, i) as i64)
                })),
                tokenizer: Arc::new(|_, src, _| {
                    let rest = src.strip_prefix("$$\n")?;
                    let close = rest.find("\n$$")?;
                    let raw_end = 3 + close + 3;
                    Some(Tokens::Generic {
                        r#type: "latexBlock".to_string(),
                        raw: src[..raw_end].to_string(),
                        tokens: None,
                        text: Some(rest[..close].to_string()),
                        pending: None,
                        extra: IndexMap::new(),
                    })
                }),
                child_tokens: None,
            }]),
        });

        let doc = concat!(
            "> quoted *line*  \n",
            "> next\\*line <https://x.dev> <a@b.co>\n\n",
            "<!-- comment -->\n\n",
            "Paragraph before\n",
            "$$\nx + y\n$$\n",
            "```rust\n",
            "fn main() {}\n",
            "```"
        );
        let list = m.lexer(doc, None);
        assert!(matches!(
            &list.tokens[0],
            Token::Blockquote(b) if matches!(b.tokens.first(), Some(Token::Paragraph(p)) if p.tokens.iter().any(|t| matches!(t, Token::Br(_))) && p.tokens.iter().any(|t| matches!(t, Token::Escape(e) if e.text == "*")) && p.tokens.iter().any(|t| matches!(t, Token::Link(l) if l.href == "https://x.dev")) && p.tokens.iter().any(|t| matches!(t, Token::Link(l) if l.href == "mailto:a@b.co")))
        ));
        assert!(matches!(
            &list.tokens[2],
            Token::Html(h) if h.block && !h.pre && h.text == "<!-- comment -->"
        ));
        assert!(matches!(
            &list.tokens[4],
            Token::Paragraph(p) if p.text == "Paragraph before"
        ));
        assert!(matches!(
            &list.tokens[5],
            Token::Generic(g) if g.r#type == "latexBlock" && g.text.as_deref() == Some("x + y") && g.raw == "$$\nx + y\n$$\n"
        ));
        assert!(matches!(
            &list.tokens[6],
            Token::Code(c) if c.lang.as_deref() == Some("rust") && c.text == "fn main() {}"
        ));
        assert!(m.lexer("", None).tokens.is_empty());
    }
}

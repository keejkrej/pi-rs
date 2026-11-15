//! Port of packages/tui/src/latex.ts

use std::sync::LazyLock;

use indexmap::{IndexMap, IndexSet};
use regex::Regex;

use crate::utils::visible_width;

static SYMBOLS: LazyLock<IndexMap<&'static str, &'static str>> = LazyLock::new(|| {
    IndexMap::from([
        ("alpha", "α"),
        ("beta", "β"),
        ("gamma", "γ"),
        ("delta", "δ"),
        ("epsilon", "ϵ"),
        ("varepsilon", "ε"),
        ("zeta", "ζ"),
        ("eta", "η"),
        ("theta", "θ"),
        ("vartheta", "ϑ"),
        ("iota", "ι"),
        ("kappa", "κ"),
        ("varkappa", "ϰ"),
        ("lambda", "λ"),
        ("mu", "μ"),
        ("nu", "ν"),
        ("xi", "ξ"),
        ("pi", "π"),
        ("varpi", "ϖ"),
        ("rho", "ρ"),
        ("varrho", "ϱ"),
        ("sigma", "σ"),
        ("varsigma", "ς"),
        ("tau", "τ"),
        ("upsilon", "υ"),
        ("phi", "ϕ"),
        ("varphi", "φ"),
        ("chi", "χ"),
        ("psi", "ψ"),
        ("omega", "ω"),
        ("Gamma", "Γ"),
        ("Delta", "Δ"),
        ("Theta", "Θ"),
        ("Lambda", "Λ"),
        ("Xi", "Ξ"),
        ("Pi", "Π"),
        ("Sigma", "Σ"),
        ("Upsilon", "Υ"),
        ("Phi", "Φ"),
        ("Psi", "Ψ"),
        ("Omega", "Ω"),
        ("pm", "±"),
        ("mp", "∓"),
        ("times", "×"),
        ("div", "÷"),
        ("cdot", "·"),
        ("ast", "∗"),
        ("star", "⋆"),
        ("circ", "∘"),
        ("bullet", "•"),
        ("oplus", "⊕"),
        ("ominus", "⊖"),
        ("otimes", "⊗"),
        ("oslash", "⊘"),
        ("odot", "⊙"),
        ("bigcirc", "○"),
        ("dagger", "†"),
        ("ddagger", "‡"),
        ("amalg", "⨿"),
        ("uplus", "⊎"),
        ("sqcap", "⊓"),
        ("sqcup", "⊔"),
        ("bowtie", "⋈"),
        ("Join", "⋈"),
        ("ltimes", "⋉"),
        ("rtimes", "⋊"),
        ("leftouterjoin", "⟕"),
        ("rightouterjoin", "⟖"),
        ("fullouterjoin", "⟗"),
        ("triangleleft", "◁"),
        ("triangleright", "▷"),
        ("wr", "≀"),
        ("cap", "∩"),
        ("cup", "∪"),
        ("bigcap", "⋂"),
        ("bigcup", "⋃"),
        ("bigwedge", "⋀"),
        ("bigvee", "⋁"),
        ("bigsqcup", "⨆"),
        ("biguplus", "⨄"),
        ("bigoplus", "⨁"),
        ("bigotimes", "⨂"),
        ("bigodot", "⨀"),
        ("setminus", "∖"),
        ("in", "∈"),
        ("notin", "∉"),
        ("ni", "∋"),
        ("subset", "⊂"),
        ("supset", "⊃"),
        ("subseteq", "⊆"),
        ("supseteq", "⊇"),
        ("sqsubset", "⊏"),
        ("sqsupset", "⊐"),
        ("sqsubseteq", "⊑"),
        ("sqsupseteq", "⊒"),
        ("prec", "≺"),
        ("preceq", "≼"),
        ("succ", "≻"),
        ("succeq", "≽"),
        ("ll", "≪"),
        ("gg", "≫"),
        ("le", "≤"),
        ("leq", "≤"),
        ("leqslant", "≤"),
        ("ge", "≥"),
        ("geq", "≥"),
        ("geqslant", "≥"),
        ("ne", "≠"),
        ("neq", "≠"),
        ("equiv", "≡"),
        ("approx", "≈"),
        ("sim", "∼"),
        ("simeq", "≃"),
        ("cong", "≅"),
        ("asymp", "≍"),
        ("doteq", "≐"),
        ("propto", "∝"),
        ("parallel", "∥"),
        ("perp", "⊥"),
        ("mid", "∣"),
        ("vdash", "⊢"),
        ("dashv", "⊣"),
        ("models", "⊨"),
        ("Vdash", "⊩"),
        ("Vvdash", "⊪"),
        ("nvdash", "⊬"),
        ("nvDash", "⊭"),
        ("forall", "∀"),
        ("exists", "∃"),
        ("nexists", "∄"),
        ("neg", "¬"),
        ("land", "∧"),
        ("wedge", "∧"),
        ("lor", "∨"),
        ("vee", "∨"),
        ("to", "→"),
        ("rightarrow", "→"),
        ("longrightarrow", "→"),
        ("leftarrow", "←"),
        ("longleftarrow", "←"),
        ("gets", "←"),
        ("leftrightarrow", "↔"),
        ("longleftrightarrow", "↔"),
        ("hookleftarrow", "↩"),
        ("hookrightarrow", "↪"),
        ("twoheadleftarrow", "↞"),
        ("twoheadrightarrow", "↠"),
        ("leftharpoonup", "↼"),
        ("leftharpoondown", "↽"),
        ("rightharpoonup", "⇀"),
        ("rightharpoondown", "⇁"),
        ("rightleftharpoons", "⇌"),
        ("leftrightharpoons", "⇋"),
        ("nearrow", "↗"),
        ("searrow", "↘"),
        ("swarrow", "↙"),
        ("nwarrow", "↖"),
        ("rightsquigarrow", "⇝"),
        ("leadsto", "⇝"),
        ("Rightarrow", "⇒"),
        ("Longrightarrow", "⇒"),
        ("Leftarrow", "⇐"),
        ("Longleftarrow", "⇐"),
        ("Leftrightarrow", "⇔"),
        ("Longleftrightarrow", "⇔"),
        ("implies", "⇒"),
        ("iff", "⇔"),
        ("mapsto", "↦"),
        ("longmapsto", "↦"),
        ("uparrow", "↑"),
        ("downarrow", "↓"),
        ("partial", "∂"),
        ("nabla", "∇"),
        ("int", "∫"),
        ("iint", "∬"),
        ("iiint", "∭"),
        ("oint", "∮"),
        ("sum", "∑"),
        ("prod", "∏"),
        ("coprod", "∐"),
        ("infty", "∞"),
        ("emptyset", "∅"),
        ("varnothing", "∅"),
        ("angle", "∠"),
        ("therefore", "∴"),
        ("because", "∵"),
        ("aleph", "ℵ"),
        ("beth", "ℶ"),
        ("gimel", "ℷ"),
        ("daleth", "ℸ"),
        ("top", "⊤"),
        ("bot", "⊥"),
        ("triangle", "△"),
        ("square", "□"),
        ("lozenge", "◊"),
        ("checkmark", "✓"),
        ("complement", "∁"),
        ("wp", "℘"),
        ("prime", "′"),
        ("ldots", "…"),
        ("dots", "…"),
        ("cdots", "⋯"),
        ("vdots", "⋮"),
        ("ddots", "⋱"),
        ("ell", "ℓ"),
        ("hbar", "ℏ"),
        ("Im", "ℑ"),
        ("Re", "ℜ"),
        ("langle", "⟨"),
        ("rangle", "⟩"),
        ("vert", "|"),
        ("lvert", "|"),
        ("rvert", "|"),
        ("Vert", "‖"),
        ("lVert", "‖"),
        ("rVert", "‖"),
        ("lbrace", "{"),
        ("rbrace", "}"),
        ("backslash", "\\"),
        ("lfloor", "⌊"),
        ("rfloor", "⌋"),
        ("lceil", "⌈"),
        ("rceil", "⌉"),
        ("colon", ":"),
    ])
});

static NAMED_OPERATORS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        "arccos", "arcsin", "arctan", "arg", "cos", "cosh", "cot", "coth", "csc", "deg", "det", "dim", "exp", "gcd",
        "hom", "inf", "ker", "lg", "lim", "liminf", "limsup", "ln", "log", "max", "min", "Pr", "sec", "sin", "sinh",
        "sup", "tan", "tanh",
    ])
});

static LIMIT_OPERATORS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        "argmax", "argmin", "inf", "injlim", "lim", "liminf", "limsup", "max", "min", "projlim", "sup",
    ])
});

static DISPLAY_LIMIT_SYMBOLS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        "bigcap",
        "bigcup",
        "bigodot",
        "bigoplus",
        "bigotimes",
        "bigsqcup",
        "biguplus",
        "bigvee",
        "bigwedge",
        "coprod",
        "int",
        "iint",
        "iiint",
        "oint",
        "prod",
        "sum",
    ])
});

static RELATION_COMMANDS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        "Leftarrow",
        "Leftrightarrow",
        "Longleftarrow",
        "Longleftrightarrow",
        "Longrightarrow",
        "Rightarrow",
        "Join",
        "Vdash",
        "Vvdash",
        "approx",
        "asymp",
        "bowtie",
        "cong",
        "dashv",
        "fullouterjoin",
        "doteq",
        "downarrow",
        "equiv",
        "ge",
        "geq",
        "geqslant",
        "gets",
        "gg",
        "hookleftarrow",
        "hookrightarrow",
        "iff",
        "implies",
        "in",
        "leadsto",
        "le",
        "leftarrow",
        "leftharpoondown",
        "leftharpoonup",
        "leftrightarrow",
        "leftrightharpoons",
        "leftouterjoin",
        "leq",
        "leqslant",
        "ll",
        "longleftarrow",
        "longleftrightarrow",
        "longmapsto",
        "longrightarrow",
        "ltimes",
        "mapsto",
        "mid",
        "models",
        "ne",
        "nearrow",
        "neq",
        "ni",
        "notin",
        "nvdash",
        "nvDash",
        "nwarrow",
        "parallel",
        "perp",
        "prec",
        "preceq",
        "propto",
        "rightharpoondown",
        "rightharpoonup",
        "rightleftharpoons",
        "rightouterjoin",
        "rightarrow",
        "rightsquigarrow",
        "rtimes",
        "searrow",
        "sim",
        "simeq",
        "sqsubset",
        "sqsubseteq",
        "sqsupset",
        "sqsupseteq",
        "subset",
        "subseteq",
        "succ",
        "succeq",
        "supset",
        "supseteq",
        "swarrow",
        "to",
        "triangleleft",
        "triangleright",
        "twoheadleftarrow",
        "twoheadrightarrow",
        "uparrow",
        "vdash",
    ])
});

static NEGATED_SYMBOLS: LazyLock<IndexMap<&'static str, &'static str>> = LazyLock::new(|| {
    IndexMap::from([
        ("<", "≮"),
        (">", "≯"),
        ("=", "≠"),
        ("∈", "∉"),
        ("∋", "∌"),
        ("∣", "∤"),
        ("∥", "∦"),
        ("∼", "≁"),
        ("≃", "≄"),
        ("≅", "≇"),
        ("≈", "≉"),
        ("≡", "≢"),
        ("≤", "≰"),
        ("≥", "≱"),
        ("≺", "⊀"),
        ("≻", "⊁"),
        ("⊂", "⊄"),
        ("⊃", "⊅"),
        ("⊆", "⊈"),
        ("⊇", "⊉"),
        ("⊢", "⊬"),
        ("⊨", "⊭"),
        ("↔", "↮"),
        ("←", "↚"),
        ("→", "↛"),
        ("⇒", "⇏"),
        ("⇐", "⇍"),
        ("⇔", "⇎"),
        ("≼", "⋠"),
        ("≽", "⋡"),
    ])
});

static BLACKBOARD: LazyLock<IndexMap<char, &'static str>> = LazyLock::new(|| {
    IndexMap::from([
        ('C', "ℂ"),
        ('H', "ℍ"),
        ('N', "ℕ"),
        ('P', "ℙ"),
        ('Q', "ℚ"),
        ('R', "ℝ"),
        ('Z', "ℤ"),
    ])
});

static SUPERSCRIPTS: LazyLock<IndexMap<char, char>> = LazyLock::new(|| {
    IndexMap::from([
        ('0', '⁰'),
        ('1', '¹'),
        ('2', '²'),
        ('3', '³'),
        ('4', '⁴'),
        ('5', '⁵'),
        ('6', '⁶'),
        ('7', '⁷'),
        ('8', '⁸'),
        ('9', '⁹'),
        ('+', '⁺'),
        ('-', '⁻'),
        ('=', '⁼'),
        ('(', '⁽'),
        (')', '⁾'),
        ('a', 'ᵃ'),
        ('b', 'ᵇ'),
        ('c', 'ᶜ'),
        ('d', 'ᵈ'),
        ('e', 'ᵉ'),
        ('f', 'ᶠ'),
        ('g', 'ᵍ'),
        ('h', 'ʰ'),
        ('i', 'ⁱ'),
        ('j', 'ʲ'),
        ('k', 'ᵏ'),
        ('l', 'ˡ'),
        ('m', 'ᵐ'),
        ('n', 'ⁿ'),
        ('o', 'ᵒ'),
        ('p', 'ᵖ'),
        ('r', 'ʳ'),
        ('s', 'ˢ'),
        ('t', 'ᵗ'),
        ('u', 'ᵘ'),
        ('v', 'ᵛ'),
        ('w', 'ʷ'),
        ('x', 'ˣ'),
        ('y', 'ʸ'),
        ('z', 'ᶻ'),
    ])
});

static SUBSCRIPTS: LazyLock<IndexMap<char, char>> = LazyLock::new(|| {
    IndexMap::from([
        ('0', '₀'),
        ('1', '₁'),
        ('2', '₂'),
        ('3', '₃'),
        ('4', '₄'),
        ('5', '₅'),
        ('6', '₆'),
        ('7', '₇'),
        ('8', '₈'),
        ('9', '₉'),
        ('+', '₊'),
        ('-', '₋'),
        ('=', '₌'),
        ('(', '₍'),
        (')', '₎'),
        ('a', 'ₐ'),
        ('e', 'ₑ'),
        ('h', 'ₕ'),
        ('i', 'ᵢ'),
        ('j', 'ⱼ'),
        ('k', 'ₖ'),
        ('l', 'ₗ'),
        ('m', 'ₘ'),
        ('n', 'ₙ'),
        ('o', 'ₒ'),
        ('p', 'ₚ'),
        ('r', 'ᵣ'),
        ('s', 'ₛ'),
        ('t', 'ₜ'),
        ('u', 'ᵤ'),
        ('v', 'ᵥ'),
        ('x', 'ₓ'),
    ])
});

static SPACING_COMMANDS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        ",",
        ":",
        ";",
        " ",
        ">",
        "enspace",
        "enskip",
        "medspace",
        "quad",
        "qquad",
        "thickspace",
        "thinspace",
    ])
});
static NEGATIVE_SPACING_COMMANDS: LazyLock<IndexSet<&'static str>> =
    LazyLock::new(|| IndexSet::from(["!", "negmedspace", "negthickspace", "negthinspace"]));
const NEGATIVE_SPACE: &str = "\u{0000}";
static FONT_SWITCH_COMMANDS: LazyLock<IndexSet<&'static str>> =
    LazyLock::new(|| IndexSet::from(["bf", "cal", "it", "rm", "sf", "sl", "tt"]));
static IGNORED_COMMANDS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        "displaystyle",
        "limits",
        "nolimits",
        "scriptstyle",
        "scriptscriptstyle",
        "textstyle",
    ])
});
static SIZE_COMMANDS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        "big", "Big", "bigg", "Bigg", "bigl", "Bigl", "biggl", "Biggl", "bigr", "Bigr", "biggr", "Biggr",
    ])
});
static PLAIN_WRAPPERS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        "emph",
        "mathcal",
        "mathbf",
        "mathfrak",
        "mathit",
        "mathrm",
        "mathnormal",
        "mathscr",
        "mathsf",
        "mathtt",
        "mathup",
        "mbox",
        "overbrace",
        "pmb",
        "smash",
        "substack",
        "text",
        "textbf",
        "textit",
        "textmd",
        "textnormal",
        "textrm",
        "textsc",
        "textsf",
        "textsl",
        "texttt",
        "textup",
        "underbrace",
        "bm",
        "boldsymbol",
    ])
});
static ACCENTS: LazyLock<IndexMap<&'static str, &'static str>> = LazyLock::new(|| {
    IndexMap::from([
        ("acute", "\u{0301}"),
        ("bar", "\u{0305}"),
        ("breve", "\u{0306}"),
        ("check", "\u{030c}"),
        ("ddot", "\u{0308}"),
        ("dot", "\u{0307}"),
        ("grave", "\u{0300}"),
        ("hat", "\u{0302}"),
        ("mathring", "\u{030a}"),
        ("overleftarrow", "\u{20d6}"),
        ("overleftrightarrow", "\u{20e1}"),
        ("overline", "\u{0305}"),
        ("overrightarrow", "\u{20d7}"),
        ("tilde", "\u{0303}"),
        ("underline", "\u{0332}"),
        ("vec", "\u{20d7}"),
        ("widehat", "\u{0302}"),
        ("widetilde", "\u{0303}"),
    ])
});

static SCRIPT_OP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!("{ws}*([=+-]){ws}*", ws = pi_js::regex::JS_WS_CLASS)).expect("SCRIPT_OP_RE compiles")
});
static SIMPLE_ALNUM_DOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}.]+$").expect("SIMPLE_ALNUM_DOT_RE compiles"));
static SIMPLE_NUM_DOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{N}.]+$").expect("SIMPLE_NUM_DOT_RE compiles"));
static NAMED_OP_LEFT_RE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"(?<=[\p{L}\p{N})\]}\u{f0001}])\u{f0004}").expect("NAMED_OP_LEFT_RE compiles")
});
static NAMED_OP_RIGHT_RE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"\u{f0005}(?=[\p{L}\p{N}√\u{f0000}])").expect("NAMED_OP_RIGHT_RE compiles")
});
static HORIZ_SPACE_RUN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[ \t]+").expect("HORIZ_SPACE_RUN_RE compiles"));
static LAYOUT_MARKER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\u{f0000}([0-9]+)\u{f0001}").expect("LAYOUT_MARKER_RE compiles"));
static TRAILING_LAYOUT_MARKER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\u{f0000}([0-9]+)\u{f0001}$").expect("TRAILING_LAYOUT_MARKER_RE compiles"));
static ENV_ROW_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\\\(?:\[[^\]\n]*\])?").expect("ENV_ROW_SPLIT_RE compiles"));
static LEADING_BRACE_ARG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!("^{}*\\{{[^}}]*\\}}", pi_js::regex::JS_WS_CLASS)).expect("LEADING_BRACE_ARG_RE compiles")
});
static CASE_CONDITION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:if|when|for|otherwise)\b").expect("CASE_CONDITION_RE compiles"));

const NAMED_OPERATOR_START: &str = "\u{f0004}";
const NAMED_OPERATOR_END: &str = "\u{f0005}";
const LAYOUT_MARKER_START: &str = "\u{f0000}";
const LAYOUT_MARKER_END: &str = "\u{f0001}";
const PROTECTED_SPACE: &str = "\u{f0002}";

fn is_js_ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\x0b' | '\x0c' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScriptKind {
    Sub,
    Sup,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InlineLowerStyle {
    Bracket,
    Script,
}

fn replace_characters(value: &str, replacements: &IndexMap<char, char>) -> Option<String> {
    let mut result = String::with_capacity(value.len());
    for character in value.chars() {
        let replacement = replacements.get(&character)?;
        result.push(*replacement);
    }
    Some(result)
}

fn normalize_script_value(value: &str) -> String {
    SCRIPT_OP_RE.replace_all(pi_js::str16::trim(value), "$1").into_owned()
}

fn format_unicode_script(value: &str, kind: ScriptKind) -> Option<String> {
    let norm = normalize_script_value(value);
    match kind {
        ScriptKind::Sub => replace_characters(&norm, &SUBSCRIPTS),
        ScriptKind::Sup => replace_characters(&norm, &SUPERSCRIPTS),
    }
}

fn format_script(value: &str, kind: ScriptKind) -> String {
    let value = normalize_script_value(value);
    if let Some(unicode) = format_unicode_script(&value, kind) {
        return unicode;
    }

    let prefix = if kind == ScriptKind::Sub { "_" } else { "^" };
    if value.chars().count() == 1
        || (kind == ScriptKind::Sub && !value.is_empty() && value.chars().all(|c| c.is_ascii_alphabetic()))
    {
        return format!("{prefix}{value}");
    }
    format!("{prefix}({value})")
}

fn format_fraction(numerator: &str, denominator: &str) -> String {
    let numerator = pi_js::str16::trim(numerator);
    let denominator = pi_js::str16::trim(denominator);
    let simple_numerator = SIMPLE_ALNUM_DOT_RE.is_match(numerator);
    let simple_denominator = SIMPLE_NUM_DOT_RE.is_match(denominator) || denominator.chars().count() == 1;
    let num_str = if simple_numerator {
        numerator.to_string()
    } else {
        format!("({numerator})")
    };
    let den_str = if simple_denominator {
        denominator.to_string()
    } else {
        format!("({denominator})")
    };
    format!("{num_str}/{den_str}")
}

fn format_root(value: &str, symbol: &str) -> String {
    let value = pi_js::str16::trim(value);
    if SIMPLE_ALNUM_DOT_RE.is_match(value) {
        format!("{symbol}{value}")
    } else {
        format!("{symbol}({value})")
    }
}

fn normalize_output(value: &str) -> String {
    let step1 = NAMED_OP_LEFT_RE.replace_all(value, " ");
    let step2 = step1.replace(NAMED_OPERATOR_START, "");
    let step3 = NAMED_OP_RIGHT_RE.replace_all(&step2, " ");
    let step4 = step3.replace(NAMED_OPERATOR_END, "");
    let lines: Vec<String> = step4
        .split('\n')
        .map(|line| pi_js::str16::trim(&HORIZ_SPACE_RUN_RE.replace_all(line, " ")).to_string())
        .collect();
    let total = lines.len();
    let filtered: Vec<String> = lines
        .into_iter()
        .enumerate()
        .filter(|(index, line)| !line.is_empty() || (*index > 0 && *index < total - 1))
        .map(|(_, line)| line)
        .collect();
    pi_js::str16::trim(&filtered.join("\n")).to_string()
}

#[derive(Clone, Debug)]
enum LayoutNode {
    Fraction {
        numerator: String,
        denominator: String,
    },
    Operator {
        operator: String,
        lower: Option<String>,
        upper: Option<String>,
    },
    Script {
        lower: Option<String>,
        upper: Option<String>,
    },
    Matrix {
        lines: Vec<String>,
        baseline: usize,
    },
}

#[derive(Clone, Debug)]
struct Layout {
    lines: Vec<String>,
    width: usize,
    baseline: usize,
}

fn pad_layout_line(line: &str, width: usize, centered: bool) -> String {
    let vis = visible_width(line).max(0) as usize;
    let padding = width.saturating_sub(vis);
    let left = if centered { padding / 2 } else { 0 };
    format!("{}{}{}", " ".repeat(left), line, " ".repeat(padding - left))
}

fn join_layouts(layouts: &[Layout]) -> Layout {
    if layouts.is_empty() {
        return Layout {
            lines: vec![String::new()],
            width: 0,
            baseline: 0,
        };
    }
    let baseline = layouts.iter().map(|l| l.baseline).max().unwrap_or(0);
    let below = layouts
        .iter()
        .map(|l| l.lines.len().saturating_sub(l.baseline).saturating_sub(1))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::with_capacity(baseline + below + 1);
    for row in 0..=(baseline + below) {
        let mut line = String::new();
        for layout in layouts {
            let source_row = row as isize - baseline as isize + layout.baseline as isize;
            if source_row >= 0 && (source_row as usize) < layout.lines.len() {
                line.push_str(&pad_layout_line(
                    &layout.lines[source_row as usize],
                    layout.width,
                    false,
                ));
            } else {
                line.push_str(&" ".repeat(layout.width));
            }
        }
        lines.push(pi_js::str16::trim_end(&line).to_string());
    }
    Layout {
        lines,
        width: layouts.iter().map(|l| l.width).sum(),
        baseline,
    }
}

fn render_layout(source: &str, nodes: &[LayoutNode]) -> Layout {
    let mut rendered_lines: Vec<String> = Vec::new();
    let mut first_baseline = 0usize;
    for source_line in source.split('\n') {
        let mut layouts: Vec<Layout> = Vec::new();
        let mut position = 0usize;
        let mut previous_node: Option<&LayoutNode> = None;

        for caps in LAYOUT_MARKER_RE.captures_iter(source_line) {
            let full = caps.get(0).unwrap();
            let idx_str = caps.get(1).unwrap().as_str();
            let Ok(node_idx) = idx_str.parse::<usize>() else {
                continue;
            };
            let Some(node) = nodes.get(node_idx) else {
                continue;
            };
            let index = full.start();
            if index > position {
                let sliced = &source_line[position..index];
                let trimmed = pi_js::str16::trim_end(if previous_node.is_some() {
                    pi_js::str16::trim_start(sliced)
                } else {
                    sliced
                });
                let preserve_leading_space = matches!(previous_node, Some(LayoutNode::Matrix { .. }))
                    && sliced.chars().next().is_some_and(is_js_ws);
                let preserve_trailing_space =
                    matches!(node, LayoutNode::Matrix { .. }) && sliced.chars().next_back().is_some_and(is_js_ws);
                let text = if !trimmed.is_empty() {
                    format!(
                        "{}{}{}",
                        if preserve_leading_space { " " } else { "" },
                        trimmed,
                        if preserve_trailing_space { " " } else { "" }
                    )
                } else if preserve_leading_space || preserve_trailing_space {
                    " ".to_string()
                } else {
                    String::new()
                };
                let width = visible_width(&text).max(0) as usize;
                layouts.push(Layout {
                    lines: vec![text],
                    width,
                    baseline: 0,
                });
            }

            match node {
                LayoutNode::Fraction { numerator, denominator } => {
                    let num_layout = render_layout(numerator, nodes);
                    let den_layout = render_layout(denominator, nodes);
                    let content_width = num_layout.width.max(den_layout.width).max(1);
                    let width = content_width + 2;
                    let mut lines = Vec::with_capacity(num_layout.lines.len() + 1 + den_layout.lines.len());
                    for line in &num_layout.lines {
                        lines.push(pad_layout_line(line, width, true));
                    }
                    lines.push(format!(" {} ", "─".repeat(content_width)));
                    for line in &den_layout.lines {
                        lines.push(pad_layout_line(line, width, true));
                    }
                    layouts.push(Layout {
                        lines,
                        width,
                        baseline: num_layout.lines.len(),
                    });
                }
                LayoutNode::Operator { operator, lower, upper } => {
                    let op_w = visible_width(operator).max(0) as usize;
                    let low_w = lower.as_deref().map_or(0, |s| visible_width(s).max(0) as usize);
                    let up_w = upper.as_deref().map_or(0, |s| visible_width(s).max(0) as usize);
                    let content_width = op_w.max(low_w).max(up_w);
                    let mut lines = Vec::new();
                    if let Some(u) = upper {
                        lines.push(format!("{} ", pad_layout_line(u, content_width, true)));
                    }
                    lines.push(format!("{} ", pad_layout_line(operator, content_width, true)));
                    if let Some(l) = lower {
                        lines.push(format!("{} ", pad_layout_line(l, content_width, true)));
                    }
                    layouts.push(Layout {
                        lines,
                        width: content_width + 1,
                        baseline: if upper.is_some() { 1 } else { 0 },
                    });
                }
                LayoutNode::Script { lower, upper } => {
                    let up_layout = upper.as_deref().map(|s| render_layout(s, nodes));
                    let low_layout = lower.as_deref().map(|s| render_layout(s, nodes));
                    let width = up_layout
                        .as_ref()
                        .map_or(0, |l| l.width)
                        .max(low_layout.as_ref().map_or(0, |l| l.width));
                    let mut lines = Vec::new();
                    if let Some(u) = &up_layout {
                        for line in &u.lines {
                            lines.push(pad_layout_line(line, width, false));
                        }
                    }
                    lines.push(" ".repeat(width));
                    if let Some(l) = &low_layout {
                        for line in &l.lines {
                            lines.push(pad_layout_line(line, width, false));
                        }
                    }
                    layouts.push(Layout {
                        lines,
                        width,
                        baseline: up_layout.as_ref().map_or(0, |l| l.lines.len()),
                    });
                }
                LayoutNode::Matrix { lines, baseline } => {
                    let width = lines
                        .iter()
                        .map(|line| visible_width(line).max(0) as usize)
                        .max()
                        .unwrap_or(0);
                    layouts.push(Layout {
                        lines: lines.iter().map(|line| pad_layout_line(line, width, false)).collect(),
                        width,
                        baseline: *baseline,
                    });
                }
            }
            position = full.end();
            previous_node = Some(node);
        }

        if position < source_line.len() {
            let sliced = &source_line[position..];
            let trimmed = if previous_node.is_some() {
                pi_js::str16::trim_start(sliced)
            } else {
                sliced
            };
            let text = if matches!(previous_node, Some(LayoutNode::Matrix { .. }))
                && sliced.chars().next().is_some_and(is_js_ws)
            {
                format!(" {trimmed}")
            } else {
                trimmed.to_string()
            };
            let width = visible_width(&text).max(0) as usize;
            layouts.push(Layout {
                lines: vec![text],
                width,
                baseline: 0,
            });
        }

        let line_layout = join_layouts(&layouts);
        if rendered_lines.is_empty() {
            first_baseline = line_layout.baseline;
        }
        rendered_lines.extend(line_layout.lines);
    }

    let width = rendered_lines
        .iter()
        .map(|line| visible_width(line).max(0) as usize)
        .max()
        .unwrap_or(0);
    Layout {
        lines: rendered_lines,
        width,
        baseline: first_baseline,
    }
}

struct LatexParser<'a> {
    chars: Vec<char>,
    layout_nodes: &'a mut Vec<LayoutNode>,
    display: bool,
    position: usize,
    supported: bool,
    stack_fractions: bool,
    script_depth: usize,
}

impl<'a> LatexParser<'a> {
    fn new(source: &str, layout_nodes: &'a mut Vec<LayoutNode>, display: bool) -> Self {
        Self {
            chars: source.chars().collect(),
            layout_nodes,
            display,
            position: 0,
            supported: true,
            stack_fractions: true,
            script_depth: 0,
        }
    }

    fn render(mut self) -> Option<String> {
        let rendered = self.parse_sequence(None);
        if !self.supported || self.position != self.chars.len() {
            return None;
        }
        Some(normalize_output(&rendered))
    }

    fn slice_string(&self, start: usize, end: usize) -> String {
        self.chars[start..end].iter().collect()
    }

    fn find_subseq(&self, needle: &[char], from: usize) -> Option<usize> {
        if needle.is_empty() {
            return Some(from.min(self.chars.len()));
        }
        if from + needle.len() > self.chars.len() {
            return None;
        }
        for i in from..=(self.chars.len() - needle.len()) {
            if self.chars[i..i + needle.len()] == *needle {
                return Some(i);
            }
        }
        None
    }

    fn parse_sequence(&mut self, end_character: Option<char>) -> String {
        let mut result = String::new();
        while self.position < self.chars.len() {
            let character = self.chars[self.position];
            if end_character == Some(character) {
                self.position += 1;
                return result;
            }

            if character == '}' {
                self.supported = false;
                return result;
            }

            if character == '{' {
                self.position += 1;
                result.push_str(&self.parse_sequence(Some('}')));
                continue;
            }

            if character == '\\' {
                let command = self.parse_command();
                if command == NEGATIVE_SPACE {
                    result = pi_js::str16::trim_end(&result).to_string();
                    if let Some(stripped) = result.strip_suffix(NAMED_OPERATOR_END) {
                        result = stripped.to_string();
                    }
                } else {
                    result.push_str(&command);
                }
                continue;
            }

            if character == '^' || character == '_' {
                self.position += 1;
                result = pi_js::str16::trim_end(&result).to_string();
                let script = self.parse_scripts(character);
                if let Some(stripped) = result.strip_suffix(NAMED_OPERATOR_END) {
                    result = format!("{stripped}{script}{NAMED_OPERATOR_END}");
                } else {
                    result.push_str(&script);
                }
                continue;
            }

            if is_js_ws(character) {
                result.push_str(&self.parse_whitespace());
                continue;
            }

            if character == '=' || character == '<' || character == '>' {
                result = format!("{} {character} ", pi_js::str16::trim_end(&result));
                self.position += 1;
                continue;
            }

            if character == '&' {
                self.position += 1;
                continue;
            }

            if character == '~' {
                self.position += 1;
                result.push(' ');
                continue;
            }

            if character == '.' {
                if let Some(caps) = TRAILING_LAYOUT_MARKER_RE.captures(&result) {
                    if let Ok(idx) = caps[1].parse::<usize>() {
                        if let Some(LayoutNode::Matrix { lines, .. }) = self.layout_nodes.get_mut(idx) {
                            if let Some(last) = lines.last_mut() {
                                last.push(character);
                            }
                            self.position += 1;
                            continue;
                        }
                    }
                }
            }

            result.push(character);
            self.position += 1;
        }

        if end_character.is_some() {
            self.supported = false;
        }
        result
    }

    fn parse_scripts(&mut self, initial_marker: char) -> String {
        let mut sub: Option<String> = None;
        let mut sup: Option<String> = None;
        let mut order: Vec<ScriptKind> = Vec::with_capacity(2);

        let mut parse = |this: &mut Self, marker: char| {
            let kind = if marker == '_' {
                ScriptKind::Sub
            } else {
                ScriptKind::Sup
            };
            this.script_depth += 1;
            let parsed = this.parse_required_argument(false);
            this.script_depth -= 1;
            match kind {
                ScriptKind::Sub => sub = Some(parsed),
                ScriptKind::Sup => sup = Some(parsed),
            }
            order.push(kind);
        };

        parse(self, initial_marker);
        let mut next_position = self.position;
        while next_position < self.chars.len() && is_js_ws(self.chars[next_position]) {
            next_position += 1;
        }
        if let Some(&next_marker) = self.chars.get(next_position) {
            if (next_marker == '^' || next_marker == '_') && next_marker != initial_marker {
                self.position = next_position + 1;
                parse(self, next_marker);
            }
        }

        let sub_unicode = sub.as_deref().and_then(|s| format_unicode_script(s, ScriptKind::Sub));
        let sup_unicode = sup.as_deref().and_then(|s| format_unicode_script(s, ScriptKind::Sup));
        let can_use_layout = ![sub.as_deref(), sup.as_deref()].into_iter().any(|opt| {
            opt.is_some_and(|value| {
                value.contains('/')
                    || (!value.contains(LAYOUT_MARKER_START)
                        && value.chars().count() > 1
                        && !value.chars().any(|c| c.is_ascii_uppercase() || c == '*' || c == '∗'))
            })
        });
        let needs_layout = self.display
            && can_use_layout
            && (self.script_depth > 0
                || (sub.is_some() && sub_unicode.is_none())
                || (sup.is_some() && sup_unicode.is_none()));

        if !needs_layout {
            let mut out = String::new();
            for kind in order {
                match kind {
                    ScriptKind::Sub => {
                        if let Some(u) = &sub_unicode {
                            out.push_str(u);
                        } else {
                            out.push_str(&format_script(sub.as_deref().unwrap_or(""), ScriptKind::Sub));
                        }
                    }
                    ScriptKind::Sup => {
                        if let Some(u) = &sup_unicode {
                            out.push_str(u);
                        } else {
                            out.push_str(&format_script(sup.as_deref().unwrap_or(""), ScriptKind::Sup));
                        }
                    }
                }
            }
            return out;
        }

        let index = self.layout_nodes.len();
        self.layout_nodes.push(LayoutNode::Script {
            lower: sub.as_deref().map(normalize_output),
            upper: sup.as_deref().map(normalize_output),
        });
        format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}")
    }

    fn parse_whitespace(&mut self) -> String {
        while self.position < self.chars.len() && is_js_ws(self.chars[self.position]) {
            self.position += 1;
        }
        " ".to_string()
    }

    fn parse_command(&mut self) -> String {
        self.position += 1;
        if self.position >= self.chars.len() {
            self.supported = false;
            return String::new();
        }

        let first = self.chars[self.position];
        if first == '\n' || first == '\r' {
            self.position += 1;
            if first == '\r' && self.chars.get(self.position) == Some(&'\n') {
                self.position += 1;
            }
            return " ".to_string();
        }

        let command = if first.is_ascii_alphabetic() {
            let start = self.position;
            while self.position < self.chars.len() && self.chars[self.position].is_ascii_alphabetic() {
                self.position += 1;
            }
            self.slice_string(start, self.position)
        } else {
            self.position += 1;
            first.to_string()
        };

        if command == "\\" {
            return "\n".to_string();
        }
        if SPACING_COMMANDS.contains(command.as_str()) {
            return " ".to_string();
        }
        if NEGATIVE_SPACING_COMMANDS.contains(command.as_str()) {
            return NEGATIVE_SPACE.to_string();
        }
        if FONT_SWITCH_COMMANDS.contains(command.as_str()) {
            while self.position < self.chars.len() && is_js_ws(self.chars[self.position]) {
                self.position += 1;
            }
            return String::new();
        }
        if IGNORED_COMMANDS.contains(command.as_str()) {
            return String::new();
        }
        if matches!(command.as_str(), "{" | "}" | "$" | "%" | "#" | "_" | "&") {
            return command;
        }
        if command == "|" {
            return "‖".to_string();
        }
        if command == "not" {
            let raw = self.parse_required_argument(false);
            let value = pi_js::str16::trim(&raw);
            if let Some(&negated) = NEGATED_SYMBOLS.get(value) {
                return format!(" {negated} ");
            }
            let mut chars_iter = value.chars();
            let Some(first_ch) = chars_iter.next() else {
                self.supported = false;
                return String::new();
            };
            let rest: String = chars_iter.collect();
            return format!(" {first_ch}\u{0338}{rest} ");
        }
        if LIMIT_OPERATORS.contains(command.as_str()) {
            return self.parse_operator(&command, InlineLowerStyle::Bracket, true, true);
        }
        if let Some(&symbol) = SYMBOLS.get(command.as_str()) {
            if DISPLAY_LIMIT_SYMBOLS.contains(command.as_str()) {
                return self.parse_operator(symbol, InlineLowerStyle::Script, true, false);
            }
            return if command == "cdot" || command == "times" || RELATION_COMMANDS.contains(command.as_str()) {
                format!(" {symbol} ")
            } else {
                symbol.to_string()
            };
        }
        if NAMED_OPERATORS.contains(command.as_str()) {
            return format!("{NAMED_OPERATOR_START}{command}{NAMED_OPERATOR_END}");
        }
        if SIZE_COMMANDS.contains(command.as_str()) {
            return String::new();
        }
        if matches!(command.as_str(), "left" | "middle" | "right") {
            if self.chars.get(self.position) == Some(&'.') {
                self.position += 1;
            }
            return String::new();
        }
        if matches!(command.as_str(), "frac" | "dfrac" | "tfrac") {
            let should_stack = self.display && self.stack_fractions && command != "tfrac";
            let numerator = self.parse_required_argument(!should_stack);
            let denominator = self.parse_required_argument(!should_stack);
            if should_stack {
                let index = self.layout_nodes.len();
                self.layout_nodes.push(LayoutNode::Fraction {
                    numerator: normalize_output(&numerator),
                    denominator: normalize_output(&denominator),
                });
                return format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}");
            }
            return format_fraction(&numerator, &denominator);
        }
        if command == "sqrt" {
            let degree = self
                .parse_optional_argument()
                .map(|d| pi_js::str16::trim(&d).to_string());
            let value = self.parse_required_argument(true);
            return match degree.as_deref() {
                None | Some("2") => format_root(&value, "√"),
                Some("3") => format_root(&value, "∛"),
                Some("4") => format_root(&value, "∜"),
                Some(deg) => format!("{}{}", format_script(deg, ScriptKind::Sup), format_root(&value, "√")),
            };
        }
        if command == "boxed" || command == "fbox" {
            let inner = self.parse_required_argument(true);
            return format!("[{}]", pi_js::str16::trim(&inner));
        }
        if matches!(command.as_str(), "binom" | "dbinom" | "tbinom") {
            let n = self.parse_required_argument(true);
            let k = self.parse_required_argument(true);
            return format!("({n} choose {k})");
        }
        if let Some(&accent) = ACCENTS.get(command.as_str()) {
            let value = self.parse_required_argument(true);
            return if value.chars().count() == 1 {
                format!("{value}{accent}")
            } else {
                format!("{command}({value})")
            };
        }
        if command == "mathbb" {
            let value = self.parse_required_argument(true);
            let mut out = String::with_capacity(value.len());
            for ch in value.chars() {
                if let Some(&mapped) = BLACKBOARD.get(&ch) {
                    out.push_str(mapped);
                } else {
                    out.push(ch);
                }
            }
            return out;
        }
        if command == "operatorname" {
            let starred = self.chars.get(self.position) == Some(&'*');
            if starred {
                self.position += 1;
            }
            let raw_arg = self.parse_required_argument(true);
            let operator = pi_js::str16::trim(&normalize_output(&raw_arg)).to_string();
            return self.parse_operator(&operator, InlineLowerStyle::Bracket, starred, true);
        }
        if command == "mod" || command == "bmod" {
            return " mod ".to_string();
        }
        if command == "pmod" || command == "pod" {
            let raw = self.parse_required_argument(true);
            let value = pi_js::str16::trim(&raw);
            return if command == "pmod" {
                format!(" (mod {value})")
            } else {
                format!(" ({value})")
            };
        }
        if command == "overset" || command == "stackrel" {
            let upper = self.parse_required_argument(true);
            let raw_val = self.parse_required_argument(true);
            let value = pi_js::str16::trim(&raw_val);
            return format!("{value}{}", format_script(&upper, ScriptKind::Sup));
        }
        if command == "underset" {
            let lower = self.parse_required_argument(true);
            let raw_val = self.parse_required_argument(true);
            let value = pi_js::str16::trim(&raw_val);
            return format!("{value}{}", format_script(&lower, ScriptKind::Sub));
        }
        if PLAIN_WRAPPERS.contains(command.as_str()) {
            let value = self.parse_required_argument(true);
            return if command.starts_with("text") || command == "mbox" {
                value
            } else {
                pi_js::str16::trim(&value).to_string()
            };
        }
        if command == "begin" {
            return self.parse_environment();
        }
        if command == "end" {
            self.supported = false;
            return String::new();
        }

        self.supported = false;
        format!("\\{command}")
    }

    fn parse_operator(
        &mut self,
        operator: &str,
        inline_lower_style: InlineLowerStyle,
        display_limits: bool,
        spaced: bool,
    ) -> String {
        let mut use_display_limits = display_limits;
        let mut modifier_position = self.position;
        while modifier_position < self.chars.len()
            && (self.chars[modifier_position] == ' ' || self.chars[modifier_position] == '\t')
        {
            modifier_position += 1;
        }
        let check_modifier = |chars: &[char], pos: usize, kw: &[char]| -> Option<usize> {
            let len = 1 + kw.len();
            if pos + len <= chars.len() && chars[pos] == '\\' && chars[pos + 1..pos + len] == *kw {
                if !chars.get(pos + len).is_some_and(|c| c.is_ascii_alphabetic()) {
                    return Some(pos + len);
                }
            }
            None
        };
        if let Some(end) = check_modifier(&self.chars, modifier_position, &['l', 'i', 'm', 'i', 't', 's']) {
            use_display_limits = true;
            self.position = end;
        } else if let Some(end) = check_modifier(
            &self.chars,
            modifier_position,
            &['n', 'o', 'l', 'i', 'm', 'i', 't', 's'],
        ) {
            use_display_limits = false;
            self.position = end;
        }

        let mut lower: Option<String> = None;
        let mut upper: Option<String> = None;
        loop {
            let mut script_position = self.position;
            while script_position < self.chars.len()
                && (self.chars[script_position] == ' ' || self.chars[script_position] == '\t')
            {
                script_position += 1;
            }
            let Some(&kind) = self.chars.get(script_position) else {
                break;
            };
            if kind != '_' && kind != '^' {
                break;
            }
            self.position = script_position + 1;
            let arg = self.parse_required_argument(false);
            let value = normalize_output(&arg).replace(' ', "");
            if kind == '_' {
                if lower.is_some() {
                    self.supported = false;
                }
                lower = Some(value);
            } else {
                if upper.is_some() {
                    self.supported = false;
                }
                upper = Some(value);
            }
        }

        if self.display && use_display_limits && (lower.is_some() || upper.is_some()) {
            let index = self.layout_nodes.len();
            self.layout_nodes.push(LayoutNode::Operator {
                operator: operator.to_string(),
                lower,
                upper,
            });
            return format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}");
        }

        let mut rendered = operator.to_string();
        if let Some(l) = lower {
            match inline_lower_style {
                InlineLowerStyle::Bracket => rendered.push_str(&format!("[{l}]")),
                InlineLowerStyle::Script => rendered.push_str(&format_script(&l, ScriptKind::Sub)),
            }
        }
        if let Some(u) = upper {
            rendered.push_str(&format_script(&u, ScriptKind::Sup));
        }
        if spaced { format!(" {rendered} ") } else { rendered }
    }

    fn parse_required_argument(&mut self, stack_fractions: bool) -> String {
        let previous_stack_fractions = self.stack_fractions;
        self.stack_fractions = previous_stack_fractions && stack_fractions;
        let value = self.parse_required_argument_value();
        self.stack_fractions = previous_stack_fractions;
        value
    }

    fn parse_required_argument_value(&mut self) -> String {
        while self.position < self.chars.len() && is_js_ws(self.chars[self.position]) {
            self.position += 1;
        }
        if self.position >= self.chars.len() {
            self.supported = false;
            return String::new();
        }
        if self.chars[self.position] == '{' {
            self.position += 1;
            return self.parse_sequence(Some('}'));
        }
        if self.chars[self.position] == '\\' {
            return self.parse_command();
        }
        let value = self.chars[self.position].to_string();
        self.position += 1;
        value
    }

    fn parse_optional_argument(&mut self) -> Option<String> {
        while self.position < self.chars.len()
            && (self.chars[self.position] == ' ' || self.chars[self.position] == '\t')
        {
            self.position += 1;
        }
        if self.chars.get(self.position) != Some(&'[') {
            return None;
        }
        let Some(end) = self.find_subseq(&[']'], self.position + 1) else {
            self.supported = false;
            return None;
        };
        let value = self.slice_string(self.position + 1, end);
        self.position = end + 1;
        Some(self.render_nested(&value, true))
    }

    fn read_raw_group(&mut self) -> Option<String> {
        while self.position < self.chars.len()
            && (self.chars[self.position] == ' ' || self.chars[self.position] == '\t')
        {
            self.position += 1;
        }
        if self.chars.get(self.position) != Some(&'{') {
            self.supported = false;
            return None;
        }

        self.position += 1;
        let start = self.position;
        let mut depth = 1usize;
        while self.position < self.chars.len() {
            let character = self.chars[self.position];
            if character == '\\' {
                self.position += 2;
                continue;
            }
            if character == '{' {
                depth += 1;
            }
            if character == '}' {
                depth -= 1;
            }
            if depth == 0 {
                let value = self.slice_string(start, self.position);
                self.position += 1;
                return Some(value);
            }
            self.position += 1;
        }
        self.supported = false;
        None
    }

    fn split_environment_rows<'b>(&self, body: &'b str) -> Vec<&'b str> {
        ENV_ROW_SPLIT_RE.split(body).collect()
    }

    fn parse_environment(&mut self) -> String {
        let Some(environment) = self.read_raw_group().filter(|s| !s.is_empty()) else {
            return String::new();
        };
        let end_marker = format!("\\end{{{environment}}}");
        let marker_chars: Vec<char> = end_marker.chars().collect();
        let Some(end) = self.find_subseq(&marker_chars, self.position) else {
            self.supported = false;
            return String::new();
        };
        let body = self.slice_string(self.position, end);
        self.position = end + marker_chars.len();

        if matches!(environment.as_str(), "equation" | "equation*" | "displaymath") {
            let rendered = self.render_nested(&body, true);
            return pi_js::str16::trim(&rendered).to_string();
        }

        if matches!(
            environment.as_str(),
            "aligned"
                | "align"
                | "align*"
                | "alignedat"
                | "alignat"
                | "alignat*"
                | "gather"
                | "gathered"
                | "multline"
                | "multline*"
                | "split"
        ) {
            let aligned_at = matches!(environment.as_str(), "alignedat" | "alignat" | "alignat*");
            let aligned_body = if aligned_at {
                LEADING_BRACE_ARG_RE.replace(&body, "").into_owned()
            } else {
                body
            };
            let rows: Vec<String> = self
                .split_environment_rows(&aligned_body)
                .into_iter()
                .map(|row| {
                    let cells: Vec<&str> = row.split('&').collect();
                    let source = if aligned_at {
                        cells.chunks(2).map(|pair| pair.concat()).collect::<Vec<_>>().join(" ")
                    } else {
                        cells.concat()
                    };
                    pi_js::str16::trim(&self.render_nested(&source, true)).to_string()
                })
                .filter(|s| !s.is_empty())
                .collect();
            return rows.join("\n");
        }

        if environment == "cases" || environment == "cases*" {
            return self.render_cases(&body);
        }

        if matches!(
            environment.as_str(),
            "array" | "matrix" | "smallmatrix" | "pmatrix" | "bmatrix" | "Bmatrix" | "vmatrix" | "Vmatrix"
        ) {
            let matrix_body = if environment == "array" {
                LEADING_BRACE_ARG_RE.replace(&body, "").into_owned()
            } else {
                body
            };
            return self.render_matrix(&environment, &matrix_body);
        }

        self.supported = false;
        body
    }

    fn render_cases(&mut self, body: &str) -> String {
        let rows: Vec<Vec<String>> = self
            .split_environment_rows(body)
            .into_iter()
            .map(|row| {
                row.split('&')
                    .map(|cell| pi_js::str16::trim(&self.render_nested(cell, false)).to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|row| row.iter().any(|c| !c.is_empty()))
            .collect();

        let trim_trailing_comma = |s: &str| -> String {
            if let Some(comma_idx) = s.rfind(',') {
                if s[comma_idx + 1..].chars().all(is_js_ws) {
                    return s[..comma_idx].to_string();
                }
            }
            s.to_string()
        };

        let value_width = rows
            .iter()
            .map(|row| {
                let val = trim_trailing_comma(row.first().map_or("", String::as_str));
                visible_width(&val).max(0) as usize
            })
            .max()
            .unwrap_or(0);

        let contents: Vec<String> = rows
            .iter()
            .map(|row| {
                let value = trim_trailing_comma(row.first().map_or("", String::as_str));
                let condition = row.get(1).map_or("", String::as_str);
                if condition.is_empty() {
                    return value;
                }
                let condition_prefix = if CASE_CONDITION_RE.is_match(condition) {
                    " "
                } else {
                    " if "
                };
                let pad_count = value_width.saturating_sub(visible_width(&value).max(0) as usize);
                format!(
                    "{value}{}{condition_prefix}{condition}",
                    PROTECTED_SPACE.repeat(pad_count)
                )
            })
            .collect();

        if contents.len() <= 1 {
            return contents.first().map_or_else(String::new, |first| format!("⎧ {first}"));
        }

        let middle = contents.len() / 2;
        let mut visual_rows: Vec<Option<String>> = contents.into_iter().map(Some).collect();
        if visual_rows.len() % 2 == 0 {
            visual_rows.insert(middle, None);
        }
        let total = visual_rows.len();
        let lines: Vec<String> = visual_rows
            .into_iter()
            .enumerate()
            .map(|(index, content)| {
                let delimiter = if index == 0 {
                    "⎧"
                } else if index == total - 1 {
                    "⎩"
                } else {
                    "⎨"
                };
                match content {
                    None => delimiter.to_string(),
                    Some(text) => format!("{delimiter} {text}"),
                }
            })
            .collect();

        let index = self.layout_nodes.len();
        self.layout_nodes.push(LayoutNode::Matrix {
            lines,
            baseline: middle,
        });
        format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}")
    }

    fn render_matrix(&mut self, environment: &str, body: &str) -> String {
        let matrix: Vec<Vec<String>> = self
            .split_environment_rows(body)
            .into_iter()
            .map(|row| {
                row.split('&')
                    .map(|cell| pi_js::str16::trim(&self.render_nested(cell, false)).to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|row| row.iter().any(|c| !c.is_empty()))
            .collect();

        let column_count = matrix.iter().map(Vec::len).max().unwrap_or(0);
        let column_widths: Vec<usize> = (0..column_count)
            .map(|col| {
                matrix
                    .iter()
                    .map(|row| visible_width(row.get(col).map_or("", String::as_str)).max(0) as usize)
                    .max()
                    .unwrap_or(0)
            })
            .collect();

        let rows: Vec<String> = matrix
            .iter()
            .map(|row| {
                (0..column_count)
                    .map(|col| {
                        let cell = row.get(col).map_or("", String::as_str);
                        let vis = visible_width(cell).max(0) as usize;
                        let pad = column_widths[col].saturating_sub(vis);
                        format!("{cell}{}", PROTECTED_SPACE.repeat(pad))
                    })
                    .collect::<Vec<_>>()
                    .join(" │ ")
            })
            .collect();

        let lines: Vec<String> = if matches!(environment, "array" | "matrix" | "smallmatrix") {
            rows
        } else {
            let delimiter: Option<[&str; 6]> = match environment {
                "pmatrix" => Some(["⎛", "⎞", "⎜", "⎟", "⎝", "⎠"]),
                "bmatrix" => Some(["⎡", "⎤", "⎢", "⎥", "⎣", "⎦"]),
                "Bmatrix" => Some(["⎧", "⎫", "⎨", "⎬", "⎩", "⎭"]),
                "vmatrix" => Some(["│", "│", "│", "│", "│", "│"]),
                "Vmatrix" => Some(["║", "║", "║", "║", "║", "║"]),
                _ => None,
            };
            let Some(delimiter) = delimiter else {
                self.supported = false;
                return rows.join("\n");
            };
            let total = rows.len();
            rows.into_iter()
                .enumerate()
                .map(|(index, row)| {
                    let left = if index == 0 {
                        delimiter[0]
                    } else if index == total - 1 {
                        delimiter[4]
                    } else {
                        delimiter[2]
                    };
                    let right = if index == 0 {
                        delimiter[1]
                    } else if index == total - 1 {
                        delimiter[5]
                    } else {
                        delimiter[3]
                    };
                    format!("{left} {row} {right}")
                })
                .collect()
        };

        if lines.len() <= 1 {
            return lines.into_iter().next().unwrap_or_default();
        }
        let index = self.layout_nodes.len();
        self.layout_nodes.push(LayoutNode::Matrix { lines, baseline: 0 });
        format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}")
    }

    fn render_nested(&mut self, source: &str, stack_fractions: bool) -> String {
        if let Some(rendered) = LatexParser::new(source, self.layout_nodes, self.display && stack_fractions).render() {
            rendered
        } else {
            self.supported = false;
            source.to_string()
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RenderLatexOptions {
    /// Stack fractions and operator limits vertically for display math (default: false).
    pub display: Option<bool>,
}

/// Render a basic LaTeX math expression as terminal-friendly Unicode text.
/// Returns undefined when the expression contains unsupported or malformed syntax.
pub fn render_latex(source: &str, options: Option<RenderLatexOptions>) -> Option<String> {
    let display = options.and_then(|o| o.display).unwrap_or(false);
    let mut layout_nodes: Vec<LayoutNode> = Vec::new();
    let rendered = LatexParser::new(source, &mut layout_nodes, display).render()?;
    if layout_nodes.is_empty() {
        return Some(rendered.replace(PROTECTED_SPACE, " "));
    }
    let lines = render_layout(&rendered, &layout_nodes).lines;
    let indentation = lines
        .iter()
        .filter(|line| !pi_js::str16::trim(line).is_empty())
        .map(|line| pi_js::str16::len(line) - pi_js::str16::len(pi_js::str16::trim_start(line)))
        .min();
    let Some(indentation) = indentation else {
        return Some(String::new());
    };
    let sliced: Vec<String> = lines
        .into_iter()
        .map(|line| pi_js::str16::trim_end(&pi_js::str16::slice(&line, indentation as i64, None)).to_string())
        .collect();
    Some(pi_js::str16::trim_end(&sliced.join("\n")).replace(PROTECTED_SPACE, " "))
}

//! pi_js::regex (Rust-only; API contract: PORTING.md Appendix A).
//!
//! JS regular-expression support:
//!
//! - [`JS_WS_CLASS`] (plus [`JS_WS_CHARS`] / [`JS_NON_WS_CLASS`] / [`JS_DOT_CLASS`]) are the
//!   translations used when a TS regex literal is ported to the `regex` / `fancy_regex`
//!   crates (PORTING.md §13.2: `\s` → `JS_WS_CLASS`, `.` → `[^\n\r\u{2028}\u{2029}]`).
//! - [`ecma`] compiles a user- or data-provided pattern with ECMAScript semantics (`regress`),
//!   the way `new RegExp(pattern, flags)` does in Node 24 / V8. Invalid flags and invalid
//!   patterns fail with the exact V8 `SyntaxError` text, e.g.
//!   `Invalid regular expression: /(/i: Unterminated group`.
//! - [`replace`] / [`replace_with`] / [`test`] / [`exec`] / [`match_all`] give
//!   `String.prototype.replace` / `RegExp.prototype.test` / `exec` / `String.prototype.matchAll`
//!   semantics on top of a compiled `regress::Regex` (GetSubstitution `$` patterns, UTF-16
//!   indices).

use crate::error::{Error, Result};

/// The JS `\s` class (WhiteSpace + LineTerminator) as a bracketed character class that works in
/// the `regex`, `fancy_regex` and `regress` crates. The class holds the literal characters
/// (not escapes), so it is valid in every engine; do not use it in `(?x)` mode, where literal
/// whitespace is ignored.
pub const JS_WS_CLASS: &str =
    "[\t\n\u{b}\u{c}\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";

/// The members of [`JS_WS_CLASS`] without the surrounding brackets, for embedding `\s` inside
/// another class (`[^\s,]` → `format!("[^{JS_WS_CHARS},]")`).
pub const JS_WS_CHARS: &str =
    "\t\n\u{b}\u{c}\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";

/// The JS `\S` class: everything except [`JS_WS_CHARS`].
pub const JS_NON_WS_CLASS: &str =
    "[^\t\n\u{b}\u{c}\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";

/// The JS `.` (without the `s` flag): every character except the line terminators.
pub const JS_DOT_CLASS: &str = "[^\n\r\u{2028}\u{2029}]";

/// Parsed and validated JS RegExp flags (`d g i m s u v y`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JsFlags {
    pub has_indices: bool,
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub unicode: bool,
    pub unicode_sets: bool,
    pub sticky: bool,
}

impl JsFlags {
    /// Validates `flags` like the `RegExp` constructor. Fails with
    /// `SyntaxError: Invalid flags supplied to RegExp constructor '<flags>'` on an unknown or
    /// repeated flag, or when `u` and `v` are combined.
    pub fn parse(flags: &str) -> Result<JsFlags> {
        let mut out = JsFlags::default();
        for c in flags.chars() {
            let slot = match c {
                'd' => &mut out.has_indices,
                'g' => &mut out.global,
                'i' => &mut out.ignore_case,
                'm' => &mut out.multiline,
                's' => &mut out.dot_all,
                'u' => &mut out.unicode,
                'v' => &mut out.unicode_sets,
                'y' => &mut out.sticky,
                _ => return Err(invalid_flags(flags)),
            };
            if *slot {
                return Err(invalid_flags(flags));
            }
            *slot = true;
        }
        if out.unicode && out.unicode_sets {
            return Err(invalid_flags(flags));
        }
        Ok(out)
    }

    /// `RegExp.prototype.flags`: the canonical flag string (`dgimsuvy` order).
    pub fn canonical(&self) -> String {
        let mut s = String::new();
        for (on, c) in [
            (self.has_indices, 'd'),
            (self.global, 'g'),
            (self.ignore_case, 'i'),
            (self.multiline, 'm'),
            (self.dot_all, 's'),
            (self.unicode, 'u'),
            (self.unicode_sets, 'v'),
            (self.sticky, 'y'),
        ] {
            if on {
                s.push(c);
            }
        }
        s
    }

    /// True for `u` or `v` (the spec's "UnicodeMode").
    pub fn unicode_mode(&self) -> bool {
        self.unicode || self.unicode_sets
    }

    /// The compile flags `regress` understands (`g`, `y` and `d` only affect matching).
    pub fn to_regress(&self) -> regress::Flags {
        regress::Flags {
            icase: self.ignore_case,
            multiline: self.multiline,
            dot_all: self.dot_all,
            no_opt: false,
            unicode: self.unicode,
            unicode_sets: self.unicode_sets,
        }
    }
}

fn invalid_flags(flags: &str) -> Error {
    Error::js(
        "SyntaxError",
        format!("Invalid flags supplied to RegExp constructor '{flags}'"),
    )
}

fn invalid_pattern(pattern: &str, flags: &JsFlags, message: &str) -> Error {
    Error::js(
        "SyntaxError",
        format!(
            "Invalid regular expression: /{pattern}/{}: {message}",
            flags.canonical()
        ),
    )
}

/// `new RegExp(pattern, flags)` with ECMAScript semantics (regress).
///
/// Flag and syntax errors are `SyntaxError`s with the V8 message text. The `g`, `y` and `d`
/// flags are validated but do not change the compiled regex; use [`exec`] / [`match_all`] /
/// [`replace`] for their matching semantics.
pub fn ecma(pattern: &str, flags: &str) -> Result<regress::Regex> {
    let parsed = JsFlags::parse(flags)?;
    if let Some(message) = v8::check(pattern, &parsed) {
        return Err(invalid_pattern(pattern, &parsed, message));
    }
    regress::Regex::with_flags(pattern, parsed.to_regress()).map_err(|e| {
        // PORT: V8 accepted the syntax but regress rejected it (e.g. a Unicode property V8
        // knows and regress does not); report regress's reason mapped to the closest V8 text.
        invalid_pattern(pattern, &parsed, map_regress_error(&e.text))
    })
}

/// Checks `pattern` / `flags` like `new RegExp` without compiling. Returns the `SyntaxError`
/// V8 would throw.
pub fn validate(pattern: &str, flags: &str) -> Result<()> {
    ecma(pattern, flags).map(|_| ())
}

fn map_regress_error(text: &str) -> &'static str {
    match text {
        "Unbalanced parenthesis" => "Unterminated group",
        "Incomplete escape" | "Unterminated escape" => "\\ at end of pattern",
        "Invalid token at named capture group identifier" => "Invalid capture group name",
        "Duplicate capture group name" => "Duplicate capture group name",
        "Invalid braced quantifier" | "Invalid quantifier" => "Incomplete quantifier",
        "Quantifier not allowed here" => "Nothing to repeat",
        "Unbalanced bracket" => "Unterminated character class",
        "Invalid character range" => "Range out of order in character class",
        "Invalid property escape" | "Invalid character at property escape start" | "Invalid property name" => {
            "Invalid property name"
        }
        "Invalid unicode escape" => "Invalid Unicode escape",
        "Invalid named backreference syntax" => "Invalid named reference",
        "Capture group count limit exceeded" => "Too many captures",
        "Regular expression is too deeply nested" => "Maximum call stack size exceeded",
        "Invalid group modifier" => "Invalid group",
        "Invalid class set range" => "Range out of order in character class",
        "Unexpected character in class set intersection" | "Unexpected character in class set subtraction" => {
            "Invalid set operation in character class"
        }
        "Empty class set operand" | "Invalid class set character" | "Incomplete class set character" => {
            "Invalid character in character class"
        }
        "Unbalanced class set bracket" => "Unterminated character class",
        _ => "Invalid escape",
    }
}

/// One match in JS terms. Indices are UTF-16 code units (as `match.index` / `lastIndex`);
/// `byte_*` are the UTF-8 offsets into the searched `&str`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsMatch {
    /// `match[0]`.
    pub matched: String,
    /// `match.index` (UTF-16).
    pub index: usize,
    /// UTF-8 byte range of `match[0]`.
    pub byte_start: usize,
    pub byte_end: usize,
    /// `match[1..]`; `None` for groups that did not participate (`undefined`).
    pub captures: Vec<Option<String>>,
    /// `match.groups` in pattern order; `None` when the pattern has no named groups.
    pub groups: Option<Vec<(String, Option<String>)>>,
}

impl JsMatch {
    fn from_regress(m: &regress::Match, text: &str) -> JsMatch {
        let captures = m
            .captures
            .iter()
            .map(|g| g.as_ref().map(|r| text[r.clone()].to_string()))
            .collect();
        let named: Vec<(String, Option<String>)> = m
            .named_groups()
            .map(|(name, r)| (name.to_string(), r.map(|r| text[r].to_string())))
            .collect();
        JsMatch {
            matched: text[m.range()].to_string(),
            index: utf16_len(&text[..m.start()]),
            byte_start: m.start(),
            byte_end: m.end(),
            captures,
            groups: if named.is_empty() { None } else { Some(named) },
        }
    }

    /// `match.groups[name]`.
    pub fn group(&self, name: &str) -> Option<&str> {
        self.groups.as_ref()?.iter().find(|(n, _)| n == name)?.1.as_deref()
    }
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `regex.test(text)` for a regex without `g` / `y` (stateless).
pub fn test(re: &regress::Regex, text: &str) -> bool {
    re.find(text).is_some()
}

/// `regex.exec(text)` for a regex without `g` / `y` (stateless).
pub fn exec(re: &regress::Regex, text: &str) -> Option<JsMatch> {
    re.find(text).map(|m| JsMatch::from_regress(&m, text))
}

/// `[...text.matchAll(regex)]` (the regex must be global in JS; here every match is returned).
pub fn match_all(re: &regress::Regex, text: &str) -> Vec<JsMatch> {
    re.find_iter(text).map(|m| JsMatch::from_regress(&m, text)).collect()
}

/// `text.replace(regex, replacement)` with a string replacement. `global` mirrors the `g` flag
/// (replace every match instead of the first). The replacement understands the JS
/// GetSubstitution patterns `$$`, `$&`, `` $` ``, `$'`, `$n`, `$nn` and `$<name>`.
pub fn replace(re: &regress::Regex, text: &str, replacement: &str, global: bool) -> String {
    replace_impl(re, text, global, |m| get_substitution(m, text, replacement))
}

/// `text.replace(regex, fn)`: the callback receives each match and returns the replacement.
pub fn replace_with(re: &regress::Regex, text: &str, global: bool, mut f: impl FnMut(&JsMatch) -> String) -> String {
    replace_impl(re, text, global, |m| f(&JsMatch::from_regress(m, text)))
}

fn replace_impl(re: &regress::Regex, text: &str, global: bool, mut f: impl FnMut(&regress::Match) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    if global {
        for m in re.find_iter(text) {
            out.push_str(&text[last..m.start()]);
            out.push_str(&f(&m));
            last = m.end();
        }
    } else if let Some(m) = re.find(text) {
        out.push_str(&text[..m.start()]);
        out.push_str(&f(&m));
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

/// ES `GetSubstitution` for a regress match.
fn get_substitution(m: &regress::Match, text: &str, replacement: &str) -> String {
    let group_count = m.captures.len();
    let has_named = m.named_groups().next().is_some();
    let bytes = replacement.as_bytes();
    let mut out = String::with_capacity(replacement.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'$' || i + 1 >= bytes.len() {
            let ch_len = utf8_char_len(b);
            out.push_str(&replacement[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        let next = bytes[i + 1];
        match next {
            b'$' => {
                out.push('$');
                i += 2;
            }
            b'&' => {
                out.push_str(&text[m.range()]);
                i += 2;
            }
            b'`' => {
                out.push_str(&text[..m.start()]);
                i += 2;
            }
            b'\'' => {
                out.push_str(&text[m.end()..]);
                i += 2;
            }
            b'0'..=b'9' => {
                let d1 = (next - b'0') as usize;
                let two = bytes
                    .get(i + 2)
                    .filter(|c| c.is_ascii_digit())
                    .map(|c| d1 * 10 + (c - b'0') as usize);
                if let Some(n) = two.filter(|n| *n >= 1 && *n <= group_count) {
                    if let Some(r) = &m.captures[n - 1] {
                        out.push_str(&text[r.clone()]);
                    }
                    i += 3;
                } else if d1 >= 1 && d1 <= group_count {
                    if let Some(r) = &m.captures[d1 - 1] {
                        out.push_str(&text[r.clone()]);
                    }
                    i += 2;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            b'<' if has_named => match replacement[i + 2..].find('>') {
                Some(close) => {
                    let name = &replacement[i + 2..i + 2 + close];
                    if let Some(r) = m.named_group(name) {
                        out.push_str(&text[r]);
                    }
                    i += 2 + close + 1;
                }
                None => {
                    out.push('$');
                    i += 1;
                }
            },
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

fn utf8_char_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Escapes regex syntax characters (the common `s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")`).
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A syntax checker that mirrors V8's `RegExpParser` (Node 24) closely enough to report the
/// same first error with the same message. Semantics (matching) stay with regress.
mod v8 {
    use super::JsFlags;

    const END: u32 = u32::MAX;
    const K_INFINITY: i64 = i32::MAX as i64;
    const K_MAX_CAPTURES: i64 = (1 << 16) - 1;

    pub(super) fn check(pattern: &str, flags: &JsFlags) -> Option<&'static str> {
        let mut p = Parser::new(pattern, flags);
        p.parse_pattern().err()
    }

    type R<T> = std::result::Result<T, &'static str>;

    const UNTERMINATED_GROUP: &str = "Unterminated group";
    const UNMATCHED_PAREN: &str = "Unmatched ')'";
    const ESCAPE_AT_END: &str = "\\ at end of pattern";
    const INVALID_PROPERTY_NAME: &str = "Invalid property name";
    const INVALID_ESCAPE: &str = "Invalid escape";
    const INVALID_DECIMAL_ESCAPE: &str = "Invalid decimal escape";
    const INVALID_UNICODE_ESCAPE: &str = "Invalid Unicode escape";
    const NOTHING_TO_REPEAT: &str = "Nothing to repeat";
    const LONE_QUANTIFIER_BRACKETS: &str = "Lone quantifier brackets";
    const RANGE_OUT_OF_ORDER: &str = "numbers out of order in {} quantifier";
    const INCOMPLETE_QUANTIFIER: &str = "Incomplete quantifier";
    const INVALID_QUANTIFIER: &str = "Invalid quantifier";
    const INVALID_GROUP: &str = "Invalid group";
    const MULTIPLE_FLAG_DASHES: &str = "Multiple dashes in flag group";
    const REPEATED_FLAG: &str = "Repeated flag in flag group";
    const INVALID_FLAG_GROUP: &str = "Invalid flag group";
    const TOO_MANY_CAPTURES: &str = "Too many captures";
    const INVALID_CAPTURE_GROUP_NAME: &str = "Invalid capture group name";
    const DUPLICATE_CAPTURE_GROUP_NAME: &str = "Duplicate capture group name";
    const INVALID_NAMED_REFERENCE: &str = "Invalid named reference";
    const INVALID_NAMED_CAPTURE_REFERENCED: &str = "Invalid named capture referenced";
    const INVALID_CLASS_PROPERTY_NAME: &str = "Invalid property name in character class";
    const INVALID_CHARACTER_CLASS: &str = "Invalid character class";
    const UNTERMINATED_CHARACTER_CLASS: &str = "Unterminated character class";
    const OUT_OF_ORDER_CHARACTER_CLASS: &str = "Range out of order in character class";
    const INVALID_CLASS_SET_OPERATION: &str = "Invalid set operation in character class";
    const INVALID_CHARACTER_IN_CLASS: &str = "Invalid character in character class";
    const NEGATED_CLASS_WITH_STRINGS: &str = "Negated character class may contain strings";

    /// Properties of strings (only valid with `v`, never negated).
    const STRING_PROPERTIES: &[&str] = &[
        "Basic_Emoji",
        "Emoji_Keycap_Sequence",
        "RGI_Emoji_Modifier_Sequence",
        "RGI_Emoji_Flag_Sequence",
        "RGI_Emoji_Tag_Sequence",
        "RGI_Emoji_ZWJ_Sequence",
        "RGI_Emoji",
    ];

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum GroupKind {
        Capture,
        Grouping,
        Lookahead,
        Lookbehind,
    }

    struct GroupState {
        kind: GroupKind,
        name: Option<String>,
    }

    /// What the last parsed term was, for quantifier validity.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Last {
        Atom,
        Lookahead,
        Lookbehind,
    }

    struct Parser {
        chars: Vec<u32>,
        pos: usize,
        unicode: bool,
        unicode_sets: bool,
        // Lazily computed by `scan_for_captures`.
        scanned: bool,
        capture_count: i64,
        has_named_captures: bool,
        captures_started: i64,
        named: Vec<(String, Vec<(usize, usize)>)>,
        named_refs: Vec<String>,
        next_disjunction: usize,
        /// The last `v`-mode class set character, for ranges.
        last_char: u32,
    }

    impl Parser {
        fn new(src: &str, flags: &JsFlags) -> Parser {
            // Non-unicode patterns are parsed per UTF-16 unit in V8; a surrogate pair in a
            // `&str` is one char here, which only matters for ranges built from lone halves.
            Parser {
                chars: src.chars().map(u32::from).collect(),
                pos: 0,
                unicode: flags.unicode_mode(),
                unicode_sets: flags.unicode_sets,
                scanned: false,
                capture_count: 0,
                has_named_captures: false,
                captures_started: 0,
                named: Vec::new(),
                named_refs: Vec::new(),
                next_disjunction: 1,
                last_char: 0,
            }
        }

        fn current(&self) -> u32 {
            self.chars.get(self.pos).copied().unwrap_or(END)
        }

        fn next(&self) -> u32 {
            self.chars.get(self.pos + 1).copied().unwrap_or(END)
        }

        fn at(&self, i: usize) -> u32 {
            self.chars.get(i).copied().unwrap_or(END)
        }

        fn advance(&mut self) {
            if self.pos < self.chars.len() {
                self.pos += 1;
            } else {
                self.pos = self.chars.len() + 1;
            }
        }

        fn advance_by(&mut self, n: usize) {
            for _ in 0..n {
                self.advance();
            }
        }

        fn has_more(&self) -> bool {
            self.pos < self.chars.len()
        }

        fn is(&self, c: char) -> bool {
            self.current() == c as u32
        }

        fn has_named_captures(&mut self) -> bool {
            if self.has_named_captures {
                return true;
            }
            self.scan_for_captures();
            self.has_named_captures
        }

        fn capture_count(&mut self) -> i64 {
            self.scan_for_captures();
            self.capture_count
        }

        /// V8 `ScanForCaptures`: counts capture groups and detects named ones.
        fn scan_for_captures(&mut self) {
            if self.scanned {
                return;
            }
            self.scanned = true;
            let mut count = 0;
            let mut i = 0;
            let mut class_depth = 0usize;
            while i < self.chars.len() {
                let c = self.chars[i];
                match char::from_u32(c).unwrap_or('\0') {
                    '\\' => {
                        i += 2;
                        continue;
                    }
                    '[' => {
                        if self.unicode_sets {
                            class_depth += 1;
                        } else if class_depth == 0 {
                            class_depth = 1;
                        }
                    }
                    ']' => {
                        class_depth = class_depth.saturating_sub(1);
                    }
                    '(' if class_depth == 0 => {
                        if self.at(i + 1) == '?' as u32 {
                            if self.at(i + 2) == '<' as u32
                                && self.at(i + 3) != '=' as u32
                                && self.at(i + 3) != '!' as u32
                            {
                                count += 1;
                                self.has_named_captures = true;
                            }
                        } else {
                            count += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            self.capture_count = count;
        }

        fn parse_pattern(&mut self) -> R<()> {
            let top = 0usize;
            let mut stack: Vec<GroupState> = Vec::new();
            // (disjunction id, alternative index) of the innermost open body.
            let mut disjunction = top;
            let mut alternative = 0usize;
            let mut path: Vec<(usize, usize)> = Vec::new();
            loop {
                let mut last = Last::Atom;
                let c = self.current();
                if c == END {
                    if !stack.is_empty() {
                        return Err(UNTERMINATED_GROUP);
                    }
                    for name in &self.named_refs {
                        if !self.named.iter().any(|(n, _)| n == name) {
                            return Err(INVALID_NAMED_CAPTURE_REFERENCED);
                        }
                    }
                    return Ok(());
                }
                match char::from_u32(c).unwrap_or('\0') {
                    ')' => {
                        let Some(group) = stack.pop() else {
                            return Err(UNMATCHED_PAREN);
                        };
                        self.advance();
                        let (outer_disjunction, outer_alternative) = path.pop().unwrap_or((top, 0));
                        if let Some(name) = group.name {
                            // Duplicate names are allowed only in different alternatives.
                            let mut my_path = path.clone();
                            my_path.push((outer_disjunction, outer_alternative));
                            for (other, other_path) in &self.named {
                                if *other != name {
                                    continue;
                                }
                                let disjoint = my_path
                                    .iter()
                                    .any(|(d, a)| other_path.iter().any(|(od, oa)| od == d && oa != a));
                                if !disjoint {
                                    return Err(DUPLICATE_CAPTURE_GROUP_NAME);
                                }
                            }
                            self.named.push((name, my_path));
                        }
                        disjunction = outer_disjunction;
                        alternative = outer_alternative;
                        last = match group.kind {
                            GroupKind::Lookahead => Last::Lookahead,
                            GroupKind::Lookbehind => Last::Lookbehind,
                            GroupKind::Capture | GroupKind::Grouping => Last::Atom,
                        };
                    }
                    '|' => {
                        self.advance();
                        alternative += 1;
                        continue;
                    }
                    '*' | '+' | '?' => return Err(NOTHING_TO_REPEAT),
                    '^' | '$' => {
                        self.advance();
                        continue;
                    }
                    '.' => {
                        self.advance();
                    }
                    '(' => {
                        let group = self.parse_open_parenthesis()?;
                        path.push((disjunction, alternative));
                        let id = self.next_disjunction;
                        self.next_disjunction += 1;
                        stack.push(GroupState {
                            kind: group.0,
                            name: group.1,
                        });
                        disjunction = id;
                        alternative = 0;
                        continue;
                    }
                    '[' => {
                        self.parse_character_class()?;
                    }
                    '\\' => match char::from_u32(self.next()).unwrap_or('\0') {
                        _ if self.next() == END => return Err(ESCAPE_AT_END),
                        'b' | 'B' => {
                            self.advance_by(2);
                            continue;
                        }
                        '1'..='9' => {
                            if self.parse_back_reference_index()? {
                            } else {
                                if self.unicode {
                                    return Err(INVALID_ESCAPE);
                                }
                                let first_digit = self.next();
                                if first_digit == '8' as u32 || first_digit == '9' as u32 {
                                    self.advance_by(2);
                                } else {
                                    self.advance();
                                    self.parse_octal_literal();
                                }
                            }
                        }
                        '0' => {
                            self.advance();
                            if self.unicode && is_decimal(self.next()) {
                                return Err(INVALID_DECIMAL_ESCAPE);
                            }
                            self.parse_octal_literal();
                        }
                        'k' if self.unicode || self.has_named_captures() => {
                            self.advance_by(2);
                            self.parse_named_back_reference()?;
                        }
                        n => {
                            if matches!(n, 'd' | 'D' | 's' | 'S' | 'w' | 'W') {
                                self.advance_by(2);
                            } else if matches!(n, 'p' | 'P') && self.unicode {
                                self.advance_by(2);
                                self.parse_property(n == 'P', false)?;
                            } else {
                                self.parse_character_escape(false)?;
                            }
                        }
                    },
                    '{' => {
                        if self.parse_interval_quantifier().is_some() {
                            return Err(NOTHING_TO_REPEAT);
                        }
                        if self.unicode {
                            return Err(LONE_QUANTIFIER_BRACKETS);
                        }
                        self.advance();
                    }
                    '}' | ']' => {
                        if self.unicode {
                            return Err(LONE_QUANTIFIER_BRACKETS);
                        }
                        self.advance();
                    }
                    _ => {
                        self.advance();
                    }
                }
                // Parse a quantifier (if any).
                match char::from_u32(self.current()).unwrap_or('\0') {
                    '*' | '+' | '?' => self.advance(),
                    '{' => match self.parse_interval_quantifier() {
                        Some((min, max)) => {
                            if max < min {
                                return Err(RANGE_OUT_OF_ORDER);
                            }
                        }
                        None => {
                            if self.unicode {
                                return Err(INCOMPLETE_QUANTIFIER);
                            }
                            continue;
                        }
                    },
                    _ => continue,
                }
                if self.is('?') {
                    self.advance();
                }
                match last {
                    Last::Lookbehind => return Err(INVALID_QUANTIFIER),
                    Last::Lookahead if self.unicode => return Err(INVALID_QUANTIFIER),
                    Last::Lookahead | Last::Atom => {}
                }
            }
        }

        /// Returns (kind, name) of the group opened at the current `(`.
        fn parse_open_parenthesis(&mut self) -> R<(GroupKind, Option<String>)> {
            self.advance();
            let mut kind = GroupKind::Capture;
            let mut is_named = false;
            if self.is('?') {
                match char::from_u32(self.next()).unwrap_or('\0') {
                    ':' => {
                        self.advance_by(2);
                        kind = GroupKind::Grouping;
                    }
                    '=' | '!' => {
                        self.advance_by(2);
                        kind = GroupKind::Lookahead;
                    }
                    '-' | 'i' | 's' | 'm' => {
                        self.advance();
                        self.parse_flag_modifiers()?;
                        kind = GroupKind::Grouping;
                    }
                    '<' => {
                        self.advance();
                        if self.next() == '=' as u32 || self.next() == '!' as u32 {
                            self.advance_by(2);
                            kind = GroupKind::Lookbehind;
                        } else {
                            is_named = true;
                            self.has_named_captures = true;
                            self.advance();
                        }
                    }
                    _ => return Err(INVALID_GROUP),
                }
            }
            let mut name = None;
            if kind == GroupKind::Capture {
                if self.captures_started >= K_MAX_CAPTURES {
                    return Err(TOO_MANY_CAPTURES);
                }
                self.captures_started += 1;
                if is_named {
                    name = Some(self.parse_capture_group_name()?);
                }
            }
            Ok((kind, name))
        }

        /// `(?ims-ims:` modifiers; the current char is the first modifier char.
        fn parse_flag_modifiers(&mut self) -> R<()> {
            let mut sense_on = true;
            let mut seen = 0u8;
            loop {
                match char::from_u32(self.current()).unwrap_or('\0') {
                    '-' => {
                        if !sense_on {
                            return Err(MULTIPLE_FLAG_DASHES);
                        }
                        sense_on = false;
                        self.advance();
                    }
                    c @ ('i' | 's' | 'm') => {
                        let bit = match c {
                            'i' => 1,
                            's' => 2,
                            _ => 4,
                        };
                        if seen & bit != 0 {
                            return Err(REPEATED_FLAG);
                        }
                        seen |= bit;
                        self.advance();
                    }
                    ':' => {
                        if seen == 0 {
                            return Err(INVALID_FLAG_GROUP);
                        }
                        self.advance();
                        return Ok(());
                    }
                    _ => return Err(INVALID_GROUP),
                }
            }
        }

        /// V8 `ParseCaptureGroupName`; the current char is the first name char.
        fn parse_capture_group_name(&mut self) -> R<String> {
            let mut name = String::new();
            let mut at_start = true;
            loop {
                let mut c = self.current();
                self.advance();
                if c == '\\' as u32 && self.is('u') {
                    self.advance();
                    match self.parse_unicode_escape(true) {
                        Some(v) => c = v,
                        None => return Err(INVALID_UNICODE_ESCAPE),
                    }
                } else if (0xD800..0xDC00).contains(&c) {
                    // A lone lead surrogate cannot occur in a &str; nothing to combine.
                }
                if c == '\\' as u32 {
                    return Err(INVALID_CAPTURE_GROUP_NAME);
                }
                if at_start {
                    if !is_identifier_start(c) {
                        return Err(INVALID_CAPTURE_GROUP_NAME);
                    }
                    push_cp(&mut name, c);
                    at_start = false;
                } else if c == '>' as u32 {
                    return Ok(name);
                } else if is_identifier_part(c) {
                    push_cp(&mut name, c);
                } else {
                    return Err(INVALID_CAPTURE_GROUP_NAME);
                }
            }
        }

        fn parse_named_back_reference(&mut self) -> R<()> {
            if !self.is('<') {
                return Err(INVALID_NAMED_REFERENCE);
            }
            self.advance();
            let name = self.parse_capture_group_name()?;
            self.named_refs.push(name);
            Ok(())
        }

        /// V8 `ParseBackReferenceIndex`; the current char is `\`.
        fn parse_back_reference_index(&mut self) -> R<bool> {
            let start = self.pos;
            self.advance();
            let mut value: i64 = 0;
            while is_decimal(self.current()) {
                value = value * 10 + (self.current() - '0' as u32) as i64;
                if value > K_MAX_CAPTURES {
                    self.pos = start;
                    return Ok(false);
                }
                self.advance();
            }
            if value > self.captures_started && value > self.capture_count() {
                self.pos = start;
                return Ok(false);
            }
            Ok(true)
        }

        /// V8 `ParseOctalLiteral` (the value is irrelevant for validation); the current char
        /// is the first digit.
        fn parse_octal_literal(&mut self) {
            if is_octal(self.current()) {
                let value = self.current() - '0' as u32;
                self.advance();
                if is_octal(self.current()) {
                    let value = value * 8 + self.current() - '0' as u32;
                    self.advance();
                    if value < 32 && is_octal(self.current()) {
                        self.advance();
                    }
                }
            }
        }

        /// V8 `ParseIntervalQuantifier`; the current char is `{`. Restores the position on
        /// failure.
        fn parse_interval_quantifier(&mut self) -> Option<(i64, i64)> {
            let start = self.pos;
            self.advance();
            if !is_decimal(self.current()) {
                self.pos = start;
                return None;
            }
            let min = self.parse_decimal_clamped();
            let max;
            if self.is('}') {
                max = min;
                self.advance();
            } else if self.is(',') {
                self.advance();
                if self.is('}') {
                    max = K_INFINITY;
                    self.advance();
                } else {
                    max = self.parse_decimal_clamped();
                    if !self.is('}') {
                        self.pos = start;
                        return None;
                    }
                    self.advance();
                }
            } else {
                self.pos = start;
                return None;
            }
            Some((min, max))
        }

        fn parse_decimal_clamped(&mut self) -> i64 {
            let mut v: i64 = 0;
            while is_decimal(self.current()) {
                let next = (self.current() - '0' as u32) as i64;
                if v > (K_INFINITY - next) / 10 {
                    while is_decimal(self.current()) {
                        self.advance();
                    }
                    return K_INFINITY;
                }
                v = v * 10 + next;
                self.advance();
            }
            v
        }

        /// `\p{…}` / `\P{…}` after the `\p`; validates syntax and defers the name lookup to
        /// regress (which has the Unicode tables).
        fn parse_property(&mut self, negated: bool, in_class: bool) -> R<bool> {
            let err = if in_class {
                INVALID_CLASS_PROPERTY_NAME
            } else {
                INVALID_PROPERTY_NAME
            };
            if !self.is('{') {
                return Err(err);
            }
            self.advance();
            let start = self.pos;
            loop {
                let c = self.current();
                if c == '}' as u32 {
                    break;
                }
                let ok = char::from_u32(c).is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '=');
                if !ok {
                    return Err(err);
                }
                self.advance();
            }
            let body: String = self.chars[start..self.pos]
                .iter()
                .filter_map(|c| char::from_u32(*c))
                .collect();
            self.advance();
            if STRING_PROPERTIES.contains(&body.as_str()) {
                if !self.unicode_sets || negated {
                    return Err(err);
                }
                return Ok(true);
            }
            if body.is_empty() || body.starts_with('=') || body.ends_with('=') || body.matches('=').count() > 1 {
                return Err(err);
            }
            let probe = format!("\\p{{{body}}}");
            let flags = regress::Flags {
                unicode: true,
                ..Default::default()
            };
            if regress::Regex::with_flags(&probe, flags).is_err() {
                return Err(err);
            }
            Ok(false)
        }

        /// V8 `ParseCharacterEscape`; the current char is `\`. Returns the code point.
        fn parse_character_escape(&mut self, in_class: bool) -> R<u32> {
            self.advance();
            let c = self.current();
            match char::from_u32(c).unwrap_or('\0') {
                'f' => {
                    self.advance();
                    Ok(0x0c)
                }
                'n' => {
                    self.advance();
                    Ok(0x0a)
                }
                'r' => {
                    self.advance();
                    Ok(0x0d)
                }
                't' => {
                    self.advance();
                    Ok(0x09)
                }
                'v' => {
                    self.advance();
                    Ok(0x0b)
                }
                'c' => {
                    let control = self.next();
                    let letter = control & !('A' as u32 ^ 'a' as u32);
                    if (b'A' as u32..=b'Z' as u32).contains(&letter) {
                        self.advance_by(2);
                        return Ok(control & 0x1f);
                    }
                    if self.unicode {
                        return Err(INVALID_UNICODE_ESCAPE);
                    }
                    if in_class && (is_decimal(control) || control == '_' as u32) {
                        self.advance_by(2);
                        return Ok(control & 0x1f);
                    }
                    // Treat as a backslash; `c` is parsed next as a literal.
                    Ok('\\' as u32)
                }
                '0' if !is_decimal(self.next()) => {
                    self.advance();
                    Ok(0)
                }
                '0'..='7' => {
                    if self.unicode {
                        return Err(INVALID_DECIMAL_ESCAPE);
                    }
                    let mut value = self.current() - '0' as u32;
                    self.advance();
                    if is_octal(self.current()) {
                        value = value * 8 + self.current() - '0' as u32;
                        self.advance();
                        if value < 32 && is_octal(self.current()) {
                            value = value * 8 + self.current() - '0' as u32;
                            self.advance();
                        }
                    }
                    Ok(value)
                }
                'x' => {
                    self.advance();
                    if let Some(v) = self.parse_hex_escape(2) {
                        return Ok(v);
                    }
                    if self.unicode {
                        return Err(INVALID_ESCAPE);
                    }
                    Ok('x' as u32)
                }
                'u' => {
                    self.advance();
                    if let Some(v) = self.parse_unicode_escape(self.unicode) {
                        return Ok(v);
                    }
                    if self.unicode {
                        return Err(INVALID_UNICODE_ESCAPE);
                    }
                    Ok('u' as u32)
                }
                _ => {
                    if !self.unicode {
                        if c == 'k' as u32 && self.has_named_captures() {
                            return Err(INVALID_ESCAPE);
                        }
                        self.advance();
                        return Ok(c);
                    }
                    if is_syntax_character_or_slash(c) {
                        self.advance();
                        return Ok(c);
                    }
                    if self.unicode_sets && in_class && is_class_set_reserved_punctuator(c) {
                        self.advance();
                        return Ok(c);
                    }
                    Err(INVALID_ESCAPE)
                }
            }
        }

        fn parse_hex_escape(&mut self, len: usize) -> Option<u32> {
            let start = self.pos;
            let mut v = 0u32;
            for _ in 0..len {
                let d = char::from_u32(self.current()).and_then(|c| c.to_digit(16));
                match d {
                    Some(d) => {
                        v = v * 16 + d;
                        self.advance();
                    }
                    None => {
                        self.pos = start;
                        return None;
                    }
                }
            }
            Some(v)
        }

        /// V8 `ParseUnicodeEscape`; `\u` already consumed.
        fn parse_unicode_escape(&mut self, unicode: bool) -> Option<u32> {
            if self.is('{') && unicode {
                let start = self.pos;
                self.advance();
                let mut v: u64 = 0;
                let mut any = false;
                while let Some(d) = char::from_u32(self.current()).and_then(|c| c.to_digit(16)) {
                    v = v * 16 + d as u64;
                    any = true;
                    if v > 0x10FFFF {
                        self.pos = start;
                        return None;
                    }
                    self.advance();
                }
                if any && self.is('}') {
                    self.advance();
                    return Some(v as u32);
                }
                self.pos = start;
                return None;
            }
            let v = self.parse_hex_escape(4)?;
            if unicode && (0xD800..0xDC00).contains(&v) && self.is('\\') {
                let start = self.pos;
                if self.next() == 'u' as u32 {
                    self.advance_by(2);
                    if let Some(trail) = self.parse_hex_escape(4)
                        && (0xDC00..0xE000).contains(&trail)
                    {
                        return Some(0x10000 + ((v - 0xD800) << 10) + (trail - 0xDC00));
                    }
                }
                self.pos = start;
            }
            Some(v)
        }

        fn parse_character_class(&mut self) -> R<()> {
            self.advance();
            let negated = self.is('^');
            if negated {
                self.advance();
            }
            if self.unicode_sets {
                let may_contain_strings = self.parse_class_set_expression()?;
                if negated && may_contain_strings {
                    return Err(NEGATED_CLASS_WITH_STRINGS);
                }
                return Ok(());
            }
            while self.has_more() && !self.is(']') {
                let first = self.parse_class_atom()?;
                if self.is('-') {
                    self.advance();
                    // V8 adds `first` and `-` as atoms before `]`; at the end the outer loop reports
                    // the unterminated class. Either way the range ends here.
                    if self.current() == END || self.is(']') {
                        break;
                    }
                    let second = self.parse_class_atom()?;
                    match (first, second) {
                        (Some(a), Some(b)) => {
                            if a > b {
                                return Err(OUT_OF_ORDER_CHARACTER_CLASS);
                            }
                        }
                        _ => {
                            if self.unicode {
                                return Err(INVALID_CHARACTER_CLASS);
                            }
                        }
                    }
                }
            }
            if !self.has_more() {
                return Err(UNTERMINATED_CHARACTER_CLASS);
            }
            self.advance();
            Ok(())
        }

        /// V8 `ParseClassEscape` / plain class char. `None` for a class escape (`\d`, …).
        fn parse_class_atom(&mut self) -> R<Option<u32>> {
            if !self.is('\\') {
                let c = self.current();
                self.advance();
                return Ok(Some(c));
            }
            let next = self.next();
            match char::from_u32(next).unwrap_or('\0') {
                _ if next == END => return Err(ESCAPE_AT_END),
                'b' => {
                    self.advance_by(2);
                    return Ok(Some(0x08));
                }
                '-' if self.unicode => {
                    self.advance_by(2);
                    return Ok(Some('-' as u32));
                }
                'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                    self.advance_by(2);
                    return Ok(None);
                }
                'p' | 'P' if self.unicode => {
                    self.advance_by(2);
                    self.parse_property(next == 'P' as u32, true)?;
                    return Ok(None);
                }
                _ => {}
            }
            self.parse_character_escape(true).map(Some)
        }

        /// `v`-mode class contents after `[` / `[^`; consumes the closing `]`. Returns whether
        /// the class may contain strings.
        fn parse_class_set_expression(&mut self) -> R<bool> {
            if self.is(']') {
                self.advance();
                return Ok(false);
            }
            let (first_strings, first_kind) = self.parse_class_set_operand()?;
            if self.is('-') && self.next() == '-' as u32 {
                return self.parse_class_subtraction(first_strings);
            }
            if self.is('&') && self.next() == '&' as u32 {
                return self.parse_class_intersection(first_strings);
            }
            self.parse_class_union(first_strings, first_kind)
        }

        fn parse_class_union(&mut self, mut strings: bool, mut last_kind: OperandKind) -> R<bool> {
            while self.has_more() && !self.is(']') {
                if self.is('-') {
                    if last_kind != OperandKind::Character {
                        return Err(INVALID_CLASS_SET_OPERATION);
                    }
                    self.advance();
                    let from = self.last_char;
                    let to = self.parse_class_set_character()?;
                    if from > to {
                        return Err(OUT_OF_ORDER_CHARACTER_CLASS);
                    }
                    last_kind = OperandKind::Range;
                } else {
                    if self.is('&') && self.next() == '&' as u32 {
                        return Err(INVALID_CLASS_SET_OPERATION);
                    }
                    let (s, kind) = self.parse_class_set_operand()?;
                    strings |= s;
                    last_kind = kind;
                    if self.is('-') && self.next() == '-' as u32 {
                        return Err(INVALID_CLASS_SET_OPERATION);
                    }
                }
            }
            if !self.has_more() {
                return Err(UNTERMINATED_CHARACTER_CLASS);
            }
            self.advance();
            Ok(strings)
        }

        fn parse_class_intersection(&mut self, mut strings: bool) -> R<bool> {
            while self.is('&') && self.next() == '&' as u32 {
                self.advance_by(2);
                if self.is('&') {
                    return Err(INVALID_CHARACTER_IN_CLASS);
                }
                let (s, _) = self.parse_class_set_operand()?;
                strings &= s;
            }
            if !self.has_more() {
                return Err(UNTERMINATED_CHARACTER_CLASS);
            }
            if !self.is(']') {
                return Err(INVALID_CLASS_SET_OPERATION);
            }
            self.advance();
            Ok(strings)
        }

        fn parse_class_subtraction(&mut self, strings: bool) -> R<bool> {
            while self.is('-') && self.next() == '-' as u32 {
                self.advance_by(2);
                self.parse_class_set_operand()?;
            }
            if !self.has_more() {
                return Err(UNTERMINATED_CHARACTER_CLASS);
            }
            if !self.is(']') {
                return Err(INVALID_CLASS_SET_OPERATION);
            }
            self.advance();
            Ok(strings)
        }

        /// Returns (may contain strings, kind).
        fn parse_class_set_operand(&mut self) -> R<(bool, OperandKind)> {
            if self.is('\\') {
                let next = self.next();
                match char::from_u32(next).unwrap_or('\0') {
                    _ if next == END => return Err(ESCAPE_AT_END),
                    'q' => {
                        self.advance_by(2);
                        if !self.is('{') {
                            return Err(INVALID_ESCAPE);
                        }
                        self.advance();
                        let mut strings = false;
                        let mut len = 0usize;
                        loop {
                            if !self.has_more() {
                                return Err(UNTERMINATED_CHARACTER_CLASS);
                            }
                            if self.is('}') {
                                self.advance();
                                if len != 1 {
                                    strings = true;
                                }
                                break;
                            }
                            if self.is('|') {
                                self.advance();
                                if len != 1 {
                                    strings = true;
                                }
                                len = 0;
                                continue;
                            }
                            if self.is('\\') {
                                self.parse_character_escape(true)?;
                            } else {
                                self.advance();
                            }
                            len += 1;
                        }
                        return Ok((strings, OperandKind::Class));
                    }
                    'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                        self.advance_by(2);
                        return Ok((false, OperandKind::Class));
                    }
                    'p' | 'P' => {
                        self.advance_by(2);
                        let strings = self.parse_property(next == 'P' as u32, true)?;
                        return Ok((strings, OperandKind::Class));
                    }
                    _ => {}
                }
                let c = self.parse_class_set_character()?;
                self.last_char = c;
                return Ok((false, OperandKind::Character));
            }
            if self.is('[') {
                self.advance();
                let negated = self.is('^');
                if negated {
                    self.advance();
                }
                let strings = self.parse_class_set_expression()?;
                if negated && strings {
                    return Err(NEGATED_CLASS_WITH_STRINGS);
                }
                return Ok((strings, OperandKind::Class));
            }
            let c = self.parse_class_set_character()?;
            self.last_char = c;
            Ok((false, OperandKind::Character))
        }

        fn parse_class_set_character(&mut self) -> R<u32> {
            let c = self.current();
            if c == '\\' as u32 {
                if self.next() == END {
                    return Err(ESCAPE_AT_END);
                }
                if self.next() == 'b' as u32 {
                    self.advance_by(2);
                    return Ok(0x08);
                }
                return self.parse_character_escape(true);
            }
            if c == END {
                return Err(UNTERMINATED_CHARACTER_CLASS);
            }
            if is_class_set_syntax_character(c) {
                return Err(INVALID_CHARACTER_IN_CLASS);
            }
            if is_class_set_reserved_double_punctuator(c) && self.next() == c {
                return Err(INVALID_CLASS_SET_OPERATION);
            }
            self.advance();
            Ok(c)
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum OperandKind {
        Character,
        Range,
        Class,
    }

    fn is_decimal(c: u32) -> bool {
        (b'0' as u32..=b'9' as u32).contains(&c)
    }

    fn is_octal(c: u32) -> bool {
        (b'0' as u32..=b'7' as u32).contains(&c)
    }

    fn is_syntax_character_or_slash(c: u32) -> bool {
        char::from_u32(c).is_some_and(|c| "^$\\.*+?()[]{}|/".contains(c))
    }

    fn is_class_set_reserved_punctuator(c: u32) -> bool {
        char::from_u32(c).is_some_and(|c| "&-!#%,:;<=>@`~".contains(c))
    }

    fn is_class_set_syntax_character(c: u32) -> bool {
        char::from_u32(c).is_some_and(|c| "()[]{}/-\\|".contains(c))
    }

    fn is_class_set_reserved_double_punctuator(c: u32) -> bool {
        char::from_u32(c).is_some_and(|c| "&!#$%*+,.:;<=>?@^`~".contains(c))
    }

    fn is_identifier_start(c: u32) -> bool {
        match char::from_u32(c) {
            Some(ch) if ch.is_ascii() => ch.is_ascii_alphabetic() || ch == '$' || ch == '_',
            // PORT: non-ASCII ID_Start is approximated with Unicode Alphabetic (V8 uses ICU's
            // ID_Start); regress re-validates the name when compiling.
            Some(ch) => ch.is_alphabetic(),
            None => false,
        }
    }

    fn is_identifier_part(c: u32) -> bool {
        match char::from_u32(c) {
            Some(ch) if ch.is_ascii() => ch.is_ascii_alphanumeric() || ch == '$' || ch == '_',
            Some('\u{200c}' | '\u{200d}') => true,
            // PORT: approximation of ID_Continue (see `is_identifier_start`); combining marks are
            // accepted so that valid names are never rejected here.
            Some(ch) => ch.is_alphanumeric() || !ch.is_whitespace() && !ch.is_control() && !ch.is_ascii_punctuation(),
            None => false,
        }
    }

    fn push_cp(s: &mut String, c: u32) {
        s.push(char::from_u32(c).unwrap_or('\u{fffd}'));
    }
}

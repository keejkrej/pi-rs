//! Pure-Rust TypeScript-to-JavaScript erasable syntax stripper & ESM/CJS preprocessor
//! powering the QuickJS extension host (replaces Node `jiti`).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokKind {
    Ident,
    String,
    TemplateHead,
    TemplateMid,
    TemplateTail,
    TemplateFull,
    Regex,
    Number,
    LineComment,
    BlockComment,
    Whitespace,
    Newline,
    Punct,
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokKind,
    start: usize,
    end: usize,
}

impl Token {
    fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }

    fn is_trivia(&self) -> bool {
        matches!(
            self.kind,
            TokKind::Whitespace | TokKind::Newline | TokKind::LineComment | TokKind::BlockComment
        )
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80
}

fn is_ident_continue(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit()
}

/// Tokenize TS/JS source faithfully including regex literals and nested template literals.
fn lex(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut tokens: Vec<Token> = Vec::new();
    // Track brace depths of active template literal `${...}` expressions so closing `}` resumes template lexing.
    let mut template_brace_stack: Vec<usize> = Vec::new();
    let mut brace_depth = 0usize;

    let last_sig_kind_allows_regex = |tokens: &[Token], src: &str| -> bool {
        let Some(last) = tokens.iter().rev().find(|t| !t.is_trivia()) else {
            return true;
        };
        match last.kind {
            TokKind::Punct => !matches!(last.text(src), ")" | "]" | "++" | "--"),
            TokKind::Ident => matches!(
                last.text(src),
                "return"
                    | "throw"
                    | "case"
                    | "default"
                    | "delete"
                    | "void"
                    | "typeof"
                    | "await"
                    | "yield"
                    | "in"
                    | "instanceof"
                    | "of"
                    | "new"
                    | "else"
                    | "do"
                    | "export"
            ),
            TokKind::Regex | TokKind::Number | TokKind::String | TokKind::TemplateTail | TokKind::TemplateFull => false,
            TokKind::TemplateHead | TokKind::TemplateMid => true,
            TokKind::LineComment | TokKind::BlockComment | TokKind::Whitespace | TokKind::Newline => true,
        }
    };

    while i < len {
        let b = bytes[i];
        if b == b'\n' {
            tokens.push(Token {
                kind: TokKind::Newline,
                start: i,
                end: i + 1,
            });
            i += 1;
            continue;
        }
        if b == b'\r' {
            let end = if i + 1 < len && bytes[i + 1] == b'\n' {
                i + 2
            } else {
                i + 1
            };
            tokens.push(Token {
                kind: TokKind::Newline,
                start: i,
                end,
            });
            i = end;
            continue;
        }
        if b == b' ' || b == b'\t' || b == 0x0b || b == 0x0c {
            let start = i;
            i += 1;
            while i < len && matches!(bytes[i], b' ' | b'\t' | 0x0b | 0x0c) {
                i += 1;
            }
            tokens.push(Token {
                kind: TokKind::Whitespace,
                start,
                end: i,
            });
            continue;
        }
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            let start = i;
            i += 2;
            while i < len && bytes[i] != b'\n' && bytes[i] != b'\r' {
                i += 1;
            }
            tokens.push(Token {
                kind: TokKind::LineComment,
                start,
                end: i,
            });
            continue;
        }
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < len {
                i += 2;
            } else {
                i = len;
            }
            tokens.push(Token {
                kind: TokKind::BlockComment,
                start,
                end: i,
            });
            continue;
        }
        if b == b'\'' || b == b'"' {
            let start = i;
            let quote = b;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                if bytes[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            tokens.push(Token {
                kind: TokKind::String,
                start,
                end: i,
            });
            continue;
        }
        if b == b'`' {
            let start = i;
            i += 1;
            let mut found_interp = false;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                    i += 2;
                    found_interp = true;
                    brace_depth += 1;
                    template_brace_stack.push(brace_depth);
                    break;
                }
                if bytes[i] == b'`' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            tokens.push(Token {
                kind: if found_interp {
                    TokKind::TemplateHead
                } else {
                    TokKind::TemplateFull
                },
                start,
                end: i,
            });
            continue;
        }
        if b == b'}' && template_brace_stack.last() == Some(&brace_depth) {
            template_brace_stack.pop();
            brace_depth = brace_depth.saturating_sub(1);
            let start = i;
            i += 1;
            let mut found_interp = false;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                    i += 2;
                    found_interp = true;
                    brace_depth += 1;
                    template_brace_stack.push(brace_depth);
                    break;
                }
                if bytes[i] == b'`' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            tokens.push(Token {
                kind: if found_interp {
                    TokKind::TemplateMid
                } else {
                    TokKind::TemplateTail
                },
                start,
                end: i,
            });
            continue;
        }
        if b == b'/' && last_sig_kind_allows_regex(&tokens, src) {
            let start = i;
            i += 1;
            let mut in_class = false;
            let mut valid = false;
            while i < len {
                let c = bytes[i];
                if c == b'\n' || c == b'\r' {
                    break;
                }
                if c == b'\\' && i + 1 < len {
                    i += 2;
                    continue;
                }
                if c == b'[' {
                    in_class = true;
                } else if c == b']' && in_class {
                    in_class = false;
                } else if c == b'/' && !in_class {
                    i += 1;
                    while i < len && bytes[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    valid = true;
                    break;
                }
                i += 1;
            }
            if valid {
                tokens.push(Token {
                    kind: TokKind::Regex,
                    start,
                    end: i,
                });
                continue;
            } else {
                i = start;
            }
        }
        if b.is_ascii_digit() || (b == b'.' && i + 1 < len && bytes[i + 1].is_ascii_digit()) {
            let start = i;
            i += 1;
            while i < len
                && (bytes[i].is_ascii_alphanumeric()
                    || bytes[i] == b'_'
                    || bytes[i] == b'.'
                    || ((bytes[i] == b'+' || bytes[i] == b'-') && (bytes[i - 1] == b'e' || bytes[i - 1] == b'E')))
            {
                i += 1;
            }
            tokens.push(Token {
                kind: TokKind::Number,
                start,
                end: i,
            });
            continue;
        }
        if is_ident_start(b) {
            let start = i;
            // Advance along UTF-8 char boundaries
            let ch_len = src[i..].chars().next().map_or(1, |c| c.len_utf8());
            i += ch_len;
            while i < len && is_ident_continue(bytes[i]) {
                let ch_len = src[i..].chars().next().map_or(1, |c| c.len_utf8());
                i += ch_len;
            }
            tokens.push(Token {
                kind: TokKind::Ident,
                start,
                end: i,
            });
            continue;
        }

        // Punctuation (multi-char first)
        if b == b'{' {
            brace_depth += 1;
        } else if b == b'}' {
            brace_depth = brace_depth.saturating_sub(1);
        }
        let start = i;
        let rest = &src[i..];
        let p_len =
            if rest.starts_with("===") || rest.starts_with("!==") || rest.starts_with("...") || rest.starts_with(">>>")
            {
                3
            } else if rest.starts_with("=>")
                || rest.starts_with("==")
                || rest.starts_with("!=")
                || rest.starts_with("<=")
                || rest.starts_with(">=")
                || rest.starts_with("&&")
                || rest.starts_with("||")
                || rest.starts_with("??")
                || rest.starts_with("?.")
                || rest.starts_with("++")
                || rest.starts_with("--")
                || rest.starts_with("**")
                || rest.starts_with("<<")
                || rest.starts_with(">>")
                || rest.starts_with("+=")
                || rest.starts_with("-=")
                || rest.starts_with("*=")
                || rest.starts_with("/=")
            {
                2
            } else {
                src[i..].chars().next().map_or(1, |c| c.len_utf8())
            };
        i += p_len;
        tokens.push(Token {
            kind: TokKind::Punct,
            start,
            end: i,
        });
    }

    tokens
}

fn next_sig(tokens: &[Token], mut idx: usize) -> Option<usize> {
    while idx < tokens.len() {
        if !tokens[idx].is_trivia() {
            return Some(idx);
        }
        idx += 1;
    }
    None
}

fn prev_sig(tokens: &[Token], mut idx: usize) -> Option<usize> {
    while idx > 0 {
        idx -= 1;
        if !tokens[idx].is_trivia() {
            return Some(idx);
        }
    }
    None
}

fn has_newline_between(tokens: &[Token], from: usize, to: usize) -> bool {
    tokens[from..to].iter().any(|t| t.kind == TokKind::Newline)
}

fn emit_erased(src: &str, tokens: &[Token], from: usize, to: usize, out: &mut String) {
    // Preserve newlines so reported error line numbers match the original TS file.
    for t in &tokens[from..to] {
        if t.kind == TokKind::Newline {
            out.push('\n');
        } else if matches!(t.kind, TokKind::LineComment | TokKind::BlockComment) {
            for ch in t.text(src).chars() {
                if ch == '\n' {
                    out.push('\n');
                }
            }
        }
    }
}

/// Strip erasable TypeScript syntax into clean JavaScript while preserving line numbers.
pub fn transpile_ts_to_js(source: &str, filename: &str) -> String {
    let lower = filename.to_ascii_lowercase();
    let is_ts = lower.ends_with(".ts") || lower.ends_with(".mts") || lower.ends_with(".cts");
    if !is_ts {
        return source.to_string();
    }

    let tokens = lex(source);
    let mut out = String::with_capacity(source.len());
    strip_range(source, &tokens, 0, tokens.len(), false, &mut out);
    out
}

fn find_matching_delim(src: &str, tokens: &[Token], open_idx: usize, end_limit: usize) -> Option<usize> {
    let open = tokens[open_idx].text(src);
    let close = match open {
        "(" => ")",
        "[" => "]",
        "{" => "}",
        "<" => ">",
        _ => return None,
    };
    let mut depth = 0usize;
    let mut i = open_idx;
    while i < end_limit {
        let txt = tokens[i].text(src);
        if !tokens[i].is_trivia() {
            if txt == open {
                depth += 1;
            } else if txt == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            } else if txt == "=>" && open == "<" {
                // Allow `=>` inside type arguments like `<(a: string) => void>`.
            }
        }
        i += 1;
    }
    None
}

/// Find end index (exclusive) of a TS type expression.
fn find_type_end(src: &str, tokens: &[Token], start: usize, end_limit: usize, stop_at_parens: bool) -> usize {
    let mut i = start;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut brace = 0usize;
    let mut angle = 0usize;
    while i < end_limit {
        if tokens[i].is_trivia() {
            if tokens[i].kind == TokKind::Newline && paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
                // Check if next significant token continues the type (e.g., `|`, `&`, `=>`)
                if let Some(ns) = next_sig(tokens, i + 1) {
                    let ntxt = tokens[ns].text(src);
                    if !matches!(ntxt, "|" | "&" | "=>" | "?" | "extends" | "[") {
                        // Also check if previous sig token expects continuation
                        let prev_continues = prev_sig(tokens, i)
                            .is_some_and(|ps| matches!(tokens[ps].text(src), "|" | "&" | "=" | ":" | "<" | ","));
                        if !prev_continues {
                            return i;
                        }
                    }
                } else {
                    return i;
                }
            }
            i += 1;
            continue;
        }
        let txt = tokens[i].text(src);
        match txt {
            "(" => paren += 1,
            ")" => {
                if paren == 0 {
                    if stop_at_parens || (bracket == 0 && brace == 0 && angle == 0) {
                        return i;
                    }
                } else {
                    paren -= 1;
                }
            }
            "[" => bracket += 1,
            "]" => {
                if bracket == 0 && paren == 0 && brace == 0 && angle == 0 {
                    return i;
                }
                bracket = bracket.saturating_sub(1);
            }
            "{" => {
                // Notice: if `{` starts a function/method body after return type!
                // Heuristic: if brace == 0 && paren == 0 && bracket == 0 && angle == 0,
                // wait: `{` COULD be an object type `{ a: string }` OR a function body `{ return ... }`!
                // Callers that stop before function bodies pass a dedicated stop check; here track braces inside types unless caller checks.
                brace += 1;
            }
            "}" => {
                if brace == 0 && paren == 0 && bracket == 0 && angle == 0 {
                    return i;
                }
                brace = brace.saturating_sub(1);
            }
            "<" => angle += 1,
            ">" => angle = angle.saturating_sub(1),
            ";" => {
                if paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
                    return i;
                }
            }
            "," => {
                if paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
                    return i;
                }
            }
            "=" => {
                // `=` ends default param/var type annotation (whereas `=>` inside type is function type!)
                if paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    end_limit
}

/// Find end of return type annotation right before `{` (function body) or `=>` (arrow body) or `;`.
fn find_return_type_end(src: &str, tokens: &[Token], start: usize, end_limit: usize) -> usize {
    let mut i = start;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut brace = 0usize;
    let mut angle = 0usize;
    while i < end_limit {
        if tokens[i].is_trivia() {
            if tokens[i].kind == TokKind::Newline && paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
                if let Some(ns) = next_sig(tokens, i + 1) {
                    let ntxt = tokens[ns].text(src);
                    if matches!(ntxt, "{" | "=>" | ";" | "}") {
                        return ns;
                    }
                    if !matches!(ntxt, "|" | "&") {
                        let prev_continues =
                            prev_sig(tokens, i).is_some_and(|ps| matches!(tokens[ps].text(src), "|" | "&" | "is"));
                        if !prev_continues {
                            return i;
                        }
                    }
                }
            }
            i += 1;
            continue;
        }
        let txt = tokens[i].text(src);
        if paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
            if txt == "=>" || txt == ";" || txt == "," || txt == ")" {
                return i;
            }
            if txt == "{" {
                // Is `{` an inline object return type `{ ok: boolean; err?: string } => ...` or the function body?
                // A function body `{` either has `)` right before `{` (no type), OR if we already saw type tokens and the block starts statements/returns, vs `{ ... }` followed by `{` or `=>` or `|`/`&`!
                if let Some(close_b) = find_matching_delim(src, tokens, i, end_limit) {
                    if let Some(after) = next_sig(tokens, close_b + 1) {
                        let after_txt = tokens[after].text(src);
                        if matches!(after_txt, "{" | "=>" | "|" | "&" | "[") {
                            i = close_b + 1;
                            continue;
                        }
                    }
                }
                return i;
            }
        }
        match txt {
            "(" => paren += 1,
            ")" => paren = paren.saturating_sub(1),
            "[" => bracket += 1,
            "]" => bracket = bracket.saturating_sub(1),
            "{" => brace += 1,
            "}" => brace = brace.saturating_sub(1),
            "<" => angle += 1,
            ">" => angle = angle.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    end_limit
}

fn find_expr_type_end(src: &str, tokens: &[Token], start: usize, end_limit: usize) -> usize {
    // Used after `as` / `satisfies`: reads a TS type stopping before binary/member/delimiter tokens.
    let mut i = start;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut brace = 0usize;
    let mut angle = 0usize;
    let mut expect_operand = true;
    while i < end_limit {
        if tokens[i].is_trivia() {
            if tokens[i].kind == TokKind::Newline
                && paren == 0
                && bracket == 0
                && brace == 0
                && angle == 0
                && !expect_operand
            {
                if let Some(ns) = next_sig(tokens, i + 1) {
                    if !matches!(tokens[ns].text(src), "|" | "&" | "[") {
                        return i;
                    }
                } else {
                    return i;
                }
            }
            i += 1;
            continue;
        }
        let txt = tokens[i].text(src);
        if paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
            if !expect_operand {
                match txt {
                    "|" | "&" | "." | "extends" => {
                        expect_operand = true;
                        i += 1;
                        continue;
                    }
                    "[" => {
                        bracket += 1;
                        i += 1;
                        continue;
                    }
                    "<" => {
                        angle += 1;
                        expect_operand = true;
                        i += 1;
                        continue;
                    }
                    "=>" => {
                        expect_operand = true;
                        i += 1;
                        continue;
                    }
                    // Anything else ends `as Type`! Wait: `as unknown as Foo` chains `as`!
                    "as" | "satisfies" => {
                        expect_operand = true;
                        i += 1;
                        continue;
                    }
                    _ => return i,
                }
            } else {
                match txt {
                    "typeof" | "keyof" | "readonly" | "infer" => {
                        i += 1;
                        continue;
                    }
                    "const" => {
                        i += 1;
                        expect_operand = false;
                        continue;
                    }
                    "(" => paren += 1,
                    "[" => bracket += 1,
                    "{" => brace += 1,
                    "<" => angle += 1,
                    _ => {
                        expect_operand = false;
                        i += 1;
                        continue;
                    }
                }
            }
        } else {
            match txt {
                "(" => paren += 1,
                ")" => paren = paren.saturating_sub(1),
                "[" => bracket += 1,
                "]" => {
                    bracket = bracket.saturating_sub(1);
                    if paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
                        expect_operand = false;
                    }
                }
                "{" => brace += 1,
                "}" => {
                    brace = brace.saturating_sub(1);
                    if paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
                        expect_operand = false;
                    }
                }
                "<" => angle += 1,
                ">" => {
                    angle = angle.saturating_sub(1);
                    if paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
                        expect_operand = false;
                    }
                }
                _ => {}
            }
            if txt == ")" && paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
                // Check if `(a: string) => void`
                if let Some(ns) = next_sig(tokens, i + 1) {
                    if tokens[ns].text(src) == "=>" {
                        expect_operand = true;
                        i = ns + 1;
                        continue;
                    }
                }
                expect_operand = false;
            }
        }
        i += 1;
    }
    end_limit
}

fn find_stmt_end(src: &str, tokens: &[Token], start: usize, end_limit: usize) -> usize {
    let mut i = start;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut brace = 0usize;
    let mut angle = 0usize;
    while i < end_limit {
        if tokens[i].kind == TokKind::Newline && paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
            if let Some(ns) = next_sig(tokens, i + 1) {
                let ntxt = tokens[ns].text(src);
                if !matches!(ntxt, "|" | "&" | "=" | "<" | "{") {
                    let prev_eq = prev_sig(tokens, i).is_some_and(|ps| matches!(tokens[ps].text(src), "=" | "|" | "&"));
                    if !prev_eq {
                        return i;
                    }
                }
            } else {
                return i;
            }
        }
        if !tokens[i].is_trivia() {
            match tokens[i].text(src) {
                "(" => paren += 1,
                ")" => paren = paren.saturating_sub(1),
                "[" => bracket += 1,
                "]" => bracket = bracket.saturating_sub(1),
                "{" => brace += 1,
                "}" => brace = brace.saturating_sub(1),
                "<" => angle += 1,
                ">" => angle = angle.saturating_sub(1),
                ";" if paren == 0 && bracket == 0 && brace == 0 && angle == 0 => return i + 1,
                _ => {}
            }
        }
        i += 1;
    }
    end_limit
}

fn looks_like_generic_angle_before_paren(
    src: &str,
    tokens: &[Token],
    lt_idx: usize,
    end_limit: usize,
) -> Option<(usize, usize)> {
    // Checks `<T, U>` followed immediately by `(` (or `=>` in `<T>(x: T) => ...`).
    let mut depth = 0usize;
    let mut i = lt_idx;
    while i < end_limit {
        let t = &tokens[i];
        if !t.is_trivia() {
            let txt = t.text(src);
            if txt == "<" {
                depth += 1;
            } else if txt == ">" {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(after) = next_sig(tokens, i + 1) {
                        if tokens[after].text(src) == "(" {
                            return Some((i, after));
                        }
                    }
                    return None;
                }
            } else if txt == ">>" && depth >= 2 {
                depth -= 2;
                if depth == 0 {
                    if let Some(after) = next_sig(tokens, i + 1) {
                        if tokens[after].text(src) == "(" {
                            return Some((i, after));
                        }
                    }
                    return None;
                }
            } else if matches!(
                txt,
                ";" | "&&" | "||" | "==" | "===" | "!=" | "!==" | "+" | "-" | "*" | "/"
            ) {
                return None;
            } else if t.kind == TokKind::Ident
                && matches!(
                    txt,
                    "return" | "if" | "while" | "for" | "switch" | "throw" | "function" | "class"
                )
            {
                return None;
            }
        }
        i += 1;
    }
    None
}

fn is_arrow_param_paren(src: &str, tokens: &[Token], open_idx: usize, close_idx: usize, end_limit: usize) -> bool {
    let Some(ns) = next_sig(tokens, close_idx + 1) else {
        return false;
    };
    if tokens[ns].text(src) == "=>" {
        // Ensure `(` wasn't `if (...)` / ternary etc. (`=>` after `)` is always arrow!).
        if let Some(prev) = prev_sig(tokens, open_idx) {
            if matches!(tokens[prev].text(src), "if" | "while" | "for" | "switch" | "catch") {
                return false;
            }
        }
        return true;
    }
    if tokens[ns].text(src) == ":" {
        let ret_end = find_return_type_end(src, tokens, ns + 1, end_limit);
        if let Some(after_ret) = next_sig(tokens, ret_end) {
            return tokens[after_ret].text(src) == "=>";
        }
    }
    false
}

/// Transform `enum Foo { A = 1, B, C = "str" }` into `const Foo = { A: 1, B: 2, C: "str" };`.
fn transform_enum(
    src: &str,
    tokens: &[Token],
    enum_kw: usize,
    end_limit: usize,
    exported: bool,
) -> Option<(String, usize)> {
    let name_idx = next_sig(tokens, enum_kw + 1)?;
    let enum_name = tokens[name_idx].text(src);
    let open_brace = next_sig(tokens, name_idx + 1)?;
    if tokens[open_brace].text(src) != "{" {
        return None;
    }
    let close_brace = find_matching_delim(src, tokens, open_brace, end_limit)?;
    let body = src[tokens[open_brace].end..tokens[close_brace].start].trim();
    let mut entries = Vec::new();
    let mut next_num: i64 = 0;
    for part in body.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim();
            let val = v.trim();
            if let Ok(parsed) = val.parse::<i64>() {
                next_num = parsed + 1;
            }
            entries.push(format!("{key}: {val}"));
        } else {
            entries.push(format!("{trimmed}: {next_num}"));
            next_num += 1;
        }
    }
    let prefix = if exported { "export const" } else { "const" };
    let mut emitted = format!("{prefix} {enum_name} = {{ {} }};", entries.join(", "));
    // Preserve newline count
    let orig_lines = src[tokens[enum_kw].start..tokens[close_brace].end]
        .matches('\n')
        .count();
    for _ in 0..orig_lines {
        emitted.push('\n');
    }
    Some((emitted, close_brace + 1))
}

fn strip_params_list(
    src: &str,
    tokens: &[Token],
    open_paren: usize,
    close_paren: usize,
    promote_fields: &mut Vec<String>,
) -> String {
    let mut out = String::new();
    out.push('(');
    let mut i = open_paren + 1;
    let mut depth = 0usize;
    let mut at_param_start = true;
    let mut current_param_ident: Option<String> = None;
    let mut current_param_promoted = false;

    while i < close_paren {
        let t = &tokens[i];
        if t.is_trivia() {
            out.push_str(t.text(src));
            i += 1;
            continue;
        }
        let txt = t.text(src);
        if depth == 0 {
            if at_param_start
                && t.kind == TokKind::Ident
                && matches!(txt, "public" | "private" | "protected" | "readonly")
            {
                current_param_promoted = true;
                i += 1;
                continue;
            }
            if at_param_start && txt == "..." {
                out.push_str("...");
                i += 1;
                continue;
            }
            if at_param_start && t.kind == TokKind::Ident {
                current_param_ident = Some(txt.to_string());
                at_param_start = false;
                out.push_str(txt);
                i += 1;
                continue;
            }
            if txt == "?" && !at_param_start {
                // Optional parameter `name?: Type` -> strip `?`
                i += 1;
                continue;
            }
            if txt == ":" {
                // Skip parameter type annotation up to `,` or `=` at depth 0
                let type_end = find_type_end(src, tokens, i + 1, close_paren, true);
                emit_erased(src, tokens, i, type_end, &mut out);
                i = type_end;
                continue;
            }
            if txt == "," {
                if current_param_promoted {
                    if let Some(ident) = current_param_ident.take() {
                        promote_fields.push(ident);
                    }
                    current_param_promoted = false;
                }
                current_param_ident = None;
                at_param_start = true;
                out.push(',');
                i += 1;
                continue;
            }
            if txt == "=" {
                if current_param_promoted {
                    if let Some(ident) = current_param_ident.take() {
                        promote_fields.push(ident);
                    }
                    current_param_promoted = false;
                }
                at_param_start = false;
                out.push('=');
                i += 1;
                continue;
            }
        }
        match txt {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            _ => {}
        }
        out.push_str(txt);
        at_param_start = false;
        i += 1;
    }
    if current_param_promoted {
        if let Some(ident) = current_param_ident.take() {
            promote_fields.push(ident);
        }
    }
    out.push(')');
    out
}

fn process_import_stmt(src: &str, tokens: &[Token], import_kw: usize, end_limit: usize, out: &mut String) -> usize {
    // Check `import type ...`
    let Some(first_after) = next_sig(tokens, import_kw + 1) else {
        out.push_str("import");
        return import_kw + 1;
    };
    let after_txt = tokens[first_after].text(src);
    if after_txt == "(" || after_txt == "." {
        // Dynamic `import(...)` or `import.meta`
        out.push_str("import");
        return import_kw + 1;
    }
    // Side-effect import `import "..."`
    if tokens[first_after].kind == TokKind::String {
        return import_kw + 1; // Let caller emit tokens naturally up to `;`
    }

    let stmt_end = find_stmt_end(src, tokens, import_kw + 1, end_limit);
    if after_txt == "type" {
        // Check it's actually `import type X` and not `import type from "x"`
        if let Some(second) = next_sig(tokens, first_after + 1) {
            if tokens[second].text(src) != "from" {
                emit_erased(src, tokens, import_kw, stmt_end, out);
                return stmt_end;
            }
        }
    }

    // Look for `{ ... }` specifiers inside this import statement to strip inline `type Foo`.
    let mut brace_idx = None;
    let mut k = first_after;
    while k < stmt_end {
        if tokens[k].text(src) == "{" {
            brace_idx = Some(k);
            break;
        }
        k += 1;
    }
    let Some(open_b) = brace_idx else {
        // No named `{ ... }` block (e.g. `import * as ns from "..."` or `import def from "..."`).
        out.push_str(&src[tokens[import_kw].start..tokens[stmt_end - 1].end]);
        return stmt_end;
    };
    let Some(close_b) = find_matching_delim(src, tokens, open_b, stmt_end) else {
        out.push_str(&src[tokens[import_kw].start..tokens[stmt_end - 1].end]);
        return stmt_end;
    };

    // Split `{ ... }` by `,` cleanly
    let inner = &src[tokens[open_b].end..tokens[close_b].start];
    let mut kept_specs = Vec::new();
    for part in inner.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("type ") || trimmed.starts_with("type\t") || trimmed.starts_with("type\n") {
            continue;
        }
        kept_specs.push(trimmed);
    }

    let before_brace = src[tokens[first_after].start..tokens[open_b].start].trim();
    let has_default = !before_brace.is_empty();
    let from_part = src[tokens[close_b].end..tokens[stmt_end - 1].end].trim_start();

    let total_orig_lines = src[tokens[import_kw].start..tokens[stmt_end - 1].end]
        .matches('\n')
        .count();

    if kept_specs.is_empty() && !has_default {
        // Entire import was type-only!
        for _ in 0..total_orig_lines {
            out.push('\n');
        }
        return stmt_end;
    }

    if kept_specs.is_empty() && has_default {
        let def_name = before_brace.trim_end_matches(',').trim();
        out.push_str(&format!("import {def_name} {from_part}"));
    } else if has_default {
        let def_name = before_brace.trim_end_matches(',').trim();
        let def_clean = if def_name.ends_with(',') {
            def_name.to_string()
        } else {
            format!("{def_name},")
        };
        out.push_str(&format!(
            "import {def_clean} {{ {} }} {from_part}",
            kept_specs.join(", ")
        ));
    } else {
        out.push_str(&format!("import {{ {} }} {from_part}", kept_specs.join(", ")));
    }
    let emitted_lines = out[out.len().saturating_sub(from_part.len() + 64).min(out.len())..]
        .matches('\n')
        .count();
    let _ = emitted_lines;
    for _ in 0..total_orig_lines {
        // Wait: from_part wasn't multiline usually, just adjust if needed: actually simpler: count diff!
    }
    // Adjust exact newlines so line numbers never drift:
    let mut rebuilt_newlines = 0usize;
    if !kept_specs.is_empty() || has_default {
        rebuilt_newlines = from_part.matches('\n').count();
    }
    if total_orig_lines > rebuilt_newlines {
        for _ in 0..(total_orig_lines - rebuilt_newlines) {
            out.push('\n');
        }
    }
    stmt_end
}

fn process_class_body(src: &str, tokens: &[Token], open_b: usize, close_b: usize, out: &mut String) {
    out.push('{');
    let mut i = open_b + 1;
    let mut at_member_start = true;
    while i < close_b {
        let t = &tokens[i];
        if t.is_trivia() {
            out.push_str(t.text(src));
            i += 1;
            continue;
        }
        let txt = t.text(src);
        if at_member_start && t.kind == TokKind::Ident {
            if matches!(
                txt,
                "public" | "private" | "protected" | "readonly" | "override" | "declare"
            ) {
                if txt == "declare" {
                    let end = find_stmt_end(src, tokens, i + 1, close_b);
                    emit_erased(src, tokens, i, end, out);
                    i = end;
                    at_member_start = true;
                    continue;
                }
                i += 1;
                continue;
            }
            if txt == "abstract" {
                // Skip abstract method/field declaration
                let end = find_stmt_end(src, tokens, i + 1, close_b);
                emit_erased(src, tokens, i, end, out);
                i = end;
                at_member_start = true;
                continue;
            }
            if txt == "constructor" {
                out.push_str("constructor");
                if let Some(op) = next_sig(tokens, i + 1) {
                    if tokens[op].text(src) == "(" {
                        if let Some(cp) = find_matching_delim(src, tokens, op, close_b) {
                            let mut promoted = Vec::new();
                            let params_js = strip_params_list(src, tokens, op, cp, &mut promoted);
                            out.push_str(&params_js);
                            if let Some(body_open) = next_sig(tokens, cp + 1) {
                                if tokens[body_open].text(src) == "{" {
                                    if let Some(body_close) = find_matching_delim(src, tokens, body_open, close_b) {
                                        out.push_str(" { ");
                                        for field in &promoted {
                                            out.push_str(&format!("this.{field} = {field}; "));
                                        }
                                        strip_range(src, tokens, body_open + 1, body_close, false, out);
                                        out.push('}');
                                        i = body_close + 1;
                                        at_member_start = true;
                                        continue;
                                    }
                                }
                            }
                            i = cp + 1;
                            continue;
                        }
                    }
                }
            }
        }
        // Member method `foo<T>(a: A): Ret { ... }` or field `bar?: string = "x";` or `bar?: string;`
        if !at_member_start && txt == "<" {
            if let Some((_end_gt, open_p)) = looks_like_generic_angle_before_paren(src, tokens, i, close_b) {
                emit_erased(src, tokens, i, open_p, out);
                i = open_p;
                continue;
            }
        }
        if !at_member_start && txt == "(" {
            if let Some(cp) = find_matching_delim(src, tokens, i, close_b) {
                let mut unused_promoted = Vec::new();
                out.push_str(&strip_params_list(src, tokens, i, cp, &mut unused_promoted));
                if let Some(colon_idx) = next_sig(tokens, cp + 1) {
                    if tokens[colon_idx].text(src) == ":" {
                        let ret_end = find_return_type_end(src, tokens, colon_idx + 1, close_b);
                        emit_erased(src, tokens, colon_idx, ret_end, out);
                        i = ret_end;
                        if let Some(b_open) = next_sig(tokens, i) {
                            if tokens[b_open].text(src) == "{" {
                                if let Some(b_close) = find_matching_delim(src, tokens, b_open, close_b) {
                                    out.push('{');
                                    strip_range(src, tokens, b_open + 1, b_close, false, out);
                                    out.push('}');
                                    i = b_close + 1;
                                    at_member_start = true;
                                    continue;
                                }
                            }
                        }
                        continue;
                    } else if tokens[colon_idx].text(src) == "{" {
                        if let Some(b_close) = find_matching_delim(src, tokens, colon_idx, close_b) {
                            out.push_str(&src[tokens[cp].end..tokens[colon_idx].start]);
                            out.push('{');
                            strip_range(src, tokens, colon_idx + 1, b_close, false, out);
                            out.push('}');
                            i = b_close + 1;
                            at_member_start = true;
                            continue;
                        }
                    }
                }
                i = cp + 1;
                continue;
            }
        }
        if !at_member_start && (txt == "?" || txt == "!") {
            if let Some(ns) = next_sig(tokens, i + 1) {
                if tokens[ns].text(src) == ":" || tokens[ns].text(src) == ";" {
                    i += 1;
                    continue;
                }
            }
        }
        if !at_member_start && txt == ":" {
            // Field type annotation: `mode: "normal" | "insert" = "insert";`
            let type_end = find_type_end(src, tokens, i + 1, close_b, false);
            emit_erased(src, tokens, i, type_end, out);
            i = type_end;
            continue;
        }
        if txt == ";" || txt == "}" {
            at_member_start = true;
            out.push_str(txt);
            i += 1;
            continue;
        }
        if txt == "=" {
            let eq_end = find_stmt_end(src, tokens, i + 1, close_b);
            out.push('=');
            strip_range(src, tokens, i + 1, eq_end, false, out);
            i = eq_end;
            at_member_start = true;
            continue;
        }
        at_member_start = false;
        out.push_str(txt);
        i += 1;
    }
    out.push('}');
}

fn strip_range(src: &str, tokens: &[Token], start: usize, end: usize, _in_expr: bool, out: &mut String) {
    let mut i = start;
    while i < end {
        let t = &tokens[i];
        if t.is_trivia() {
            out.push_str(t.text(src));
            i += 1;
            continue;
        }
        let txt = t.text(src);

        // 1. import statements
        if t.kind == TokKind::Ident && txt == "import" {
            let prev_dot = prev_sig(tokens, i).is_some_and(|p| tokens[p].text(src) == ".");
            if !prev_dot {
                i = process_import_stmt(src, tokens, i, end, out);
                continue;
            }
        }

        // 2. export statements / declarations
        if t.kind == TokKind::Ident && txt == "export" {
            if let Some(ns) = next_sig(tokens, i + 1) {
                let ntxt = tokens[ns].text(src);
                if ntxt == "type" {
                    // Check if `export type { ... }` or `export type Foo = ...`
                    let stmt_end = find_stmt_end(src, tokens, i + 1, end);
                    emit_erased(src, tokens, i, stmt_end, out);
                    i = stmt_end;
                    continue;
                }
                if ntxt == "interface" {
                    if let Some(body_open) = (ns + 1..end).find(|&k| tokens[k].text(src) == "{") {
                        if let Some(body_close) = find_matching_delim(src, tokens, body_open, end) {
                            emit_erased(src, tokens, i, body_close + 1, out);
                            i = body_close + 1;
                            continue;
                        }
                    }
                }
                if ntxt == "enum" {
                    if let Some((enum_js, next_idx)) = transform_enum(src, tokens, ns, end, true) {
                        out.push_str(&enum_js);
                        i = next_idx;
                        continue;
                    }
                }
                if ntxt == "const" {
                    if let Some(nn) = next_sig(tokens, ns + 1) {
                        if tokens[nn].text(src) == "enum" {
                            if let Some((enum_js, next_idx)) = transform_enum(src, tokens, nn, end, true) {
                                out.push_str(&enum_js);
                                i = next_idx;
                                continue;
                            }
                        }
                    }
                }
                if ntxt == "declare" || ntxt == "abstract" {
                    if ntxt == "declare" {
                        let stmt_end = find_stmt_end(src, tokens, i + 1, end);
                        emit_erased(src, tokens, i, stmt_end, out);
                        i = stmt_end;
                        continue;
                    }
                }
                if ntxt == "{" {
                    if let Some(close_b) = find_matching_delim(src, tokens, ns, end) {
                        let stmt_end = find_stmt_end(src, tokens, close_b + 1, end);
                        let inner = &src[tokens[ns].end..tokens[close_b].start];
                        let kept: Vec<&str> = inner
                            .split(',')
                            .map(|p| p.trim())
                            .filter(|p| !p.is_empty() && !p.starts_with("type ") && !p.starts_with("type\t"))
                            .collect();
                        if kept.is_empty() {
                            emit_erased(src, tokens, i, stmt_end, out);
                        } else {
                            let tail = src[tokens[close_b].end..tokens[stmt_end - 1].end].trim_start();
                            out.push_str(&format!("export {{ {} }} {tail}", kept.join(", ")));
                        }
                        i = stmt_end;
                        continue;
                    }
                }
            }
        }

        // 3. interface / type / declare / enum declarations (at statement position)
        let at_stmt_boundary = prev_sig(tokens, i).is_none_or(|p| {
            matches!(tokens[p].text(src), ";" | "{" | "}" | "export") || has_newline_between(tokens, p, i)
        });
        if at_stmt_boundary && t.kind == TokKind::Ident {
            if txt == "interface" {
                if let Some(name_tok) = next_sig(tokens, i + 1) {
                    if tokens[name_tok].kind == TokKind::Ident {
                        if let Some(body_open) = (name_tok + 1..end).find(|&k| tokens[k].text(src) == "{") {
                            if let Some(body_close) = find_matching_delim(src, tokens, body_open, end) {
                                emit_erased(src, tokens, i, body_close + 1, out);
                                i = body_close + 1;
                                continue;
                            }
                        }
                    }
                }
            }
            if txt == "type" {
                if let Some(name_tok) = next_sig(tokens, i + 1) {
                    if tokens[name_tok].kind == TokKind::Ident {
                        if let Some(after_name) = next_sig(tokens, name_tok + 1) {
                            let atxt = tokens[after_name].text(src);
                            if atxt == "=" || atxt == "<" {
                                let stmt_end = find_stmt_end(src, tokens, i + 1, end);
                                emit_erased(src, tokens, i, stmt_end, out);
                                i = stmt_end;
                                continue;
                            }
                        }
                    }
                }
            }
            if txt == "declare" {
                let stmt_end = find_stmt_end(src, tokens, i + 1, end);
                emit_erased(src, tokens, i, stmt_end, out);
                i = stmt_end;
                continue;
            }
            if txt == "enum" {
                if let Some((enum_js, next_idx)) = transform_enum(src, tokens, i, end, false) {
                    out.push_str(&enum_js);
                    i = next_idx;
                    continue;
                }
            }
            if txt == "const" {
                if let Some(ns) = next_sig(tokens, i + 1) {
                    if tokens[ns].text(src) == "enum" {
                        if let Some((enum_js, next_idx)) = transform_enum(src, tokens, ns, end, false) {
                            out.push_str(&enum_js);
                            i = next_idx;
                            continue;
                        }
                    }
                }
            }
            if txt == "abstract" {
                if let Some(ns) = next_sig(tokens, i + 1) {
                    if tokens[ns].text(src) == "class" {
                        i = ns;
                        continue;
                    }
                }
            }
        }

        // 4. `const` / `let` / `var` variable type annotation: `const x: Foo = 1` / `let y: Bar;`
        if t.kind == TokKind::Ident && matches!(txt, "const" | "let" | "var") {
            out.push_str(txt);
            i += 1;
            // Walk binding pattern until `=`, `;`, `,`, `in`, `of`, `)` or newline
            let mut depth = 0usize;
            while i < end {
                let vt = &tokens[i];
                if vt.is_trivia() {
                    out.push_str(vt.text(src));
                    i += 1;
                    continue;
                }
                let vtxt = vt.text(src);
                if depth == 0 {
                    if vtxt == ":" {
                        let type_end = find_type_end(src, tokens, i + 1, end, false);
                        emit_erased(src, tokens, i, type_end, out);
                        i = type_end;
                        continue;
                    }
                    if vtxt == "!" {
                        if let Some(ns) = next_sig(tokens, i + 1) {
                            if tokens[ns].text(src) == ":" {
                                let type_end = find_type_end(src, tokens, ns + 1, end, false);
                                emit_erased(src, tokens, i, type_end, out);
                                i = type_end;
                                continue;
                            }
                        }
                    }
                    if matches!(vtxt, "=" | ";" | "of" | "in" | ")" | "}") {
                        break;
                    }
                }
                match vtxt {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth = depth.saturating_sub(1),
                    _ => {}
                }
                out.push_str(vtxt);
                i += 1;
            }
            continue;
        }

        // 5. `function [name]<T>(params): RetType { body }`
        if t.kind == TokKind::Ident && txt == "function" {
            out.push_str("function");
            i += 1;
            while i < end {
                let ft = &tokens[i];
                if ft.is_trivia() {
                    out.push_str(ft.text(src));
                    i += 1;
                    continue;
                }
                let ftxt = ft.text(src);
                if ftxt == "<" {
                    if let Some((_gt, op)) = looks_like_generic_angle_before_paren(src, tokens, i, end) {
                        emit_erased(src, tokens, i, op, out);
                        i = op;
                        continue;
                    }
                }
                if ftxt == "(" {
                    if let Some(cp) = find_matching_delim(src, tokens, i, end) {
                        let mut unused = Vec::new();
                        out.push_str(&strip_params_list(src, tokens, i, cp, &mut unused));
                        i = cp + 1;
                        if let Some(ns) = next_sig(tokens, i) {
                            if tokens[ns].text(src) == ":" {
                                let ret_end = find_return_type_end(src, tokens, ns + 1, end);
                                emit_erased(src, tokens, ns, ret_end, out);
                                i = ret_end;
                            }
                        }
                        break;
                    }
                }
                out.push_str(ftxt);
                if ftxt == "{" {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // 6. `class Name<T> extends Base<U> implements I1, I2 { ... }`
        if t.kind == TokKind::Ident && txt == "class" {
            out.push_str("class");
            i += 1;
            while i < end {
                let ct = &tokens[i];
                if ct.is_trivia() {
                    out.push_str(ct.text(src));
                    i += 1;
                    continue;
                }
                let ctxt = ct.text(src);
                if ctxt == "<" {
                    if let Some(gt) = find_matching_delim(src, tokens, i, end) {
                        emit_erased(src, tokens, i, gt + 1, out);
                        i = gt + 1;
                        continue;
                    }
                }
                if ct.kind == TokKind::Ident && ctxt == "implements" {
                    // Skip until `{`
                    if let Some(open_b) = (i + 1..end).find(|&k| tokens[k].text(src) == "{") {
                        emit_erased(src, tokens, i, open_b, out);
                        i = open_b;
                        continue;
                    }
                }
                if ctxt == "{" {
                    if let Some(close_b) = find_matching_delim(src, tokens, i, end) {
                        process_class_body(src, tokens, i, close_b, out);
                        i = close_b + 1;
                        break;
                    }
                }
                out.push_str(ctxt);
                i += 1;
            }
            continue;
        }

        // 7. Generic call `<T, U>(...)` or arrow `<T>(x: T) => ...`
        if txt == "<" {
            if let Some((_gt, op)) = looks_like_generic_angle_before_paren(src, tokens, i, end) {
                let is_after_expr_or_new = prev_sig(tokens, i)
                    .is_some_and(|p| tokens[p].kind == TokKind::Ident || matches!(tokens[p].text(src), "]" | ")"));
                let is_arrow_generic = find_matching_delim(src, tokens, op, end)
                    .is_some_and(|cp| is_arrow_param_paren(src, tokens, op, cp, end));
                if is_after_expr_or_new || is_arrow_generic {
                    emit_erased(src, tokens, i, op, out);
                    i = op;
                    continue;
                }
            }
        }

        // 8. Arrow parameter parens `(a: T, b?: U): Ret => ...` or async `(a: T): Promise<void> =>`
        if txt == "(" {
            if let Some(cp) = find_matching_delim(src, tokens, i, end) {
                if is_arrow_param_paren(src, tokens, i, cp, end) {
                    let mut unused = Vec::new();
                    out.push_str(&strip_params_list(src, tokens, i, cp, &mut unused));
                    i = cp + 1;
                    if let Some(ns) = next_sig(tokens, i) {
                        if tokens[ns].text(src) == ":" {
                            let ret_end = find_return_type_end(src, tokens, ns + 1, end);
                            emit_erased(src, tokens, ns, ret_end, out);
                            i = ret_end;
                        }
                    }
                    continue;
                }
            }
        }

        // 9. `expr as Type` / `expr satisfies Type`
        if t.kind == TokKind::Ident && (txt == "as" || txt == "satisfies") {
            let valid_lhs = prev_sig(tokens, i).is_some_and(|p| {
                let pt = tokens[p].text(src);
                !matches!(pt, "." | "import" | "export" | "{" | ",") && !matches!(tokens[p].kind, TokKind::Punct)
                    || matches!(tokens[p].text(src), ")" | "]" | "}")
            });
            if valid_lhs {
                // Skip `as Type` / `satisfies Type`
                let type_end = find_expr_type_end(src, tokens, i + 1, end);
                emit_erased(src, tokens, i, type_end, out);
                i = type_end;
                continue;
            }
        }

        // 10. Postfix non-null assertion `expr!`
        if txt == "!" {
            let is_postfix = prev_sig(tokens, i).is_some_and(|p| {
                let pt = tokens[p].text(src);
                matches!(tokens[p].kind, TokKind::Ident | TokKind::String | TokKind::Number)
                    && !matches!(
                        pt,
                        "return"
                            | "throw"
                            | "typeof"
                            | "void"
                            | "delete"
                            | "await"
                            | "yield"
                            | "case"
                            | "else"
                            | "in"
                            | "of"
                    )
                    || matches!(pt, ")" | "]")
            });
            if is_postfix {
                i += 1;
                continue;
            }
        }

        out.push_str(txt);
        i += 1;
    }
}

/// Rewrite CommonJS `.cjs` or `module.exports`/`require` script into an ESM wrapper when needed.
pub fn wrap_commonjs_if_needed(js: &str, filename: &str) -> String {
    let lower = filename.to_ascii_lowercase();
    let is_cjs_ext = lower.ends_with(".cjs") || lower.ends_with(".cts");
    let tokens = lex(js);
    let has_esm = tokens.iter().enumerate().any(|(idx, t)| {
        t.kind == TokKind::Ident
            && matches!(t.text(js), "import" | "export")
            && prev_sig(&tokens, idx).is_none_or(|p| tokens[p].text(js) != ".")
            && next_sig(&tokens, idx + 1).is_some_and(|n| tokens[n].text(js) != "(" && tokens[n].text(js) != ".")
    });
    let uses_cjs = tokens
        .iter()
        .any(|t| t.kind == TokKind::Ident && matches!(t.text(js), "module" | "exports" | "require"));
    if !is_cjs_ext && (has_esm || !uses_cjs) {
        return js.to_string();
    }
    if has_esm {
        return js.to_string();
    }
    let dir = std::path::Path::new(filename)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or(".");
    format!(
        "const module = {{ exports: {{}} }};\nconst exports = module.exports;\nconst __filename = {};\nconst __dirname = {};\nconst require = globalThis.__pi_create_require(__dirname, __filename);\n(function(exports, require, module, __filename, __dirname) {{\n{js}\n}})(exports, require, module, __filename, __dirname);\nexport default module.exports;",
        pi_js::json::stringify(filename),
        pi_js::json::stringify(dir),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ts_examples_cleanly() {
        let hello = r#"
import { Type } from "@earendil-works/pi-ai";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

const helloTool = defineTool({
    name: "hello",
    parameters: Type.Object({ name: Type.String() }),
    async execute(_id, params, _signal, _onUpdate, _ctx) {
        return { content: [{ type: "text", text: `Hello, ${params.name}!` }] };
    },
});

export default function (pi: ExtensionAPI) {
    pi.registerTool(helloTool);
}
"#;
        let out = transpile_ts_to_js(hello, "hello.ts");
        assert!(out.contains("import { Type } from \"@earendil-works/pi-ai\";"));
        assert!(out.contains("import { defineTool } from \"@earendil-works/pi-coding-agent\";"));
        assert!(out.contains("export default function (pi)"));
        assert!(!out.contains("ExtensionAPI"));
    }

    #[test]
    fn preserves_regex_with_slashes_and_strips_as_and_generics() {
        let src = r#"
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
export interface Foo { a: string; }
type Bar = "x" | "y";
export default function (pi: ExtensionAPI): void {
    const patterns = [/\brm\s+(-rf?|--recursive)/i, /\bsudo\b/i];
    pi.on("tool_call", async (event, ctx) => {
        const command = event.input.command as string;
        const x = event.input.opt!;
        const y = ctx.ui.custom<string>(async () => "ok");
        const z = (a as Foo | null)?.a satisfies string | undefined;
        return patterns.some((p) => p.test(command)) ? { block: true } : undefined;
    });
}
"#;
        let out = transpile_ts_to_js(src, "gate.ts");
        assert!(!out.contains("interface Foo"));
        assert!(!out.contains("type Bar"));
        assert!(out.contains(r"/\brm\s+(-rf?|--recursive)/i"));
        assert!(out.contains("const command = event.input.command ;"));
        assert!(out.contains("const x = event.input.opt;"));
        assert!(out.contains("const y = ctx.ui.custom(async () => \"ok\");"));
    }
}

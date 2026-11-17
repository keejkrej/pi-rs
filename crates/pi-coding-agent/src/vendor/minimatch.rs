//! Port of vendor minimatch@10.2.6 (default options + nocase) (PORTING.md §2.6).

use fancy_regex::Regex;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MinimatchOptions {
    pub nocase: bool,
    pub dot: bool,
    pub nobrace: bool,
    pub nocomment: bool,
    pub nonegate: bool,
    pub noglobstar: bool,
    pub noext: bool,
    pub match_base: bool,
    pub flip_negate: bool,
    pub partial: bool,
    pub windows_paths_no_escape: bool,
}

#[derive(Clone, Debug)]
pub struct Minimatch {
    pub pattern: String,
    pub options: MinimatchOptions,
    pub negate: bool,
    pub comment: bool,
    pub empty: bool,
    compiled: Vec<Regex>,
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut escape = false;

    while i < n {
        let c = chars[i];
        if escape {
            escape = false;
            i += 1;
            continue;
        }
        if c == '\\' {
            escape = true;
            i += 1;
            continue;
        }
        if c == '{' {
            let start = i;
            let mut depth = 1;
            let mut j = i + 1;
            let mut esc2 = false;
            let mut commas = Vec::new();
            while j < n && depth > 0 {
                let cj = chars[j];
                if esc2 {
                    esc2 = false;
                } else if cj == '\\' {
                    esc2 = true;
                } else if cj == '{' {
                    depth += 1;
                } else if cj == '}' {
                    depth -= 1;
                } else if cj == ',' && depth == 1 {
                    commas.push(j);
                }
                j += 1;
            }
            if depth == 0 {
                let end = j - 1;
                let prefix: String = chars[..start].iter().collect();
                let inner: String = chars[start + 1..end].iter().collect();
                let suffix: String = chars[end + 1..].iter().collect();
                let mut results = Vec::new();

                if !commas.is_empty() {
                    let mut last = start + 1;
                    let mut alts = Vec::new();
                    for c_idx in commas {
                        alts.push(chars[last..c_idx].iter().collect::<String>());
                        last = c_idx + 1;
                    }
                    alts.push(chars[last..end].iter().collect::<String>());
                    for alt in alts {
                        for expanded_alt in expand_braces(&alt) {
                            for expanded_tail in expand_braces(&suffix) {
                                results.push(format!("{prefix}{expanded_alt}{expanded_tail}"));
                            }
                        }
                    }
                    return results;
                } else if let Some((a_str, b_str)) = inner.split_once("..")
                    && let (Ok(a), Ok(b)) = (a_str.parse::<i64>(), b_str.parse::<i64>())
                {
                    let step = if a <= b { 1 } else { -1 };
                    let mut cur = a;
                    let mut count = 0usize;
                    while count < 10_000 {
                        for expanded_tail in expand_braces(&suffix) {
                            results.push(format!("{prefix}{cur}{expanded_tail}"));
                        }
                        if cur == b {
                            break;
                        }
                        cur += step;
                        count += 1;
                    }
                    return results;
                }
            }
        }
        i += 1;
    }

    vec![pattern.to_owned()]
}

pub fn brace_expand(pattern: &str, options: &MinimatchOptions) -> Vec<String> {
    if options.nobrace || !pattern.contains('{') {
        vec![pattern.to_owned()]
    } else {
        expand_braces(pattern)
    }
}

fn escape_regex_char(c: char, buf: &mut String) {
    if matches!(
        c,
        '.' | '+' | '*' | '?' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
    ) {
        buf.push('\\');
    }
    buf.push(c);
}

fn find_matching_paren(chars: &[char], open_idx: usize) -> Option<usize> {
    let mut depth = 1;
    let mut i = open_idx + 1;
    let mut esc = false;
    while i < chars.len() {
        let c = chars[i];
        if esc {
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn split_top_level_pipes(inner: &[char]) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut esc = false;
    let mut start = 0usize;
    for (idx, &c) in inner.iter().enumerate() {
        if esc {
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth = depth.saturating_sub(1);
        } else if c == '|' && depth == 0 {
            parts.push(inner[start..idx].iter().collect());
            start = idx + 1;
        }
    }
    parts.push(inner[start..].iter().collect());
    parts
}

fn segment_to_regex(segment: &str, opts: &MinimatchOptions) -> String {
    let chars: Vec<char> = segment.chars().collect();
    let mut out = String::new();
    let mut i = 0;

    // Leading dot protection unless `opts.dot` or segment explicitly starts with `.` or escaped `.`
    if !opts.dot && !segment.starts_with('.') {
        out.push_str(r"(?!\.)");
    }

    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && !opts.windows_paths_no_escape {
            if i + 1 < chars.len() {
                escape_regex_char(chars[i + 1], &mut out);
                i += 2;
                continue;
            }
            out.push_str(r"\\");
            i += 1;
            continue;
        }

        // Extglob: ?(...), *(...), +(...), @(...), !(...)
        if !opts.noext
            && matches!(c, '?' | '*' | '+' | '@' | '!')
            && i + 1 < chars.len()
            && chars[i + 1] == '('
            && let Some(close) = find_matching_paren(&chars, i + 1)
        {
            let alts = split_top_level_pipes(&chars[i + 2..close]);
            let alt_patterns: Vec<String> = alts
                .into_iter()
                .map(|a| {
                    segment_to_regex(
                        &a,
                        &MinimatchOptions {
                            dot: true,
                            ..opts.clone()
                        },
                    )
                })
                .collect();
            let joined = alt_patterns.join("|");
            match c {
                '?' => out.push_str(&format!("(?:{joined})?")),
                '*' => out.push_str(&format!("(?:{joined})*")),
                '+' => out.push_str(&format!("(?:{joined})+")),
                '@' => out.push_str(&format!("(?:{joined})")),
                '!' => out.push_str(&format!("(?!(?:{joined})$)[^/]*")),
                _ => {}
            }
            i = close + 1;
            continue;
        }

        match c {
            '*' => {
                while i + 1 < chars.len() && chars[i + 1] == '*' {
                    i += 1;
                }
                out.push_str("[^/]*");
            }
            '?' => out.push_str("[^/]"),
            '[' => {
                if let Some(rel_end) = chars[i + 1..].iter().position(|&ch| ch == ']') {
                    let close = i + 1 + rel_end;
                    let body: String = chars[i + 1..close].iter().collect();
                    let fixed = if let Some(rest) = body.strip_prefix('!') {
                        format!("[^{rest}]")
                    } else {
                        format!("[{body}]")
                    };
                    out.push_str(&fixed);
                    i = close + 1;
                    continue;
                }
                out.push_str(r"\[");
            }
            other => escape_regex_char(other, &mut out),
        }
        i += 1;
    }
    out
}

fn compile_single_pattern(raw_pattern: &str, opts: &MinimatchOptions) -> Option<Regex> {
    let mut parts: Vec<&str> = raw_pattern.split('/').collect();
    if parts.len() > 1 && parts.last() == Some(&"") {
        // Trailing slash matches empty tail / directory contents
    }
    if opts.noglobstar {
        parts = parts.into_iter().map(|p| if p == "**" { "*" } else { p }).collect();
    }

    // Collapse adjacent `**`
    let mut collapsed: Vec<&str> = Vec::new();
    for p in parts {
        if p == "**" && collapsed.last() == Some(&"**") {
            continue;
        }
        collapsed.push(p);
    }

    let mut re = String::from("^");
    if opts.match_base && collapsed.len() == 1 && collapsed[0] != "**" {
        re.push_str("(?:.*/)?");
    }

    let len = collapsed.len();
    for (idx, part) in collapsed.iter().enumerate() {
        let is_last = idx + 1 == len;
        if *part == "**" {
            let dot_guard = if opts.dot { "" } else { r"(?!\.[^/]*)" };
            if is_last {
                if idx == 0 {
                    re.push_str(if opts.dot {
                        ".*"
                    } else {
                        r"(?:(?!/\.)[^/]*(?:/(?!/\.)[^/]*)*)?"
                    });
                } else {
                    re.push_str(if opts.dot { "(?:/.*)?" } else { r"(?:/(?!/\.)[^/]*)*" });
                }
            } else if opts.dot {
                re.push_str("(?:.*/)?");
            } else {
                let _ = dot_guard;
                re.push_str(r"(?:(?!\.)[^/]*/)*");
            }
        } else {
            re.push_str(&segment_to_regex(part, opts));
            if !is_last && collapsed[idx + 1] != "**" {
                re.push('/');
            } else if !is_last && collapsed[idx + 1] == "**" {
                // `**` handles leading `/` when `idx > 0`, or optional tail
                if idx + 2 < len {
                    re.push('/');
                }
            }
        }
    }
    re.push('$');

    let final_re = if opts.nocase { format!("(?i){re}") } else { re };
    Regex::new(&final_re).ok()
}

impl Minimatch {
    pub fn new(pattern: &str, options: MinimatchOptions) -> Self {
        let mut pat = pattern.to_owned();
        if !options.nocomment && pat.starts_with('#') {
            return Self {
                pattern: pat,
                options,
                negate: false,
                comment: true,
                empty: false,
                compiled: Vec::new(),
            };
        }

        let mut negate = false;
        if !options.nonegate {
            while let Some(rest) = pat.strip_prefix('!') {
                negate = !negate;
                pat = rest.to_owned();
            }
        }

        if pat.trim().is_empty() && pat.is_empty() {
            return Self {
                pattern: pat,
                options,
                negate,
                comment: false,
                empty: true,
                compiled: Vec::new(),
            };
        }

        let expanded = brace_expand(&pat, &options);
        let mut compiled = Vec::new();
        for item in expanded {
            if let Some(re) = compile_single_pattern(&item, &options) {
                compiled.push(re);
            }
        }

        Self {
            pattern: pat,
            options,
            negate,
            comment: false,
            empty: false,
            compiled,
        }
    }

    pub fn match_str(&self, p: &str) -> bool {
        if self.comment {
            return false;
        }
        if self.empty {
            return p.is_empty() != self.negate;
        }
        let target = if pi_js::env::platform() == "win32" && !self.options.windows_paths_no_escape {
            p.replace('\\', "/")
        } else {
            p.to_owned()
        };
        let matched = self.compiled.iter().any(|re| re.is_match(&target).unwrap_or(false));
        if self.options.flip_negate {
            return matched;
        }
        if self.negate { !matched } else { matched }
    }
}

pub fn minimatch(p: &str, pattern: &str, options: Option<MinimatchOptions>) -> bool {
    Minimatch::new(pattern, options.unwrap_or_default()).match_str(p)
}

pub fn escape(s: &str, windows_paths_no_escape: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for (idx, ch) in s.chars().enumerate() {
        let special = matches!(ch, '[' | ']' | '?' | '*' | '{' | '}' | '(' | ')' | '|')
            || (!windows_paths_no_escape && ch == '\\')
            || (idx == 0 && matches!(ch, '!' | '#'));
        if special {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

pub fn unescape(s: &str, windows_paths_no_escape: bool) -> String {
    if windows_paths_no_escape {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
                continue;
            }
        }
        out.push(c);
    }
    out
}

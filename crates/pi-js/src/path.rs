//! pi_js::path (Rust-only; API contract: PORTING.md Appendix A).
//!
//! Port of Node v24 `lib/path.js`. The top-level functions are the platform default
//! (`win32` on Windows, `posix` elsewhere), exactly like `require("node:path")`.
//! [`posix`] and [`win32`] expose the same functions for code that uses `path.posix` /
//! `path.win32` explicitly.
//!
//! All algorithms operate on UTF-16 code units so that indices, `toLowerCase` length
//! checks and negative `slice` arguments behave exactly as in JavaScript. Every slice
//! boundary that reaches the output is adjacent to an ASCII separator, dot or colon, so
//! converting back to UTF-8 is lossless.
//!
//! `process.cwd()` (used by `resolve`, `relative` and `toNamespacedPath`) comes from
//! [`crate::env::cwd`], so `CwdGuard` overrides apply.

/// The object returned by `path.parse()` and accepted by `path.format()`.
///
/// For `format`, an empty string plays the role of a missing (falsy) property.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedPath {
    pub root: String,
    pub dir: String,
    pub base: String,
    pub ext: String,
    pub name: String,
}

const CHAR_DOT: u16 = b'.' as u16;
const CHAR_FORWARD_SLASH: u16 = b'/' as u16;
const CHAR_BACKWARD_SLASH: u16 = b'\\' as u16;
const CHAR_COLON: u16 = b':' as u16;
const CHAR_QUESTION_MARK: u16 = b'?' as u16;
const CHAR_UPPERCASE_A: u16 = b'A' as u16;
const CHAR_UPPERCASE_Z: u16 = b'Z' as u16;
const CHAR_LOWERCASE_A: u16 = b'a' as u16;
const CHAR_LOWERCASE_Z: u16 = b'z' as u16;

fn enc(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn dec(v: &[u16]) -> String {
    String::from_utf16_lossy(v)
}

/// `String.prototype.charCodeAt` returning NaN (here `None`) past the end.
fn char_code_at(s: &[u16], i: usize) -> Option<u16> {
    s.get(i).copied()
}

/// `String.prototype.slice(start, end)` with JS negative-index semantics.
fn js_slice(s: &[u16], start: isize, end: Option<isize>) -> &[u16] {
    let len = s.len() as isize;
    let norm = |v: isize| -> usize {
        if v < 0 {
            (len + v).max(0) as usize
        } else {
            v.min(len) as usize
        }
    };
    let from = norm(start);
    let to = norm(end.unwrap_or(len));
    if from >= to { &s[0..0] } else { &s[from..to] }
}

fn index_of(s: &[u16], needle: u16, from: usize) -> isize {
    s.iter()
        .enumerate()
        .skip(from)
        .find(|(_, c)| **c == needle)
        .map(|(i, _)| i as isize)
        .unwrap_or(-1)
}

fn last_index_of(s: &[u16], needle: u16) -> isize {
    s.iter().rposition(|c| *c == needle).map(|i| i as isize).unwrap_or(-1)
}

fn concat(parts: &[&[u16]]) -> Vec<u16> {
    let mut out = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

fn to_lower(s: &[u16]) -> Vec<u16> {
    enc(&dec(s).to_lowercase())
}

fn is_path_separator(code: u16) -> bool {
    code == CHAR_FORWARD_SLASH || code == CHAR_BACKWARD_SLASH
}

fn is_posix_path_separator(code: u16) -> bool {
    code == CHAR_FORWARD_SLASH
}

fn opt_is_path_separator(code: Option<u16>) -> bool {
    code.is_some_and(is_path_separator)
}

const WINDOWS_RESERVED_NAMES: [&str; 28] = [
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
];

fn is_windows_reserved_name(path: &[u16], colon_index: isize) -> bool {
    let device_part = js_slice(path, 0, Some(colon_index));
    match String::from_utf16(device_part) {
        Ok(s) => {
            let upper = s.to_uppercase();
            WINDOWS_RESERVED_NAMES.contains(&upper.as_str())
        }
        // A lone surrogate can never match a reserved name.
        Err(_) => false,
    }
}

fn is_windows_device_root(code: u16) -> bool {
    (CHAR_UPPERCASE_A..=CHAR_UPPERCASE_Z).contains(&code) || (CHAR_LOWERCASE_A..=CHAR_LOWERCASE_Z).contains(&code)
}

fn opt_is_windows_device_root(code: Option<u16>) -> bool {
    code.is_some_and(is_windows_device_root)
}

/// Resolves . and .. elements in a path with directory names
fn normalize_string(path: &[u16], allow_above_root: bool, separator: u16, is_sep: fn(u16) -> bool) -> Vec<u16> {
    let mut res: Vec<u16> = Vec::new();
    let mut last_segment_length: isize = 0;
    let mut last_slash: isize = -1;
    let mut dots: i32 = 0;
    let mut code: u16 = 0;
    let len = path.len();
    let mut i: usize = 0;
    while i <= len {
        if i < len {
            code = path[i];
        } else if is_sep(code) {
            break;
        } else {
            code = CHAR_FORWARD_SLASH;
        }

        if is_sep(code) {
            if last_slash == i as isize - 1 || dots == 1 {
                // NOOP
            } else if dots == 2 {
                if res.len() < 2
                    || last_segment_length != 2
                    || res[res.len() - 1] != CHAR_DOT
                    || res[res.len() - 2] != CHAR_DOT
                {
                    if res.len() > 2 {
                        let last_slash_index = res.len() as isize - last_segment_length - 1;
                        if last_slash_index == -1 {
                            res.clear();
                            last_segment_length = 0;
                        } else {
                            res.truncate(last_slash_index.max(0) as usize);
                            last_segment_length = res.len() as isize - 1 - last_index_of(&res, separator);
                        }
                        last_slash = i as isize;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i as isize;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !res.is_empty() {
                        res.push(separator);
                    }
                    res.push(CHAR_DOT);
                    res.push(CHAR_DOT);
                    last_segment_length = 2;
                }
            } else {
                let seg = &path[(last_slash + 1) as usize..i];
                if !res.is_empty() {
                    res.push(separator);
                }
                res.extend_from_slice(seg);
                last_segment_length = i as isize - last_slash - 1;
            }
            last_slash = i as isize;
            dots = 0;
        } else if code == CHAR_DOT && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    res
}

fn format_ext(ext: &str) -> String {
    if ext.is_empty() {
        String::new()
    } else if ext.starts_with('.') {
        ext.to_string()
    } else {
        format!(".{ext}")
    }
}

fn format_impl(sep: &str, path_object: &ParsedPath) -> String {
    let dir = if !path_object.dir.is_empty() {
        &path_object.dir
    } else {
        &path_object.root
    };
    let base = if !path_object.base.is_empty() {
        path_object.base.clone()
    } else {
        format!("{}{}", path_object.name, format_ext(&path_object.ext))
    };
    if dir.is_empty() {
        return base;
    }
    if *dir == path_object.root {
        format!("{dir}{base}")
    } else {
        format!("{dir}{sep}{base}")
    }
}

/// Shared implementation of the `basename(path, suffix)` loop. `start` is the first
/// index that may belong to the base name (2 after a win32 drive letter).
fn basename_impl(path: &[u16], suffix: Option<&[u16]>, mut start: usize, is_sep: fn(u16) -> bool) -> Vec<u16> {
    let mut end: isize = -1;
    let mut matched_slash = true;

    if let Some(suffix) = suffix
        && !suffix.is_empty()
        && suffix.len() <= path.len()
    {
        if suffix == path {
            return Vec::new();
        }
        let mut ext_idx: isize = suffix.len() as isize - 1;
        let mut first_non_slash_end: isize = -1;
        let mut i = path.len() as isize - 1;
        while i >= start as isize {
            let code = path[i as usize];
            if is_sep(code) {
                // If we reached a path separator that was not part of a set of path
                // separators at the end of the string, stop now
                if !matched_slash {
                    start = (i + 1) as usize;
                    break;
                }
            } else {
                if first_non_slash_end == -1 {
                    // We saw the first non-path separator, remember this index in case
                    // we need it if the extension ends up not matching
                    matched_slash = false;
                    first_non_slash_end = i + 1;
                }
                if ext_idx >= 0 {
                    // Try to match the explicit extension
                    if code == suffix[ext_idx as usize] {
                        ext_idx -= 1;
                        if ext_idx == -1 {
                            // We matched the extension, so mark this as the end of our path
                            // component
                            end = i;
                        }
                    } else {
                        // Extension does not match, so our result is the entire path
                        // component
                        ext_idx = -1;
                        end = first_non_slash_end;
                    }
                }
            }
            i -= 1;
        }

        if start as isize == end {
            end = first_non_slash_end;
        } else if end == -1 {
            end = path.len() as isize;
        }
        return js_slice(path, start as isize, Some(end)).to_vec();
    }
    let mut i = path.len() as isize - 1;
    while i >= start as isize {
        if is_sep(path[i as usize]) {
            // If we reached a path separator that was not part of a set of path
            // separators at the end of the string, stop now
            if !matched_slash {
                start = (i + 1) as usize;
                break;
            }
        } else if end == -1 {
            // We saw the first non-path separator, mark this as the end of our
            // path component
            matched_slash = false;
            end = i + 1;
        }
        i -= 1;
    }

    if end == -1 {
        return Vec::new();
    }
    js_slice(path, start as isize, Some(end)).to_vec()
}

/// Shared implementation of `extname`; `start` is 2 after a win32 drive letter.
fn extname_impl(path: &[u16], start: usize, is_sep: fn(u16) -> bool) -> Vec<u16> {
    let mut start_dot: isize = -1;
    let mut start_part: isize = start as isize;
    let mut end: isize = -1;
    let mut matched_slash = true;
    // Track the state of characters (if any) we see before our first dot and
    // after any path separator we find
    let mut pre_dot_state = 0;

    let mut i = path.len() as isize - 1;
    while i >= start as isize {
        let code = path[i as usize];
        if is_sep(code) {
            // If we reached a path separator that was not part of a set of path
            // separators at the end of the string, stop now
            if !matched_slash {
                start_part = i + 1;
                break;
            }
            i -= 1;
            continue;
        }
        if end == -1 {
            // We saw the first non-path separator, mark this as the end of our
            // extension
            matched_slash = false;
            end = i + 1;
        }
        if code == CHAR_DOT {
            // If this is our first dot, mark it as the start of our extension
            if start_dot == -1 {
                start_dot = i;
            } else if pre_dot_state != 1 {
                pre_dot_state = 1;
            }
        } else if start_dot != -1 {
            // We saw a non-dot and non-path separator before our dot, so we should
            // have a good chance at having a non-empty extension
            pre_dot_state = -1;
        }
        i -= 1;
    }

    if start_dot == -1
        || end == -1
        // We saw a non-dot character immediately before the dot
        || pre_dot_state == 0
        // The (right-most) trimmed path component is exactly '..'
        || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part + 1)
    {
        return Vec::new();
    }
    js_slice(path, start_dot, Some(end)).to_vec()
}

/// Node `path.posix`.
pub mod posix {
    use super::*;

    /// Separator `/`.
    pub fn sep() -> &'static str {
        "/"
    }

    /// Delimiter `:`.
    pub fn delimiter() -> &'static str {
        ":"
    }

    fn posix_cwd() -> Vec<u16> {
        let cwd = crate::env::cwd();
        if cfg!(windows) {
            // Converts Windows' backslash path separators to POSIX forward slashes
            // and truncates any drive indicator
            let cwd: Vec<u16> = enc(&cwd)
                .into_iter()
                .map(|c| {
                    if c == CHAR_BACKWARD_SLASH {
                        CHAR_FORWARD_SLASH
                    } else {
                        c
                    }
                })
                .collect();
            let idx = index_of(&cwd, CHAR_FORWARD_SLASH, 0);
            return js_slice(&cwd, idx, None).to_vec();
        }
        // We're already on POSIX, no need for any transformations
        enc(&cwd)
    }

    /// path.resolve([from ...], to)
    pub fn resolve(args: &[&str]) -> String {
        dec(&resolve_u16(args))
    }

    pub(super) fn resolve_u16(args: &[&str]) -> Vec<u16> {
        if args.is_empty() || (args.len() == 1 && (args[0].is_empty() || args[0] == ".")) {
            let cwd = posix_cwd();
            if char_code_at(&cwd, 0) == Some(CHAR_FORWARD_SLASH) {
                return cwd;
            }
        }
        let mut resolved_path: Vec<u16> = Vec::new();
        let mut resolved_absolute = false;

        let mut i = args.len() as isize - 1;
        while i >= 0 && !resolved_absolute {
            let path = enc(args[i as usize]);
            i -= 1;

            // Skip empty entries
            if path.is_empty() {
                continue;
            }

            resolved_path = concat(&[&path, &[CHAR_FORWARD_SLASH], &resolved_path]);
            resolved_absolute = char_code_at(&path, 0) == Some(CHAR_FORWARD_SLASH);
        }

        if !resolved_absolute {
            let cwd = posix_cwd();
            resolved_path = concat(&[&cwd, &[CHAR_FORWARD_SLASH], &resolved_path]);
            resolved_absolute = char_code_at(&cwd, 0) == Some(CHAR_FORWARD_SLASH);
        }

        // At this point the path should be resolved to a full absolute path, but
        // handle relative paths to be safe (might happen when process.cwd() fails)

        // Normalize the path
        let resolved_path = normalize_string(
            &resolved_path,
            !resolved_absolute,
            CHAR_FORWARD_SLASH,
            is_posix_path_separator,
        );

        if resolved_absolute {
            return concat(&[&[CHAR_FORWARD_SLASH], &resolved_path]);
        }
        if !resolved_path.is_empty() {
            resolved_path
        } else {
            vec![CHAR_DOT]
        }
    }

    pub fn normalize(path: &str) -> String {
        dec(&normalize_u16(&enc(path)))
    }

    fn normalize_u16(path: &[u16]) -> Vec<u16> {
        if path.is_empty() {
            return vec![CHAR_DOT];
        }

        let is_absolute = char_code_at(path, 0) == Some(CHAR_FORWARD_SLASH);
        let trailing_separator = char_code_at(path, path.len() - 1) == Some(CHAR_FORWARD_SLASH);

        // Normalize the path
        let mut path = normalize_string(path, !is_absolute, CHAR_FORWARD_SLASH, is_posix_path_separator);

        if path.is_empty() {
            if is_absolute {
                return vec![CHAR_FORWARD_SLASH];
            }
            return if trailing_separator { enc("./") } else { vec![CHAR_DOT] };
        }
        if trailing_separator {
            path.push(CHAR_FORWARD_SLASH);
        }

        if is_absolute {
            concat(&[&[CHAR_FORWARD_SLASH], &path])
        } else {
            path
        }
    }

    pub fn is_absolute(path: &str) -> bool {
        path.as_bytes().first() == Some(&b'/')
    }

    pub fn join(args: &[&str]) -> String {
        if args.is_empty() {
            return ".".to_string();
        }

        let path: Vec<&str> = args.iter().copied().filter(|a| !a.is_empty()).collect();

        if path.is_empty() {
            return ".".to_string();
        }

        normalize(&path.join("/"))
    }

    pub fn relative(from: &str, to: &str) -> String {
        if from == to {
            return String::new();
        }

        // Trim leading forward slashes.
        let from = resolve_u16(&[from]);
        let to = resolve_u16(&[to]);

        if from == to {
            return String::new();
        }

        let from_start: usize = 1;
        let from_end = from.len();
        let from_len = from_end as isize - from_start as isize;
        let to_start: usize = 1;
        let to_len = to.len() as isize - to_start as isize;

        // Compare paths to find the longest common path from root
        let length = if from_len < to_len { from_len } else { to_len };
        let mut last_common_sep: isize = -1;
        let mut i: isize = 0;
        while i < length {
            let from_code = char_code_at(&from, (from_start as isize + i) as usize);
            if from_code != char_code_at(&to, (to_start as isize + i) as usize) {
                break;
            } else if from_code == Some(CHAR_FORWARD_SLASH) {
                last_common_sep = i;
            }
            i += 1;
        }
        if i == length {
            if to_len > length {
                if char_code_at(&to, (to_start as isize + i) as usize) == Some(CHAR_FORWARD_SLASH) {
                    // We get here if `from` is the exact base path for `to`.
                    // For example: from='/foo/bar'; to='/foo/bar/baz'
                    return dec(js_slice(&to, to_start as isize + i + 1, None));
                }
                if i == 0 {
                    // We get here if `from` is the root
                    // For example: from='/'; to='/foo'
                    return dec(js_slice(&to, to_start as isize + i, None));
                }
            } else if from_len > length {
                if char_code_at(&from, (from_start as isize + i) as usize) == Some(CHAR_FORWARD_SLASH) {
                    // We get here if `to` is the exact base path for `from`.
                    // For example: from='/foo/bar/baz'; to='/foo/bar'
                    last_common_sep = i;
                } else if i == 0 {
                    // We get here if `to` is the root.
                    // For example: from='/foo/bar'; to='/'
                    last_common_sep = 0;
                }
            }
        }

        let mut out: Vec<u16> = Vec::new();
        // Generate the relative path based on the path difference between `to`
        // and `from`.
        let mut i = from_start as isize + last_common_sep + 1;
        while i <= from_end as isize {
            if i == from_end as isize || char_code_at(&from, i as usize) == Some(CHAR_FORWARD_SLASH) {
                if !out.is_empty() {
                    out.push(CHAR_FORWARD_SLASH);
                }
                out.push(CHAR_DOT);
                out.push(CHAR_DOT);
            }
            i += 1;
        }

        // Lastly, append the rest of the destination (`to`) path that comes after
        // the common path parts.
        dec(&concat(&[
            &out,
            js_slice(&to, to_start as isize + last_common_sep, None),
        ]))
    }

    pub fn to_namespaced_path(path: &str) -> String {
        // Non-op on posix systems
        path.to_string()
    }

    pub fn dirname(path: &str) -> String {
        let path = enc(path);
        if path.is_empty() {
            return ".".to_string();
        }
        let has_root = char_code_at(&path, 0) == Some(CHAR_FORWARD_SLASH);
        let mut end: isize = -1;
        let mut matched_slash = true;
        let mut i = path.len() as isize - 1;
        while i >= 1 {
            if path[i as usize] == CHAR_FORWARD_SLASH {
                if !matched_slash {
                    end = i;
                    break;
                }
            } else {
                // We saw the first non-path separator
                matched_slash = false;
            }
            i -= 1;
        }

        if end == -1 {
            return if has_root { "/".to_string() } else { ".".to_string() };
        }
        if has_root && end == 1 {
            return "//".to_string();
        }
        dec(js_slice(&path, 0, Some(end)))
    }

    pub fn basename(path: &str, suffix: Option<&str>) -> String {
        let suffix = suffix.map(enc);
        dec(&basename_impl(
            &enc(path),
            suffix.as_deref(),
            0,
            is_posix_path_separator,
        ))
    }

    pub fn extname(path: &str) -> String {
        dec(&extname_impl(&enc(path), 0, is_posix_path_separator))
    }

    pub fn format(path_object: &ParsedPath) -> String {
        format_impl("/", path_object)
    }

    pub fn parse(path: &str) -> ParsedPath {
        let path = enc(path);
        let mut ret = ParsedPath::default();
        if path.is_empty() {
            return ret;
        }
        let is_absolute = char_code_at(&path, 0) == Some(CHAR_FORWARD_SLASH);
        let start: isize = if is_absolute {
            ret.root = "/".to_string();
            1
        } else {
            0
        };
        let mut start_dot: isize = -1;
        let mut start_part: isize = 0;
        let mut end: isize = -1;
        let mut matched_slash = true;
        let mut i = path.len() as isize - 1;

        // Track the state of characters (if any) we see before our first dot and
        // after any path separator we find
        let mut pre_dot_state = 0;

        // Get non-dir info
        while i >= start {
            let code = path[i as usize];
            if code == CHAR_FORWARD_SLASH {
                // If we reached a path separator that was not part of a set of path
                // separators at the end of the string, stop now
                if !matched_slash {
                    start_part = i + 1;
                    break;
                }
                i -= 1;
                continue;
            }
            if end == -1 {
                // We saw the first non-path separator, mark this as the end of our
                // extension
                matched_slash = false;
                end = i + 1;
            }
            if code == CHAR_DOT {
                // If this is our first dot, mark it as the start of our extension
                if start_dot == -1 {
                    start_dot = i;
                } else if pre_dot_state != 1 {
                    pre_dot_state = 1;
                }
            } else if start_dot != -1 {
                // We saw a non-dot and non-path separator before our dot, so we should
                // have a good chance at having a non-empty extension
                pre_dot_state = -1;
            }
            i -= 1;
        }

        if end != -1 {
            let start = if start_part == 0 && is_absolute { 1 } else { start_part };
            if start_dot == -1
                // We saw a non-dot character immediately before the dot
                || pre_dot_state == 0
                // The (right-most) trimmed path component is exactly '..'
                || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part + 1)
            {
                ret.base = dec(js_slice(&path, start, Some(end)));
                ret.name = ret.base.clone();
            } else {
                ret.name = dec(js_slice(&path, start, Some(start_dot)));
                ret.base = dec(js_slice(&path, start, Some(end)));
                ret.ext = dec(js_slice(&path, start_dot, Some(end)));
            }
        }

        if start_part > 0 {
            ret.dir = dec(js_slice(&path, 0, Some(start_part - 1)));
        } else if is_absolute {
            ret.dir = "/".to_string();
        }

        ret
    }
}

/// Node `path.win32`.
pub mod win32 {
    use super::*;

    /// Separator `\`.
    pub fn sep() -> &'static str {
        "\\"
    }

    /// Delimiter `;`.
    pub fn delimiter() -> &'static str {
        ";"
    }

    /// path.resolve([from ...], to)
    pub fn resolve(args: &[&str]) -> String {
        dec(&resolve_u16(args))
    }

    fn resolve_u16(args: &[&str]) -> Vec<u16> {
        let mut resolved_device: Vec<u16> = Vec::new();
        let mut resolved_tail: Vec<u16> = Vec::new();
        let mut resolved_absolute = false;

        let mut i = args.len() as isize - 1;
        while i >= -1 {
            let path: Vec<u16>;
            if i >= 0 {
                path = enc(args[i as usize]);

                // Skip empty entries
                if path.is_empty() {
                    i -= 1;
                    continue;
                }
            } else if resolved_device.is_empty() {
                let mut cwd = enc(&crate::env::cwd());
                // Fast path for current directory
                if args.is_empty()
                    || ((args.len() == 1 && (args[0].is_empty() || args[0] == "."))
                        && opt_is_path_separator(char_code_at(&cwd, 0)))
                {
                    if !cfg!(windows) {
                        cwd = cwd
                            .into_iter()
                            .map(|c| {
                                if c == CHAR_FORWARD_SLASH {
                                    CHAR_BACKWARD_SLASH
                                } else {
                                    c
                                }
                            })
                            .collect();
                    }
                    return cwd;
                }
                path = cwd;
            } else {
                // Windows has the concept of drive-specific current working
                // directories. If we've resolved a drive letter but not yet an
                // absolute path, get cwd for that drive, or the process cwd if
                // the drive cwd is not available. We're sure the device is not
                // a UNC path at this points, because UNC paths are always absolute.
                let env_cwd = crate::env::var(&format!("={}", dec(&resolved_device))).filter(|v| !v.is_empty());
                let mut p = enc(&env_cwd.unwrap_or_else(crate::env::cwd));

                // Verify that a cwd was found and that it actually points
                // to our drive. If not, default to the drive's root.
                if to_lower(js_slice(&p, 0, Some(2))) != to_lower(&resolved_device)
                    && char_code_at(&p, 2) == Some(CHAR_BACKWARD_SLASH)
                {
                    p = concat(&[&resolved_device, &[CHAR_BACKWARD_SLASH]]);
                }
                path = p;
            }

            let len = path.len();
            let mut root_end: usize = 0;
            let mut device: Vec<u16> = Vec::new();
            let mut is_absolute = false;
            let code = char_code_at(&path, 0);

            // Try to match a root
            if len == 1 {
                if opt_is_path_separator(code) {
                    // `path` contains just a path separator
                    root_end = 1;
                    is_absolute = true;
                }
            } else if opt_is_path_separator(code) {
                // Possible UNC root

                // If we started with a separator, we know we at least have an
                // absolute path of some kind (UNC or otherwise)
                is_absolute = true;

                if opt_is_path_separator(char_code_at(&path, 1)) {
                    // Matched double path separator at beginning
                    let mut j = 2;
                    let mut last = j;
                    // Match 1 or more non-path separators
                    while j < len && !is_path_separator(path[j]) {
                        j += 1;
                    }
                    if j < len && j != last {
                        let first_part = path[last..j].to_vec();
                        // Matched!
                        last = j;
                        // Match 1 or more path separators
                        while j < len && is_path_separator(path[j]) {
                            j += 1;
                        }
                        if j < len && j != last {
                            // Matched!
                            last = j;
                            // Match 1 or more non-path separators
                            while j < len && !is_path_separator(path[j]) {
                                j += 1;
                            }
                            if j == len || j != last {
                                if first_part != [CHAR_DOT] && first_part != [CHAR_QUESTION_MARK] {
                                    // We matched a UNC root
                                    device = concat(&[
                                        &[CHAR_BACKWARD_SLASH, CHAR_BACKWARD_SLASH],
                                        &first_part,
                                        &[CHAR_BACKWARD_SLASH],
                                        &path[last..j],
                                    ]);
                                    root_end = j;
                                } else {
                                    // We matched a device root (e.g. \\\\.\\PHYSICALDRIVE0)
                                    device = concat(&[&[CHAR_BACKWARD_SLASH, CHAR_BACKWARD_SLASH], &first_part]);
                                    root_end = 4;
                                }
                            }
                        }
                    }
                } else {
                    root_end = 1;
                }
            } else if opt_is_windows_device_root(code) && char_code_at(&path, 1) == Some(CHAR_COLON) {
                // Possible device root
                device = path[0..2].to_vec();
                root_end = 2;
                if len > 2 && opt_is_path_separator(char_code_at(&path, 2)) {
                    // Treat separator following drive name as an absolute path
                    // indicator
                    is_absolute = true;
                    root_end = 3;
                }
            }

            if !device.is_empty() {
                if !resolved_device.is_empty() {
                    if to_lower(&device) != to_lower(&resolved_device) {
                        // This path points to another device so it is not applicable
                        i -= 1;
                        continue;
                    }
                } else {
                    resolved_device = device;
                }
            }

            if resolved_absolute {
                if !resolved_device.is_empty() {
                    break;
                }
            } else {
                resolved_tail = concat(&[
                    js_slice(&path, root_end as isize, None),
                    &[CHAR_BACKWARD_SLASH],
                    &resolved_tail,
                ]);
                resolved_absolute = is_absolute;
                if is_absolute && !resolved_device.is_empty() {
                    break;
                }
            }
            i -= 1;
        }

        // At this point the path should be resolved to a full absolute path,
        // but handle relative paths to be safe (might happen when process.cwd()
        // fails)

        // Normalize the tail path
        let resolved_tail = normalize_string(
            &resolved_tail,
            !resolved_absolute,
            CHAR_BACKWARD_SLASH,
            is_path_separator,
        );

        if resolved_absolute {
            concat(&[&resolved_device, &[CHAR_BACKWARD_SLASH], &resolved_tail])
        } else {
            let out = concat(&[&resolved_device, &resolved_tail]);
            if out.is_empty() { vec![CHAR_DOT] } else { out }
        }
    }

    pub fn normalize(path: &str) -> String {
        dec(&normalize_u16(&enc(path)))
    }

    fn normalize_u16(path: &[u16]) -> Vec<u16> {
        let len = path.len();
        if len == 0 {
            return vec![CHAR_DOT];
        }
        let mut root_end: usize = 0;
        let mut device: Option<Vec<u16>> = None;
        let mut is_absolute = false;
        let code = path[0];

        // Try to match a root
        if len == 1 {
            // `path` contains just a single char, exit early to avoid
            // unnecessary work
            return if is_posix_path_separator(code) {
                vec![CHAR_BACKWARD_SLASH]
            } else {
                path.to_vec()
            };
        }
        if is_path_separator(code) {
            // Possible UNC root

            // If we started with a separator, we know we at least have an absolute
            // path of some kind (UNC or otherwise)
            is_absolute = true;

            if is_path_separator(path[1]) {
                // Matched double path separator at beginning
                let mut j = 2;
                let mut last = j;
                // Match 1 or more non-path separators
                while j < len && !is_path_separator(path[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    let first_part = path[last..j].to_vec();
                    // Matched!
                    last = j;
                    // Match 1 or more path separators
                    while j < len && is_path_separator(path[j]) {
                        j += 1;
                    }
                    if j < len && j != last {
                        // Matched!
                        last = j;
                        // Match 1 or more non-path separators
                        while j < len && !is_path_separator(path[j]) {
                            j += 1;
                        }
                        if j == len || j != last {
                            if first_part == [CHAR_DOT] || first_part == [CHAR_QUESTION_MARK] {
                                // We matched a device root (e.g. \\\\.\\PHYSICALDRIVE0)
                                device = Some(concat(&[&[CHAR_BACKWARD_SLASH, CHAR_BACKWARD_SLASH], &first_part]));
                                root_end = 4;
                                let colon_index = index_of(path, CHAR_COLON, 0);
                                // Special case: handle \\?\COM1: or similar reserved device paths
                                let possible_device = js_slice(path, 4, Some(colon_index + 1)).to_vec();
                                if is_windows_reserved_name(&possible_device, possible_device.len() as isize - 1) {
                                    device = Some(concat(&[enc("\\\\?\\").as_slice(), &possible_device]));
                                    root_end = 4 + possible_device.len();
                                }
                            } else if j == len {
                                // We matched a UNC root only
                                // Return the normalized version of the UNC root since there
                                // is nothing left to process
                                return concat(&[
                                    &[CHAR_BACKWARD_SLASH, CHAR_BACKWARD_SLASH],
                                    &first_part,
                                    &[CHAR_BACKWARD_SLASH],
                                    &path[last..],
                                    &[CHAR_BACKWARD_SLASH],
                                ]);
                            } else {
                                // We matched a UNC root with leftovers
                                device = Some(concat(&[
                                    &[CHAR_BACKWARD_SLASH, CHAR_BACKWARD_SLASH],
                                    &first_part,
                                    &[CHAR_BACKWARD_SLASH],
                                    &path[last..j],
                                ]));
                                root_end = j;
                            }
                        }
                    }
                }
            } else {
                root_end = 1;
            }
        } else {
            let colon_index = index_of(path, CHAR_COLON, 0);
            if colon_index > 0 {
                if is_windows_device_root(code) && colon_index == 1 {
                    device = Some(path[0..2].to_vec());
                    root_end = 2;
                    if len > 2 && is_path_separator(path[2]) {
                        is_absolute = true;
                        root_end = 3;
                    }
                } else if is_windows_reserved_name(path, colon_index) {
                    device = Some(path[0..(colon_index as usize + 1)].to_vec());
                    root_end = colon_index as usize + 1;
                }
            }
        }

        let mut tail = if root_end < len {
            normalize_string(&path[root_end..], !is_absolute, CHAR_BACKWARD_SLASH, is_path_separator)
        } else {
            Vec::new()
        };
        if tail.is_empty() && !is_absolute {
            tail = vec![CHAR_DOT];
        }
        if !tail.is_empty() && is_path_separator(path[len - 1]) {
            tail.push(CHAR_BACKWARD_SLASH);
        }
        if !is_absolute && device.is_none() && path.contains(&CHAR_COLON) {
            // If the original path was not absolute and if we have not been able to
            // resolve it relative to a particular device, we need to ensure that the
            // `tail` has not become something that Windows might interpret as an
            // absolute path. See CVE-2024-36139.
            if tail.len() >= 2 && is_windows_device_root(tail[0]) && tail[1] == CHAR_COLON {
                return concat(&[&[CHAR_DOT, CHAR_BACKWARD_SLASH], &tail]);
            }
            let mut index = index_of(path, CHAR_COLON, 0);

            loop {
                if index == len as isize - 1 || opt_is_path_separator(char_code_at(path, (index + 1) as usize)) {
                    return concat(&[&[CHAR_DOT, CHAR_BACKWARD_SLASH], &tail]);
                }
                index = index_of(path, CHAR_COLON, (index + 1) as usize);
                if index == -1 {
                    break;
                }
            }
        }
        let colon_index = index_of(path, CHAR_COLON, 0);
        if is_windows_reserved_name(path, colon_index) {
            return concat(&[
                &[CHAR_DOT, CHAR_BACKWARD_SLASH],
                device.as_deref().unwrap_or(&[]),
                &tail,
            ]);
        }
        match device {
            None => {
                if is_absolute {
                    concat(&[&[CHAR_BACKWARD_SLASH], &tail])
                } else {
                    tail
                }
            }
            Some(device) => {
                if is_absolute {
                    concat(&[&device, &[CHAR_BACKWARD_SLASH], &tail])
                } else {
                    concat(&[&device, &tail])
                }
            }
        }
    }

    pub fn is_absolute(path: &str) -> bool {
        let path = enc(path);
        let len = path.len();
        if len == 0 {
            return false;
        }

        let code = path[0];
        is_path_separator(code)
            // Possible device root
            || (len > 2 && is_windows_device_root(code) && path[1] == CHAR_COLON && is_path_separator(path[2]))
    }

    pub fn join(args: &[&str]) -> String {
        if args.is_empty() {
            return ".".to_string();
        }

        let path: Vec<Vec<u16>> = args.iter().filter(|a| !a.is_empty()).map(|a| enc(a)).collect();

        if path.is_empty() {
            return ".".to_string();
        }

        let first_part = &path[0];
        let mut joined: Vec<u16> = Vec::new();
        for (i, p) in path.iter().enumerate() {
            if i > 0 {
                joined.push(CHAR_BACKWARD_SLASH);
            }
            joined.extend_from_slice(p);
        }

        // Make sure that the joined path doesn't start with two slashes, because
        // normalize() will mistake it for a UNC path then.
        //
        // This step is skipped when it is very clear that the user actually
        // intended to point at a UNC path. This is assumed when the first
        // non-empty string arguments starts with exactly two slashes followed by
        // at least one more non-slash character.
        //
        // Note that for normalize() to treat a path as a UNC path it needs to
        // have at least 2 components, so we don't filter for that here.
        // This means that the user can use join to construct UNC paths from
        // a server name and a share name; for example:
        //   path.join('//server', 'share') -> '\\\\server\\share\\')
        let mut needs_replace = true;
        let mut slash_count = 0usize;
        if opt_is_path_separator(char_code_at(first_part, 0)) {
            slash_count += 1;
            let first_len = first_part.len();
            if first_len > 1 && is_path_separator(first_part[1]) {
                slash_count += 1;
                if first_len > 2 {
                    if is_path_separator(first_part[2]) {
                        slash_count += 1;
                    } else {
                        // We matched a UNC path in the first part
                        needs_replace = false;
                    }
                }
            }
        }
        if needs_replace {
            // Find any more consecutive slashes we need to replace
            while slash_count < joined.len() && is_path_separator(joined[slash_count]) {
                slash_count += 1;
            }

            // Replace the slashes if needed
            if slash_count >= 2 {
                joined = concat(&[&[CHAR_BACKWARD_SLASH], &joined[slash_count..]]);
            }
        }

        // Skip normalization when reserved device names are present
        let mut parts: Vec<Vec<u16>> = Vec::new();
        let mut part: Vec<u16> = Vec::new();

        let mut i = 0usize;
        while i < joined.len() {
            if joined[i] == CHAR_BACKWARD_SLASH {
                if !part.is_empty() {
                    parts.push(std::mem::take(&mut part));
                }
                part.clear();
                // Skip consecutive backslashes
                while i + 1 < joined.len() && joined[i + 1] == CHAR_BACKWARD_SLASH {
                    i += 1;
                }
            } else {
                part.push(joined[i]);
            }
            i += 1;
        }
        // Add the final part if any
        if !part.is_empty() {
            parts.push(part);
        }

        // Check if any part has a Windows reserved name
        if parts.iter().any(|p| {
            let colon_index = index_of(p, CHAR_COLON, 0);
            colon_index != -1 && is_windows_reserved_name(p, colon_index)
        }) {
            // Replace forward slashes with backslashes
            let result: Vec<u16> = joined
                .iter()
                .map(|c| {
                    if *c == CHAR_FORWARD_SLASH {
                        CHAR_BACKWARD_SLASH
                    } else {
                        *c
                    }
                })
                .collect();
            return dec(&result);
        }

        dec(&normalize_u16(&joined))
    }

    /// It will solve the relative path from `from` to `to`, for instance
    /// from = 'C:\\orandea\\test\\aaa'
    /// to = 'C:\\orandea\\impl\\bbb'
    /// The output of the function should be: '..\\..\\impl\\bbb'
    pub fn relative(from: &str, to: &str) -> String {
        if from == to {
            return String::new();
        }

        let from_orig = resolve_u16(&[from]);
        let to_orig = resolve_u16(&[to]);

        if from_orig == to_orig {
            return String::new();
        }

        let from = to_lower(&from_orig);
        let to = to_lower(&to_orig);

        if from == to {
            return String::new();
        }

        if from_orig.len() != from.len() || to_orig.len() != to.len() {
            let mut from_split: Vec<&[u16]> = from_orig.split(|c| *c == CHAR_BACKWARD_SLASH).collect();
            let mut to_split: Vec<&[u16]> = to_orig.split(|c| *c == CHAR_BACKWARD_SLASH).collect();
            if from_split.last().is_some_and(|s| s.is_empty()) {
                from_split.pop();
            }
            if to_split.last().is_some_and(|s| s.is_empty()) {
                to_split.pop();
            }

            let from_len = from_split.len();
            let to_len = to_split.len();
            let length = if from_len < to_len { from_len } else { to_len };

            let mut i = 0usize;
            while i < length {
                if to_lower(from_split[i]) != to_lower(to_split[i]) {
                    break;
                }
                i += 1;
            }

            let join_back = |parts: &[&[u16]]| -> Vec<u16> {
                let mut out = Vec::new();
                for (k, p) in parts.iter().enumerate() {
                    if k > 0 {
                        out.push(CHAR_BACKWARD_SLASH);
                    }
                    out.extend_from_slice(p);
                }
                out
            };

            if i == 0 {
                return dec(&to_orig);
            } else if i == length {
                if to_len > length {
                    return dec(&join_back(&to_split[i..]));
                }
                if from_len > length {
                    return format!("{}..", "..\\".repeat(from_len - 1 - i));
                }
                return String::new();
            }

            return format!("{}{}", "..\\".repeat(from_len - i), dec(&join_back(&to_split[i..])));
        }

        // Trim any leading backslashes
        let mut from_start = 0usize;
        while from_start < from.len() && from[from_start] == CHAR_BACKWARD_SLASH {
            from_start += 1;
        }
        // Trim trailing backslashes (applicable to UNC paths only)
        let mut from_end = from.len();
        while from_end as isize - 1 > from_start as isize && from[from_end - 1] == CHAR_BACKWARD_SLASH {
            from_end -= 1;
        }
        let from_len = from_end as isize - from_start as isize;

        // Trim any leading backslashes
        let mut to_start = 0usize;
        while to_start < to.len() && to[to_start] == CHAR_BACKWARD_SLASH {
            to_start += 1;
        }
        // Trim trailing backslashes (applicable to UNC paths only)
        let mut to_end = to.len();
        while to_end as isize - 1 > to_start as isize && to[to_end - 1] == CHAR_BACKWARD_SLASH {
            to_end -= 1;
        }
        let to_len = to_end as isize - to_start as isize;

        // Compare paths to find the longest common path from root
        let length = if from_len < to_len { from_len } else { to_len };
        let mut last_common_sep: isize = -1;
        let mut i: isize = 0;
        while i < length {
            let from_code = char_code_at(&from, (from_start as isize + i) as usize);
            if from_code != char_code_at(&to, (to_start as isize + i) as usize) {
                break;
            } else if from_code == Some(CHAR_BACKWARD_SLASH) {
                last_common_sep = i;
            }
            i += 1;
        }

        // We found a mismatch before the first common path separator was seen, so
        // return the original `to`.
        if i != length {
            if last_common_sep == -1 {
                return dec(&to_orig);
            }
        } else {
            if to_len > length {
                if char_code_at(&to, (to_start as isize + i) as usize) == Some(CHAR_BACKWARD_SLASH) {
                    // We get here if `from` is the exact base path for `to`.
                    // For example: from='C:\\foo\\bar'; to='C:\\foo\\bar\\baz'
                    return dec(js_slice(&to_orig, to_start as isize + i + 1, None));
                }
                if i == 2 {
                    // We get here if `from` is the device root.
                    // For example: from='C:\\'; to='C:\\foo'
                    return dec(js_slice(&to_orig, to_start as isize + i, None));
                }
            }
            if from_len > length {
                if char_code_at(&from, (from_start as isize + i) as usize) == Some(CHAR_BACKWARD_SLASH) {
                    // We get here if `to` is the exact base path for `from`.
                    // For example: from='C:\\foo\\bar'; to='C:\\foo'
                    last_common_sep = i;
                } else if i == 2 {
                    // We get here if `to` is the device root.
                    // For example: from='C:\\foo\\bar'; to='C:\\'
                    last_common_sep = 3;
                }
            }
            if last_common_sep == -1 {
                last_common_sep = 0;
            }
        }

        let mut out: Vec<u16> = Vec::new();
        // Generate the relative path based on the path difference between `to` and
        // `from`
        let mut i = from_start as isize + last_common_sep + 1;
        while i <= from_end as isize {
            if i == from_end as isize || char_code_at(&from, i as usize) == Some(CHAR_BACKWARD_SLASH) {
                if !out.is_empty() {
                    out.push(CHAR_BACKWARD_SLASH);
                }
                out.push(CHAR_DOT);
                out.push(CHAR_DOT);
            }
            i += 1;
        }

        let mut to_start = to_start as isize + last_common_sep;

        // Lastly, append the rest of the destination (`to`) path that comes after
        // the common path parts
        if !out.is_empty() {
            return dec(&concat(&[&out, js_slice(&to_orig, to_start, Some(to_end as isize))]));
        }

        if char_code_at(&to_orig, to_start.max(0) as usize) == Some(CHAR_BACKWARD_SLASH) {
            to_start += 1;
        }
        dec(js_slice(&to_orig, to_start, Some(to_end as isize)))
    }

    pub fn to_namespaced_path(path: &str) -> String {
        // Note: this will *probably* throw somewhere.
        if path.is_empty() {
            return path.to_string();
        }

        let resolved_path = resolve_u16(&[path]);

        if resolved_path.len() <= 2 {
            return path.to_string();
        }

        if resolved_path[0] == CHAR_BACKWARD_SLASH {
            // Possible UNC root
            if resolved_path[1] == CHAR_BACKWARD_SLASH {
                let code = resolved_path[2];
                if code != CHAR_QUESTION_MARK && code != CHAR_DOT {
                    // Matched non-long UNC root, convert the path to a long UNC path
                    return format!("\\\\?\\UNC\\{}", dec(&resolved_path[2..]));
                }
            }
        } else if is_windows_device_root(resolved_path[0])
            && resolved_path[1] == CHAR_COLON
            && resolved_path[2] == CHAR_BACKWARD_SLASH
        {
            // Matched device root, convert the path to a long UNC path
            return format!("\\\\?\\{}", dec(&resolved_path));
        }

        dec(&resolved_path)
    }

    pub fn dirname(path: &str) -> String {
        let path = enc(path);
        let len = path.len();
        if len == 0 {
            return ".".to_string();
        }
        let mut root_end: isize = -1;
        let mut offset: usize = 0;
        let code = path[0];

        if len == 1 {
            // `path` contains just a path separator, exit early to avoid
            // unnecessary work or a dot.
            return if is_path_separator(code) {
                dec(&path)
            } else {
                ".".to_string()
            };
        }

        // Try to match a root
        if is_path_separator(code) {
            // Possible UNC root

            root_end = 1;
            offset = 1;

            if is_path_separator(path[1]) {
                // Matched double path separator at beginning
                let mut j = 2;
                let mut last = j;
                // Match 1 or more non-path separators
                while j < len && !is_path_separator(path[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    // Matched!
                    last = j;
                    // Match 1 or more path separators
                    while j < len && is_path_separator(path[j]) {
                        j += 1;
                    }
                    if j < len && j != last {
                        // Matched!
                        last = j;
                        // Match 1 or more non-path separators
                        while j < len && !is_path_separator(path[j]) {
                            j += 1;
                        }
                        if j == len {
                            // We matched a UNC root only
                            return dec(&path);
                        }
                        if j != last {
                            // We matched a UNC root with leftovers

                            // Offset by 1 to include the separator after the UNC root to
                            // treat it as a "normal root" on top of a (UNC) root
                            root_end = (j + 1) as isize;
                            offset = j + 1;
                        }
                    }
                }
            }
            // Possible device root
        } else if is_windows_device_root(code) && path[1] == CHAR_COLON {
            root_end = if len > 2 && is_path_separator(path[2]) { 3 } else { 2 };
            offset = root_end as usize;
        }

        let mut end: isize = -1;
        let mut matched_slash = true;
        let mut i = len as isize - 1;
        while i >= offset as isize {
            if is_path_separator(path[i as usize]) {
                if !matched_slash {
                    end = i;
                    break;
                }
            } else {
                // We saw the first non-path separator
                matched_slash = false;
            }
            i -= 1;
        }

        if end == -1 {
            if root_end == -1 {
                return ".".to_string();
            }

            end = root_end;
        }
        dec(js_slice(&path, 0, Some(end)))
    }

    pub fn basename(path: &str, suffix: Option<&str>) -> String {
        let path = enc(path);
        let suffix = suffix.map(enc);
        // Check for a drive letter prefix so as not to mistake the following
        // path separator as an extra separator at the end of the path that can be
        // disregarded
        let start = if path.len() >= 2 && is_windows_device_root(path[0]) && path[1] == CHAR_COLON {
            2
        } else {
            0
        };
        dec(&basename_impl(&path, suffix.as_deref(), start, is_path_separator))
    }

    pub fn extname(path: &str) -> String {
        let path = enc(path);
        // Check for a drive letter prefix so as not to mistake the following
        // path separator as an extra separator at the end of the path that can be
        // disregarded
        let start = if path.len() >= 2 && path[1] == CHAR_COLON && is_windows_device_root(path[0]) {
            2
        } else {
            0
        };
        dec(&extname_impl(&path, start, is_path_separator))
    }

    pub fn format(path_object: &ParsedPath) -> String {
        format_impl("\\", path_object)
    }

    pub fn parse(path: &str) -> ParsedPath {
        let path = enc(path);
        let mut ret = ParsedPath::default();
        if path.is_empty() {
            return ret;
        }

        let len = path.len();
        let mut root_end: usize = 0;
        let mut code = path[0];

        if len == 1 {
            if is_path_separator(code) {
                // `path` contains just a path separator, exit early to avoid
                // unnecessary work
                ret.root = dec(&path);
                ret.dir = ret.root.clone();
                return ret;
            }
            ret.base = dec(&path);
            ret.name = ret.base.clone();
            return ret;
        }
        // Try to match a root
        if is_path_separator(code) {
            // Possible UNC root

            root_end = 1;
            if is_path_separator(path[1]) {
                // Matched double path separator at beginning
                let mut j = 2;
                let mut last = j;
                // Match 1 or more non-path separators
                while j < len && !is_path_separator(path[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    // Matched!
                    last = j;
                    // Match 1 or more path separators
                    while j < len && is_path_separator(path[j]) {
                        j += 1;
                    }
                    if j < len && j != last {
                        // Matched!
                        last = j;
                        // Match 1 or more non-path separators
                        while j < len && !is_path_separator(path[j]) {
                            j += 1;
                        }
                        if j == len {
                            // We matched a UNC root only
                            root_end = j;
                        } else if j != last {
                            // We matched a UNC root with leftovers
                            root_end = j + 1;
                        }
                    }
                }
            }
        } else if is_windows_device_root(code) && path[1] == CHAR_COLON {
            // Possible device root
            if len <= 2 {
                // `path` contains just a drive root, exit early to avoid
                // unnecessary work
                ret.root = dec(&path);
                ret.dir = ret.root.clone();
                return ret;
            }
            root_end = 2;
            if is_path_separator(path[2]) {
                if len == 3 {
                    // `path` contains just a drive root, exit early to avoid
                    // unnecessary work
                    ret.root = dec(&path);
                    ret.dir = ret.root.clone();
                    return ret;
                }
                root_end = 3;
            }
        }
        if root_end > 0 {
            ret.root = dec(&path[0..root_end]);
        }

        let mut start_dot: isize = -1;
        let mut start_part: isize = root_end as isize;
        let mut end: isize = -1;
        let mut matched_slash = true;
        let mut i = path.len() as isize - 1;

        // Track the state of characters (if any) we see before our first dot and
        // after any path separator we find
        let mut pre_dot_state = 0;

        // Get non-dir info
        while i >= root_end as isize {
            code = path[i as usize];
            if is_path_separator(code) {
                // If we reached a path separator that was not part of a set of path
                // separators at the end of the string, stop now
                if !matched_slash {
                    start_part = i + 1;
                    break;
                }
                i -= 1;
                continue;
            }
            if end == -1 {
                // We saw the first non-path separator, mark this as the end of our
                // extension
                matched_slash = false;
                end = i + 1;
            }
            if code == CHAR_DOT {
                // If this is our first dot, mark it as the start of our extension
                if start_dot == -1 {
                    start_dot = i;
                } else if pre_dot_state != 1 {
                    pre_dot_state = 1;
                }
            } else if start_dot != -1 {
                // We saw a non-dot and non-path separator before our dot, so we should
                // have a good chance at having a non-empty extension
                pre_dot_state = -1;
            }
            i -= 1;
        }

        if end != -1 {
            if start_dot == -1
                // We saw a non-dot character immediately before the dot
                || pre_dot_state == 0
                // The (right-most) trimmed path component is exactly '..'
                || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part + 1)
            {
                ret.base = dec(js_slice(&path, start_part, Some(end)));
                ret.name = ret.base.clone();
            } else {
                ret.name = dec(js_slice(&path, start_part, Some(start_dot)));
                ret.base = dec(js_slice(&path, start_part, Some(end)));
                ret.ext = dec(js_slice(&path, start_dot, Some(end)));
            }
        }

        // If the directory is the root, use the entire root as the `dir` including
        // the trailing slash if any (`C:\abc` -> `C:\`). Otherwise, strip out the
        // trailing slash (`C:\abc\def` -> `C:\abc`).
        if start_part > 0 && start_part != root_end as isize {
            ret.dir = dec(js_slice(&path, 0, Some(start_part - 1)));
        } else {
            ret.dir = ret.root.clone();
        }

        ret
    }
}

#[cfg(not(windows))]
use posix as platform;
#[cfg(windows)]
use win32 as platform;

/// `path.join(...parts)` (platform default).
pub fn join(parts: &[&str]) -> String {
    platform::join(parts)
}

/// `path.resolve(...parts)` (platform default).
pub fn resolve(parts: &[&str]) -> String {
    platform::resolve(parts)
}

/// `path.normalize(p)` (platform default).
pub fn normalize(p: &str) -> String {
    platform::normalize(p)
}

/// `path.relative(from, to)` (platform default).
pub fn relative(from: &str, to: &str) -> String {
    platform::relative(from, to)
}

/// `path.dirname(p)` (platform default).
pub fn dirname(p: &str) -> String {
    platform::dirname(p)
}

/// `path.basename(p, ext)` (platform default).
pub fn basename(p: &str, ext: Option<&str>) -> String {
    platform::basename(p, ext)
}

/// `path.extname(p)` (platform default).
pub fn extname(p: &str) -> String {
    platform::extname(p)
}

/// `path.isAbsolute(p)` (platform default).
pub fn is_absolute(p: &str) -> bool {
    platform::is_absolute(p)
}

/// `path.sep` (platform default).
pub fn sep() -> &'static str {
    platform::sep()
}

/// `path.delimiter` (platform default).
pub fn delimiter() -> &'static str {
    platform::delimiter()
}

/// `path.parse(p)` (platform default).
pub fn parse(p: &str) -> ParsedPath {
    platform::parse(p)
}

/// `path.format(obj)` (platform default).
pub fn format(path_object: &ParsedPath) -> String {
    platform::format(path_object)
}

/// `path.toNamespacedPath(p)` (platform default).
pub fn to_namespaced_path(p: &str) -> String {
    platform::to_namespaced_path(p)
}

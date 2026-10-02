use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiCode {
    pub code: String,
    pub length: usize,
}

pub fn extract_ansi_code(text: &str, pos: usize) -> Option<AnsiCode> {
    if pos >= text.len() || !text[pos..].starts_with('\u{1b}') {
        return None;
    }
    let bytes = text.as_bytes();
    let next = *bytes.get(pos + 1)? as char;
    match next {
        '[' => {
            let mut j = pos + 2;
            while j < text.len() {
                let ch = bytes[j] as char;
                if matches!(ch, 'm' | 'G' | 'K' | 'H' | 'J')
                    || (('@'..='~').contains(&ch)
                        && !ch.is_ascii_digit()
                        && ch != ';'
                        && ch != ':'
                        && ch != '<'
                        && ch != '?'
                        && ch != '>')
                {
                    return Some(AnsiCode {
                        code: text[pos..=j].to_string(),
                        length: j + 1 - pos,
                    });
                }
                j += 1;
            }
            None
        }
        ']' | '_' | 'P' => {
            let mut j = pos + 2;
            while j < text.len() {
                if bytes[j] == b'\x07' {
                    return Some(AnsiCode {
                        code: text[pos..=j].to_string(),
                        length: j + 1 - pos,
                    });
                }
                if bytes[j] == b'\x1b' && bytes.get(j + 1) == Some(&b'\\') {
                    return Some(AnsiCode {
                        code: text[pos..j + 2].to_string(),
                        length: j + 2 - pos,
                    });
                }
                j += 1;
            }
            None
        }
        _ => None,
    }
}

pub fn visible_width(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let mut clean = String::new();
    let mut i = 0;
    while i < text.len() {
        if let Some(ansi) = extract_ansi_code(text, i) {
            i += ansi.length;
            continue;
        }
        let ch = text[i..].chars().next().expect("valid char boundary");
        if ch == '\t' {
            clean.push_str("   ");
        } else {
            clean.push(ch);
        }
        i += ch.len_utf8();
    }
    UnicodeWidthStr::width(clean.as_str())
}

pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    truncate_to_width_with(text, max_width, "...", false)
}

pub fn truncate_to_width_with(text: &str, max_width: usize, ellipsis: &str, pad: bool) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text.is_empty() {
        return if pad {
            " ".repeat(max_width)
        } else {
            String::new()
        };
    }
    let text_width = visible_width(text);
    if text_width <= max_width {
        let mut result = text.to_string();
        if pad {
            result.push_str(&" ".repeat(max_width - text_width));
        }
        return result;
    }

    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= max_width {
        let (clipped, clipped_width) = truncate_fragment_to_width(ellipsis, max_width);
        if clipped_width == 0 {
            return if pad {
                " ".repeat(max_width)
            } else {
                String::new()
            };
        }
        return finalize_truncated_result("", 0, &clipped, clipped_width, max_width, pad);
    }

    let target_width = max_width.saturating_sub(ellipsis_width);
    let (out, width) = truncate_fragment_to_width(text, target_width);
    finalize_truncated_result(&out, width, ellipsis, ellipsis_width, max_width, pad)
}

fn truncate_fragment_to_width(text: &str, max_width: usize) -> (String, usize) {
    let mut out = String::new();
    let mut width = 0;
    let mut i = 0;
    while i < text.len() {
        if let Some(ansi) = extract_ansi_code(text, i) {
            out.push_str(&ansi.code);
            i += ansi.length;
            continue;
        }
        let ch = text[i..].chars().next().expect("valid char boundary");
        let ch_width = if ch == '\t' {
            3
        } else {
            UnicodeWidthChar::width(ch).unwrap_or(0)
        };
        if width + ch_width > max_width {
            break;
        }
        out.push(ch);
        width += ch_width;
        i += ch.len_utf8();
    }
    (out, width)
}

fn finalize_truncated_result(
    prefix: &str,
    prefix_width: usize,
    ellipsis: &str,
    ellipsis_width: usize,
    max_width: usize,
    pad: bool,
) -> String {
    let mut out = String::from(prefix);
    out.push_str("\u{1b}[0m");
    if !ellipsis.is_empty() {
        out.push_str(ellipsis);
        out.push_str("\u{1b}[0m");
    }
    if pad {
        out.push_str(&" ".repeat(max_width.saturating_sub(prefix_width + ellipsis_width)));
    }
    out
}

pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    wrap_text_with_ansi(text, width)
}

pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    if width == 0 {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    for source_line in text.split('\n') {
        if source_line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_width = 0;
        for token in split_words_preserve_spaces(source_line) {
            let token_width = visible_width(&token);
            let is_space = token.trim().is_empty();
            if current_width > 0 && current_width + token_width > width {
                lines.push(current.trim_end().to_string());
                current = String::new();
                current_width = 0;
                if is_space {
                    continue;
                }
            }
            if token_width > width && !is_space {
                if !current.is_empty() {
                    lines.push(current.trim_end().to_string());
                    current = String::new();
                }
                let mut rest = token.as_str();
                while visible_width(rest) > width {
                    let part = truncate_to_width_with(rest, width, "", false);
                    let consumed = part.len();
                    lines.push(part);
                    rest = &rest[consumed..];
                }
                current.push_str(rest);
                current_width = visible_width(&current);
            } else {
                current.push_str(&token);
                current_width += token_width;
            }
        }
        if !current.is_empty() {
            lines.push(current.trim_end().to_string());
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn split_words_preserve_spaces(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut current_space: Option<bool> = None;
    let mut i = 0;
    while i < text.len() {
        if let Some(ansi) = extract_ansi_code(text, i) {
            current.push_str(&ansi.code);
            i += ansi.length;
            continue;
        }
        let ch = text[i..].chars().next().expect("valid char boundary");
        let is_space = ch == ' ';
        if current_space.is_some_and(|space| space != is_space) && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
        current_space = Some(is_space);
        current.push(ch);
        i += ch.len_utf8();
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

pub fn apply_background_to_line(
    line: &str,
    width: usize,
    bg_fn: impl Fn(&str) -> String,
) -> String {
    let mut padded = line.to_string();
    padded.push_str(&" ".repeat(width.saturating_sub(visible_width(line))));
    bg_fn(&padded)
}

pub fn slice_by_column(line: &str, start_col: usize, length: usize, strict: bool) -> String {
    slice_with_width(line, start_col, length, strict).0
}

pub fn slice_with_width(
    line: &str,
    start_col: usize,
    length: usize,
    strict: bool,
) -> (String, usize) {
    if length == 0 {
        return (String::new(), 0);
    }
    let end_col = start_col + length;
    let mut result = String::new();
    let mut result_width = 0;
    let mut current_col = 0;
    let mut pending_ansi = String::new();
    let mut i = 0;
    while i < line.len() {
        if let Some(ansi) = extract_ansi_code(line, i) {
            if current_col >= start_col && current_col < end_col {
                result.push_str(&ansi.code);
            } else if current_col < start_col {
                pending_ansi.push_str(&ansi.code);
            }
            i += ansi.length;
            continue;
        }
        let ch = line[i..].chars().next().expect("valid char boundary");
        let w = if ch == '\t' {
            3
        } else {
            UnicodeWidthChar::width(ch).unwrap_or(0)
        };
        let in_range = current_col >= start_col && current_col < end_col;
        let fits = !strict || current_col + w <= end_col;
        if in_range && fits {
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push(ch);
            result_width += w;
        }
        current_col += w;
        if current_col >= end_col {
            break;
        }
        i += ch.len_utf8();
    }
    (result, result_width)
}

pub fn normalize_terminal_output(text: &str) -> String {
    text.replace('\u{0e33}', "\u{0e4d}\u{0e32}")
        .replace('\u{0eb3}', "\u{0ecd}\u{0eb2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_by_display_width() {
        assert_eq!(truncate_to_width("abcdef", 3), "\u{1b}[0m...\u{1b}[0m");
        assert_eq!(visible_width("コン"), 4);
    }

    #[test]
    fn truncate_matches_ts_edge_cases() {
        assert_eq!(truncate_to_width_with("abcdef", 1, "🙂", false), "");
        assert_eq!(
            truncate_to_width_with("abcdef", 2, "🙂", false),
            "\u{1b}[0m🙂\u{1b}[0m"
        );
        let no_ellipsis =
            truncate_to_width_with(&format!("\u{1b}[31m{}", "hello".repeat(100)), 10, "", false);
        assert!(no_ellipsis.ends_with("\u{1b}[0m"));
        assert!(visible_width(&no_ellipsis) <= 10);
        assert_eq!(
            truncate_to_width_with("🙂\t界 \u{1b}_abc\u{7}", 7, "…", true),
            "🙂\t\u{1b}[0m…\u{1b}[0m "
        );
    }

    #[test]
    fn strips_osc_and_apc() {
        assert_eq!(visible_width("a\u{1b}]8;;url\u{7}b\u{1b}]8;;\u{7}"), 2);
        assert_eq!(visible_width(crate::tui::CURSOR_MARKER), 0);
    }
}

//! Port of packages/coding-agent/src/core/tools/edit-diff.ts (`7fbbd5f4a` + `#8544` fixes)

use pi_js::vendor::jsdiff::diff::line::diff_lines;
use pi_js::vendor::jsdiff::patch::create::{CreatePatchOptions, FILE_HEADERS_ONLY, create_two_files_patch};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuzzyMatchResult {
    pub found: bool,
    /// UTF-8 byte index in `content_for_replacement`
    pub index: usize,
    /// UTF-8 byte length of matched text in `content_for_replacement`
    pub match_length: usize,
    pub used_fuzzy_match: bool,
    pub content_for_replacement: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextReplacement {
    /// UTF-8 byte start index in `normalize_to_lf(original)`
    pub match_index: usize,
    /// UTF-8 byte length in `normalize_to_lf(original)`
    pub match_length: usize,
    pub new_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedEdits {
    pub new_content: String,
    pub replacements: Vec<TextReplacement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffResult {
    pub diff: String,
    pub first_changed_line: Option<usize>,
}

/// Detect dominant/first line ending in `content` (`"\r\n"` vs `"\n"`).
pub fn detect_line_ending(content: &str) -> &'static str {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    match (crlf_idx, lf_idx) {
        (None, _) => "\n",
        (Some(_), None) => "\r\n",
        (Some(crlf), Some(lf)) => {
            if crlf < lf {
                "\r\n"
            } else {
                "\n"
            }
        }
    }
}

/// Normalize CRLF and bare CR to LF.
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Restore LF line endings to `ending` (`"\r\n"`, `"\r"`, or `"\n"`).
pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else if ending == "\r" {
        text.replace('\n', "\r")
    } else {
        text.to_string()
    }
}

/// Normalize text for fuzzy matching:
/// - trim trailing whitespace per line
/// - smart single/double quotes -> ASCII `'` / `"`
/// - Unicode dashes/minus -> ASCII `-`
/// - special Unicode spaces -> ASCII space `' '`
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            let trimmed = line.trim_end();
            let nfkc = pi_js::str16::normalize_nfkc(trimmed);
            nfkc.chars()
                .map(|c| match c {
                    '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
                    '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
                    '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => '-',
                    '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
                    other => other,
                })
                .collect()
        })
        .collect();
    lines.join("\n")
}

/// Find `old_text` in `content`, trying exact match first, then fuzzy match.
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(idx) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index: idx,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    }

    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);

    if let Some(fuzzy_index) = fuzzy_content.find(&fuzzy_old_text) {
        return FuzzyMatchResult {
            found: true,
            index: fuzzy_index,
            match_length: fuzzy_old_text.len(),
            used_fuzzy_match: true,
            content_for_replacement: fuzzy_content,
        };
    }

    FuzzyMatchResult {
        found: false,
        index: 0,
        match_length: 0,
        used_fuzzy_match: false,
        content_for_replacement: content.to_string(),
    }
}

fn count_non_overlapping_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.match_indices(needle).count()
}

fn line_index_at_byte(text: &str, byte_idx: usize) -> usize {
    let clamped = byte_idx.min(text.len());
    text.as_bytes()[..clamped].iter().filter(|&&b| b == b'\n').count()
}

fn line_start_offsets(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (idx, b) in text.as_bytes().iter().enumerate() {
        if *b == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

/// Widen a fuzzy match inside `fuzzy_content` onto full line boundaries of `normalized_content`
/// so untouched lines keep their original trailing whitespace, quotes, and characters.
fn widen_fuzzy_match_to_normalized_lines(
    normalized_content: &str,
    fuzzy_content: &str,
    fuzzy_index: usize,
    fuzzy_length: usize,
    normalized_new_text: &str,
) -> TextReplacement {
    let orig_starts = line_start_offsets(normalized_content);
    let orig_lines: Vec<&str> = normalized_content.split('\n').collect();
    let fuzzy_starts = line_start_offsets(fuzzy_content);
    let fuzzy_lines: Vec<&str> = fuzzy_content.split('\n').collect();

    if orig_starts.len() != fuzzy_starts.len() {
        return TextReplacement {
            match_index: fuzzy_index.min(normalized_content.len()),
            match_length: fuzzy_length.min(normalized_content.len().saturating_sub(fuzzy_index)),
            new_text: normalized_new_text.to_string(),
        };
    }

    let start_line = line_index_at_byte(fuzzy_content, fuzzy_index);
    let match_end = (fuzzy_index + fuzzy_length).min(fuzzy_content.len());
    let ends_with_nl = fuzzy_length > 0 && fuzzy_content.as_bytes().get(match_end - 1) == Some(&b'\n');
    let end_probe = if ends_with_nl { match_end - 1 } else { match_end };
    let end_line = line_index_at_byte(fuzzy_content, end_probe).max(start_line);

    let fuzzy_span_start = fuzzy_starts[start_line];
    let fuzzy_span_end = fuzzy_starts[end_line] + fuzzy_lines[end_line].len();
    let prefix = &fuzzy_content[fuzzy_span_start..fuzzy_index];
    let (suffix, trailing_nl) = if ends_with_nl {
        ("", "\n")
    } else {
        (&fuzzy_content[match_end.min(fuzzy_span_end)..fuzzy_span_end], "")
    };

    let orig_span_start = orig_starts[start_line];
    let orig_span_end =
        (orig_starts[end_line] + orig_lines[end_line].len() + trailing_nl.len()).min(normalized_content.len());

    TextReplacement {
        match_index: orig_span_start,
        match_length: orig_span_end.saturating_sub(orig_span_start),
        new_text: format!("{prefix}{normalized_new_text}{suffix}"),
    }
}

/// Apply all `(old_text, new_text)` edits against `normalized_content` (LF), validating uniqueness,
/// non-overlapping regions, and non-empty net diff.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[(&str, &str)],
    file_path: &str,
) -> Result<AppliedEdits, pi_js::Error> {
    if edits.is_empty() {
        return Err(pi_js::Error::msg(
            "Invalid edits: edits array must contain at least one replacement.",
        ));
    }

    let mut matched_replacements: Vec<TextReplacement> = Vec::with_capacity(edits.len());

    for &(raw_old_text, raw_new_text) in edits {
        let normalized_old = normalize_to_lf(raw_old_text);
        let normalized_new = normalize_to_lf(raw_new_text);

        if normalized_old.is_empty() {
            return Err(pi_js::Error::msg(format!(
                "Could not find text to replace in {file_path} (oldText must not be empty)."
            )));
        }

        let match_res = fuzzy_find_text(normalized_content, &normalized_old);
        if !match_res.found {
            return Err(pi_js::Error::msg(format!(
                "Could not find the exact text in {file_path}. The old text must match exactly including all whitespace and newlines."
            )));
        }

        let occurrences = if match_res.used_fuzzy_match {
            let fuzzy_old = normalize_for_fuzzy_match(&normalized_old);
            count_non_overlapping_occurrences(&match_res.content_for_replacement, &fuzzy_old)
        } else {
            count_non_overlapping_occurrences(normalized_content, &normalized_old)
        };

        if occurrences > 1 {
            return Err(pi_js::Error::msg(format!(
                "Found {occurrences} occurrences of the text in {file_path}. The text to be replaced must be unique. Please provide more context to make it unique."
            )));
        }

        if match_res.used_fuzzy_match {
            matched_replacements.push(widen_fuzzy_match_to_normalized_lines(
                normalized_content,
                &match_res.content_for_replacement,
                match_res.index,
                match_res.match_length,
                &normalized_new,
            ));
        } else {
            matched_replacements.push(TextReplacement {
                match_index: match_res.index,
                match_length: match_res.match_length,
                new_text: normalized_new,
            });
        }
    }

    matched_replacements.sort_by_key(|r| r.match_index);

    for pair in matched_replacements.windows(2) {
        let prev = &pair[0];
        let curr = &pair[1];
        if prev.match_index + prev.match_length > curr.match_index {
            return Err(pi_js::Error::msg(format!(
                "Edits overlap in {file_path}. Combine overlapping edits into a single replacement."
            )));
        }
    }

    let mut new_content = String::with_capacity(normalized_content.len());
    let mut cursor = 0usize;
    for rep in &matched_replacements {
        new_content.push_str(&normalized_content[cursor..rep.match_index]);
        new_content.push_str(&rep.new_text);
        cursor = rep.match_index + rep.match_length;
    }
    new_content.push_str(&normalized_content[cursor..]);

    if new_content == normalized_content {
        return Err(pi_js::Error::msg(format!(
            "No changes made to {file_path}. The replacement produced identical content."
        )));
    }

    Ok(AppliedEdits {
        new_content,
        replacements: matched_replacements,
    })
}

/// Build byte offset table mapping every index `0..=normalized_len` in `normalize_to_lf(raw)` to
/// its UTF-8 byte offset in `raw`.
fn build_normalized_to_raw_offsets(raw: &str) -> Vec<usize> {
    let bytes = raw.as_bytes();
    let mut offsets = Vec::with_capacity(bytes.len() + 1);
    let mut i = 0usize;
    while i < bytes.len() {
        offsets.push(i);
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            i += 2;
        } else {
            i += 1;
        }
    }
    offsets.push(bytes.len());
    offsets
}

fn detect_local_line_ending(raw: &str, raw_start: usize, raw_end: usize) -> &'static str {
    let span = &raw[raw_start..raw_end];
    if span.contains("\r\n") {
        return "\r\n";
    }
    if span.contains('\n') {
        return "\n";
    }

    // Look forward/backward around the splice location before falling back to file-wide detection
    if let Some(after_pos) = raw[raw_end..].find(['\r', '\n']) {
        let idx = raw_end + after_pos;
        if raw.as_bytes().get(idx) == Some(&b'\r') {
            if raw.as_bytes().get(idx + 1) == Some(&b'\n') {
                return "\r\n";
            }
            if !raw.contains('\n') {
                return "\r";
            }
        } else {
            return "\n";
        }
    }

    if let Some(before_pos) = raw[..raw_start].rfind(['\r', '\n']) {
        let bytes = raw.as_bytes();
        if bytes[before_pos] == b'\n' {
            if before_pos > 0 && bytes[before_pos - 1] == b'\r' {
                return "\r\n";
            }
            return "\n";
        } else if !raw.contains('\n') {
            return "\r";
        }
    }

    if raw.contains("\r\n") {
        "\r\n"
    } else if raw.contains('\n') {
        "\n"
    } else if raw.contains('\r') {
        "\r"
    } else {
        "\n"
    }
}

fn push_with_cr_lf_seam(out: &mut String, chunk: &str) {
    if chunk.is_empty() {
        return;
    }
    // Prevent a trailing lone '\r' from merging with a leading '\n' in the following chunk
    if out.ends_with('\r') && !out.ends_with("\r\n") && chunk.starts_with('\n') {
        out.push('\n');
    }
    out.push_str(chunk);
}

/// Apply LF-indexed `replacements` directly onto `raw_original` (`#8544`), preserving line endings
/// on every untouched line and preventing CR+LF seam collapse.
pub fn apply_replacements_to_original(raw_original: &str, replacements: &[TextReplacement]) -> String {
    if replacements.is_empty() {
        return raw_original.to_string();
    }

    let offsets = build_normalized_to_raw_offsets(raw_original);
    let mut sorted: Vec<&TextReplacement> = replacements.iter().collect();
    sorted.sort_by_key(|r| r.match_index);

    let mut out = String::with_capacity(raw_original.len());
    let mut raw_cursor = 0usize;

    for rep in sorted {
        let norm_start = rep.match_index.min(offsets.len().saturating_sub(1));
        let norm_end = (rep.match_index + rep.match_length).min(offsets.len().saturating_sub(1));
        let raw_start = offsets[norm_start];
        let raw_end = offsets[norm_end];

        if raw_start < raw_cursor {
            continue;
        }

        let kept_before = &raw_original[raw_cursor..raw_start];
        push_with_cr_lf_seam(&mut out, kept_before);

        let local_ending = detect_local_line_ending(raw_original, raw_start, raw_end);
        let formatted_new = if rep.new_text.contains('\n') {
            restore_line_endings(&rep.new_text, local_ending)
        } else {
            rep.new_text.clone()
        };
        push_with_cr_lf_seam(&mut out, &formatted_new);

        raw_cursor = raw_end;
    }

    if raw_cursor < raw_original.len() {
        push_with_cr_lf_seam(&mut out, &raw_original[raw_cursor..]);
    }

    out
}

/// Generate a unified-style numbered diff string matching TS `generateDiffString`.
pub fn generate_diff_string(old_content: &str, new_content: &str, context_lines: Option<usize>) -> DiffResult {
    let context = context_lines.unwrap_or(4);
    let parts = diff_lines(old_content, new_content, None);

    let mut output: Vec<String> = Vec::new();
    let old_lines_total = old_content.split('\n').count();
    let new_lines_total = new_content.split('\n').count();
    let max_line_num = old_lines_total.max(new_lines_total).max(1);
    let line_num_width = max_line_num.to_string().len();

    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for idx in 0..parts.len() {
        let part = &parts[idx];
        let mut raw_lines: Vec<&str> = part.value.split('\n').collect();
        if raw_lines.last() == Some(&"") {
            raw_lines.pop();
        }

        if part.added || part.removed {
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num);
            }
            for line in &raw_lines {
                if part.added {
                    output.push(format!("+{new_line_num:>line_num_width$} {line}"));
                    new_line_num += 1;
                } else {
                    output.push(format!("-{old_line_num:>line_num_width$} {line}"));
                    old_line_num += 1;
                }
            }
            last_was_change = true;
        } else {
            let next_part_is_change = parts.get(idx + 1).is_some_and(|next| next.added || next.removed);

            if last_was_change && next_part_is_change {
                if raw_lines.len() > context.saturating_mul(2) {
                    for line in &raw_lines[..context] {
                        output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                    let skipped = raw_lines.len() - context * 2;
                    output.push(format!(" {:>line_num_width$} ...", ""));
                    old_line_num += skipped;
                    new_line_num += skipped;
                    for line in &raw_lines[raw_lines.len() - context..] {
                        output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    for line in &raw_lines {
                        output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                }
            } else if last_was_change || next_part_is_change {
                let mut lines_to_show = raw_lines.clone();
                let mut skip_start = 0usize;
                let mut skip_end = 0usize;

                if !last_was_change {
                    skip_start = raw_lines.len().saturating_sub(context);
                    lines_to_show = raw_lines[skip_start..].to_vec();
                }
                if !next_part_is_change && lines_to_show.len() > context {
                    skip_end = lines_to_show.len() - context;
                    lines_to_show.truncate(context);
                }

                if skip_start > 0 {
                    output.push(format!(" {:>line_num_width$} ...", ""));
                    old_line_num += skip_start;
                    new_line_num += skip_start;
                }

                for line in &lines_to_show {
                    output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                    old_line_num += 1;
                    new_line_num += 1;
                }

                if skip_end > 0 {
                    output.push(format!(" {:>line_num_width$} ...", ""));
                    old_line_num += skip_end;
                    new_line_num += skip_end;
                }
            } else {
                old_line_num += raw_lines.len();
                new_line_num += raw_lines.len();
            }

            last_was_change = false;
        }
    }

    DiffResult {
        diff: output.join("\n"),
        first_changed_line,
    }
}

/// Generate a standard unified patch string for edit details (`apply_patch` parity).
pub fn create_edit_patch(file_path: &str, old_content: &str, new_content: &str) -> String {
    let mut opts = CreatePatchOptions::default();
    opts.context = Some(4.0);
    opts.header_options = Some(FILE_HEADERS_ONLY);
    create_two_files_patch(file_path, file_path, old_content, new_content, None, None, Some(opts))
}

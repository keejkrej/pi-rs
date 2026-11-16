//! Port of packages/durable/src/tools/edit-diff.ts

use pi_js::str16::{normalize_nfkc, trim_end};
use pi_js::vendor::jsdiff::{CreatePatchOptions, FILE_HEADERS_ONLY, create_two_files_patch, diff_lines};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

pub fn detect_line_ending(content: &str) -> LineEnding {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    match (crlf_idx, lf_idx) {
        (_, None) | (None, Some(_)) => LineEnding::Lf,
        (Some(c), Some(l)) => {
            if c < l {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            }
        }
    }
}

pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn restore_line_endings(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Crlf => text.replace('\n', "\r\n"),
        LineEnding::Lf => text.to_owned(),
    }
}

/// Normalize text for fuzzy matching: NFKC, strip trailing whitespace per line,
/// smart quotes -> ASCII, Unicode dashes/hyphens -> `-`, special spaces -> `' '`.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc = normalize_nfkc(text);
    let mut out = String::with_capacity(nfkc.len());
    for (idx, line) in nfkc.split('\n').enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        for ch in trim_end(line).chars() {
            let mapped = match ch {
                '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
                '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
                '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => '-',
                '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
                other => other,
            };
            out.push(mapped);
        }
    }
    out
}

fn split_lines_with_endings(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    content.split_inclusive('\n').collect()
}

#[derive(Clone, Copy, Debug)]
struct LineSpan {
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
struct MatchedEdit {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
}

#[derive(Clone, Copy, Debug)]
struct TextReplacement<'a> {
    match_index: usize,
    match_length: usize,
    new_text: &'a str,
}

fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0usize;
    split_lines_with_endings(content)
        .into_iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

fn get_replacement_line_range(lines: &[LineSpan], replacement: TextReplacement<'_>) -> pi_js::Result<(usize, usize)> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let Some(start_line) = lines
        .iter()
        .position(|line| replacement_start >= line.start && replacement_start < line.end)
    else {
        return Err(pi_js::Error::msg("Replacement range is outside the base content."));
    };

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err(pi_js::Error::msg("Replacement range is outside the base content."));
    }

    Ok((start_line, end_line + 1))
}

fn apply_replacements(content: &str, replacements: &[TextReplacement<'_>], offset: usize) -> String {
    let mut result = content.to_owned();
    for replacement in replacements.iter().rev() {
        let match_index = replacement.match_index - offset;
        let end = match_index + replacement.match_length;
        result.replace_range(match_index..end, replacement.new_text);
    }
    result
}

/// Apply replacements matched against `base_content` to `original_content` while preserving
/// unchanged line blocks from the original.
fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[TextReplacement<'_>],
) -> pi_js::Result<String> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(pi_js::Error::msg(
            "Cannot preserve unchanged lines because the base content has a different line count.",
        ));
    }

    struct Group<'a> {
        start_line: usize,
        end_line: usize,
        replacements: Vec<TextReplacement<'a>>,
    }

    let mut sorted = replacements.to_vec();
    sorted.sort_by_key(|r| r.match_index);
    let mut groups: Vec<Group<'_>> = Vec::new();
    for replacement in sorted {
        let (start_line, end_line) = get_replacement_line_range(&base_lines, replacement)?;
        if let Some(current) = groups.last_mut()
            && start_line < current.end_line
        {
            current.end_line = current.end_line.max(end_line);
            current.replacements.push(replacement);
            continue;
        }
        groups.push(Group {
            start_line,
            end_line,
            replacements: vec![replacement],
        });
    }

    let mut original_line_index = 0usize;
    let mut result = String::new();
    for group in groups {
        for line in &original_lines[original_line_index..group.start_line] {
            result.push_str(line);
        }
        let group_start_offset = base_lines[group.start_line].start;
        let group_end_offset = base_lines[group.end_line - 1].end;
        let rewritten = apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            &group.replacements,
            group_start_offset,
        );
        result.push_str(&rewritten);
        original_line_index = group.end_line;
    }
    for line in &original_lines[original_line_index..] {
        result.push_str(line);
    }

    Ok(result)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuzzyMatchResult {
    pub found: bool,
    pub index: usize,
    pub match_length: usize,
    pub used_fuzzy_match: bool,
    pub content_for_replacement: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedEditsResult {
    pub base_content: String,
    pub new_content: String,
}

/// Find `old_text` in `content`, trying exact match first, then fuzzy match.
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(exact_index) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index: exact_index,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_owned(),
        };
    }

    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    let Some(fuzzy_index) = fuzzy_content.find(&fuzzy_old_text) else {
        return FuzzyMatchResult {
            found: false,
            index: 0,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.to_owned(),
        };
    };

    FuzzyMatchResult {
        found: true,
        index: fuzzy_index,
        match_length: fuzzy_old_text.len(),
        used_fuzzy_match: true,
        content_for_replacement: fuzzy_content,
    }
}

/// Strip UTF-8 BOM if present, returning `(bom, text_without_bom)`.
pub fn strip_bom(content: &str) -> (&'static str, &str) {
    match content.strip_prefix('\u{FEFF}') {
        Some(rest) => ("\u{FEFF}", rest),
        None => ("", content),
    }
}

fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    if fuzzy_old_text.is_empty() {
        return pi_js::str16::len(&fuzzy_content).saturating_sub(1);
    }
    fuzzy_content.matches(&fuzzy_old_text).count()
}

fn not_found_error(path: &str, edit_index: usize, total_edits: usize) -> pi_js::Error {
    if total_edits == 1 {
        pi_js::Error::msg(format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        ))
    } else {
        pi_js::Error::msg(format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        ))
    }
}

fn duplicate_error(path: &str, edit_index: usize, total_edits: usize, occurrences: usize) -> pi_js::Error {
    if total_edits == 1 {
        pi_js::Error::msg(format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        ))
    } else {
        pi_js::Error::msg(format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        ))
    }
}

fn empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> pi_js::Error {
    if total_edits == 1 {
        pi_js::Error::msg(format!("oldText must not be empty in {path}."))
    } else {
        pi_js::Error::msg(format!("edits[{edit_index}].oldText must not be empty in {path}."))
    }
}

fn no_change_error(path: &str, total_edits: usize) -> pi_js::Error {
    if total_edits == 1 {
        pi_js::Error::msg(format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        ))
    } else {
        pi_js::Error::msg(format!(
            "No changes made to {path}. The replacements produced identical content."
        ))
    }
}

/// Apply one or more exact-text replacements to LF-normalized content.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> pi_js::Result<AppliedEditsResult> {
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();

    let total = normalized_edits.len();
    for (i, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(empty_old_text_error(path, i, total));
        }
    }

    let used_fuzzy_match = normalized_edits
        .iter()
        .any(|edit| fuzzy_find_text(normalized_content, &edit.old_text).used_fuzzy_match);
    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_owned()
    };

    let mut matched_edits = Vec::with_capacity(total);
    for (i, edit) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, &edit.old_text);
        if !match_result.found {
            return Err(not_found_error(path, i, total));
        }
        let occurrences = count_occurrences(&replacement_base_content, &edit.old_text);
        if occurrences > 1 {
            return Err(duplicate_error(path, i, total, occurrences));
        }
        matched_edits.push(MatchedEdit {
            edit_index: i,
            match_index: match_result.index,
            match_length: match_result.match_length,
            new_text: edit.new_text.clone(),
        });
    }

    matched_edits.sort_by_key(|m| m.match_index);
    for pair in matched_edits.windows(2) {
        let prev = &pair[0];
        let curr = &pair[1];
        if prev.match_index + prev.match_length > curr.match_index {
            return Err(pi_js::Error::msg(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                prev.edit_index, curr.edit_index
            )));
        }
    }

    let replacements: Vec<TextReplacement<'_>> = matched_edits
        .iter()
        .map(|m| TextReplacement {
            match_index: m.match_index,
            match_length: m.match_length,
            new_text: &m.new_text,
        })
        .collect();

    let base_content = normalized_content.to_owned();
    let new_content = if used_fuzzy_match {
        apply_replacements_preserving_unchanged_lines(normalized_content, &replacement_base_content, &replacements)?
    } else {
        apply_replacements(&replacement_base_content, &replacements, 0)
    };

    if base_content == new_content {
        return Err(no_change_error(path, total));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

/// Generate a standard unified patch.
pub fn generate_unified_patch(path: &str, old_content: &str, new_content: &str, context_lines: usize) -> String {
    create_two_files_patch(
        path,
        path,
        old_content,
        new_content,
        None,
        None,
        Some(CreatePatchOptions {
            context: Some(context_lines as f64),
            header_options: Some(FILE_HEADERS_ONLY),
            ..Default::default()
        }),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffStringResult {
    pub diff: String,
    pub first_changed_line: Option<usize>,
}

/// Generate a display-oriented diff string with line numbers and context.
pub fn generate_diff_string(old_content: &str, new_content: &str, context_lines: usize) -> DiffStringResult {
    let parts = diff_lines(old_content, new_content, None);
    let mut output: Vec<String> = Vec::new();

    let old_lines_count = old_content.split('\n').count();
    let new_lines_count = new_content.split('\n').count();
    let max_line_num = old_lines_count.max(new_lines_count);
    let line_num_width = max_line_num.to_string().len();

    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line = None;

    for (i, part) in parts.iter().enumerate() {
        let mut raw: Vec<&str> = part.value.split('\n').collect();
        if raw.last() == Some(&"") {
            raw.pop();
        }

        if part.added || part.removed {
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num);
            }
            for line in raw {
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
            let next_part_is_change = parts.get(i + 1).is_some_and(|next| next.added || next.removed);
            let has_leading_change = last_was_change;
            let has_trailing_change = next_part_is_change;

            if has_leading_change && has_trailing_change {
                if raw.len() <= context_lines * 2 {
                    for line in raw {
                        output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    let leading_lines = &raw[..context_lines];
                    let trailing_lines = &raw[raw.len() - context_lines..];
                    let skipped_lines = raw.len() - leading_lines.len() - trailing_lines.len();

                    for line in leading_lines {
                        output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                    output.push(format!(" {:>line_num_width$} ...", ""));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                    for line in trailing_lines {
                        output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                }
            } else if has_leading_change {
                let shown_count = raw.len().min(context_lines);
                let skipped_lines = raw.len() - shown_count;
                for line in &raw[..shown_count] {
                    output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                    old_line_num += 1;
                    new_line_num += 1;
                }
                if skipped_lines > 0 {
                    output.push(format!(" {:>line_num_width$} ...", ""));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }
            } else if has_trailing_change {
                let skipped_lines = raw.len().saturating_sub(context_lines);
                if skipped_lines > 0 {
                    output.push(format!(" {:>line_num_width$} ...", ""));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }
                for line in &raw[skipped_lines..] {
                    output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                    old_line_num += 1;
                    new_line_num += 1;
                }
            } else {
                old_line_num += raw.len();
                new_line_num += raw.len();
            }

            last_was_change = false;
        }
    }

    DiffStringResult {
        diff: output.join("\n"),
        first_changed_line,
    }
}

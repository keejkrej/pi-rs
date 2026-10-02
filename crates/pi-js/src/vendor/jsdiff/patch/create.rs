//! Port of `diff/libesm/patch/create.js` (diff@8.0.4).

use crate::error::{Error, Result};
use crate::vendor::jsdiff::diff::line::diff_lines_abortable;
use crate::vendor::jsdiff::types::{
    AbortableDiffOptions, Change, DiffLinesOptions, StructuredPatch, StructuredPatchHunk,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderOptions {
    pub include_index: bool,
    pub include_underline: bool,
    pub include_file_headers: bool,
}

pub const INCLUDE_HEADERS: HeaderOptions = HeaderOptions {
    include_index: true,
    include_underline: true,
    include_file_headers: true,
};

pub const FILE_HEADERS_ONLY: HeaderOptions = HeaderOptions {
    include_index: false,
    include_underline: false,
    include_file_headers: true,
};

pub const OMIT_HEADERS: HeaderOptions = HeaderOptions {
    include_index: false,
    include_underline: false,
    include_file_headers: false,
};

// PORT: `StructuredPatchOptionsNonabortable` (the abortable variant takes [`AbortableDiffOptions`] separately).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StructuredPatchOptions {
    /// describes how many lines of context should be included.
    /// You can set this to `Number.MAX_SAFE_INTEGER` or `Infinity` to include the entire file content in one hunk.
    /// @default 4
    pub context: Option<f64>,
    pub ignore_whitespace: Option<bool>,
    pub strip_trailing_cr: Option<bool>,
}

pub type StructuredPatchOptionsNonabortable = StructuredPatchOptions;

// PORT: `CreatePatchOptionsNonabortable` (the abortable variant takes [`AbortableDiffOptions`] separately).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CreatePatchOptions {
    pub context: Option<f64>,
    pub ignore_whitespace: Option<bool>,
    pub strip_trailing_cr: Option<bool>,
    pub header_options: Option<HeaderOptions>,
}

pub type CreatePatchOptionsNonabortable = CreatePatchOptions;

impl CreatePatchOptions {
    fn structured(&self) -> StructuredPatchOptions {
        StructuredPatchOptions {
            context: self.context,
            ignore_whitespace: self.ignore_whitespace,
            strip_trailing_cr: self.strip_trailing_cr,
        }
    }
}

/// returns an object with an array of hunk objects.
///
/// This method is similar to createTwoFilesPatch, but returns a data structure suitable for further processing.
/// @param oldFileName String to be output in the filename section of the patch for the removals
/// @param newFileName String to be output in the filename section of the patch for the additions
/// @param oldStr Original string value
/// @param newStr New string value
/// @param oldHeader Optional additional information to include in the old file header.
/// @param newHeader Optional additional information to include in the new file header.
pub fn structured_patch(
    old_file_name: &str,
    new_file_name: &str,
    old_str: &str,
    new_str: &str,
    old_header: Option<&str>,
    new_header: Option<&str>,
    options: Option<StructuredPatchOptions>,
) -> StructuredPatch {
    // PORT: Without maxEditLength/timeout the diff always completes.
    structured_patch_abortable(
        old_file_name,
        new_file_name,
        old_str,
        new_str,
        old_header,
        new_header,
        options,
        AbortableDiffOptions::default(),
    )
    .unwrap_or_default()
}

// PORT: `structuredPatch` with `maxEditLength` / `timeout`; `None` where jsdiff returns `undefined`.
#[allow(clippy::too_many_arguments)]
pub fn structured_patch_abortable(
    old_file_name: &str,
    new_file_name: &str,
    old_str: &str,
    new_str: &str,
    old_header: Option<&str>,
    new_header: Option<&str>,
    options: Option<StructuredPatchOptions>,
    abortable: AbortableDiffOptions,
) -> Option<StructuredPatch> {
    let options_obj = options.unwrap_or_default();
    let context = options_obj.context.unwrap_or(4.0);
    let diff = diff_lines_abortable(
        old_str,
        new_str,
        Some(DiffLinesOptions {
            ignore_whitespace: options_obj.ignore_whitespace,
            strip_trailing_cr: options_obj.strip_trailing_cr,
            ..Default::default()
        }),
        abortable,
    )?;
    Some(diff_lines_result_to_patch(
        old_file_name,
        new_file_name,
        old_header,
        new_header,
        diff,
        context,
    ))
}

fn diff_lines_result_to_patch(
    old_file_name: &str,
    new_file_name: &str,
    old_header: Option<&str>,
    new_header: Option<&str>,
    diff: Vec<Change>,
    context: f64,
) -> StructuredPatch {
    // STEP 1: Build up the patch with no "\ No newline at end of file" lines and with the arrays
    //         of lines containing trailing newline characters. We'll tidy up later...

    // Append an empty value to make cleanup easier
    let mut diff_lines: Vec<(bool, bool, Vec<String>)> = diff
        .into_iter()
        .map(|c| (c.added, c.removed, split_lines(&c.value)))
        .collect();
    diff_lines.push((false, false, Vec::new()));
    let diff_len = diff_lines.len();

    fn context_lines(lines: &[String]) -> Vec<String> {
        lines.iter().map(|entry| format!(" {entry}")).collect()
    }

    let mut hunks: Vec<StructuredPatchHunk> = Vec::new();
    let mut old_range_start = 0.0f64;
    let mut new_range_start = 0.0f64;
    let mut cur_range: Vec<String> = Vec::new();
    let mut old_line = 1.0f64;
    let mut new_line = 1.0f64;
    for i in 0..diff_len {
        let (added, removed) = (diff_lines[i].0, diff_lines[i].1);
        let lines = &diff_lines[i].2;
        if added || removed {
            // If we have previous context, start with that
            if !js_truthy(old_range_start) {
                old_range_start = old_line;
                new_range_start = new_line;
                if i > 0 {
                    let prev = &diff_lines[i - 1].2;
                    cur_range = if context > 0.0 {
                        context_lines(js_slice(prev, -context, None))
                    } else {
                        Vec::new()
                    };
                    old_range_start -= cur_range.len() as f64;
                    new_range_start -= cur_range.len() as f64;
                }
            }
            // Output our changes
            for line in lines {
                cur_range.push(format!("{}{}", if added { '+' } else { '-' }, line));
            }
            // Track the updated file position
            if added {
                new_line += lines.len() as f64;
            } else {
                old_line += lines.len() as f64;
            }
        } else {
            // Identical context lines. Track line changes
            if js_truthy(old_range_start) {
                // Close out any changes that have been output (or join overlapping)
                if (lines.len() as f64) <= context * 2.0 && i + 2 < diff_len {
                    // Overlapping
                    cur_range.extend(context_lines(lines));
                } else {
                    // end the range and output
                    let context_size = js_min(lines.len() as f64, context);
                    cur_range.extend(context_lines(js_slice(lines, 0.0, Some(context_size))));
                    hunks.push(StructuredPatchHunk {
                        old_start: old_range_start,
                        old_lines: old_line - old_range_start + context_size,
                        new_start: new_range_start,
                        new_lines: new_line - new_range_start + context_size,
                        lines: std::mem::take(&mut cur_range),
                    });
                    old_range_start = 0.0;
                    new_range_start = 0.0;
                }
            }
            old_line += lines.len() as f64;
            new_line += lines.len() as f64;
        }
    }

    // Step 2: eliminate the trailing `\n` from each line of each hunk, and, where needed, add
    //         "\ No newline at end of file".
    for hunk in &mut hunks {
        let mut lines = Vec::with_capacity(hunk.lines.len());
        for line in std::mem::take(&mut hunk.lines) {
            match line.strip_suffix('\n') {
                Some(stripped) => lines.push(stripped.to_string()),
                None => {
                    lines.push(line);
                    lines.push("\\ No newline at end of file".to_string());
                }
            }
        }
        hunk.lines = lines;
    }

    StructuredPatch {
        old_file_name: Some(old_file_name.to_string()),
        new_file_name: Some(new_file_name.to_string()),
        old_header: old_header.map(String::from),
        new_header: new_header.map(String::from),
        hunks,
        index: None,
    }
}

/// creates a unified diff patch.
/// @param patch either a single structured patch object (as returned by `structuredPatch`) or an array of them (as returned by `parsePatch`)
// PORT: jsdiff decrements `oldStart`/`newStart` of zero-length hunks on the passed object itself;
// the Rust port formats from a borrowed patch and leaves it unchanged.
pub fn format_patch(patch: &StructuredPatch, header_options: Option<HeaderOptions>) -> String {
    let header_options = header_options.unwrap_or(INCLUDE_HEADERS);
    let mut ret: Vec<String> = Vec::new();
    if header_options.include_index && patch.old_file_name == patch.new_file_name {
        ret.push(format!("Index: {}", js_opt_str(&patch.old_file_name)));
    }
    if header_options.include_underline {
        ret.push("===================================================================".to_string());
    }
    if header_options.include_file_headers {
        ret.push(format!(
            "--- {}{}",
            js_opt_str(&patch.old_file_name),
            patch.old_header.as_ref().map(|h| format!("\t{h}")).unwrap_or_default()
        ));
        ret.push(format!(
            "+++ {}{}",
            js_opt_str(&patch.new_file_name),
            patch.new_header.as_ref().map(|h| format!("\t{h}")).unwrap_or_default()
        ));
    }
    for hunk in &patch.hunks {
        let mut old_start = hunk.old_start;
        let mut new_start = hunk.new_start;
        // Unified Diff Format quirk: If the chunk size is 0,
        // the first number is one lower than one would expect.
        // https://www.artima.com/weblogs/viewpost.jsp?thread=164293
        if hunk.old_lines == 0.0 {
            old_start -= 1.0;
        }
        if hunk.new_lines == 0.0 {
            new_start -= 1.0;
        }
        ret.push(format!(
            "@@ -{},{} +{},{} @@",
            js_number(old_start),
            js_number(hunk.old_lines),
            js_number(new_start),
            js_number(hunk.new_lines)
        ));
        ret.extend(hunk.lines.iter().cloned());
    }
    ret.join("\n") + "\n"
}

// PORT: `formatPatch` for an array of structured patches (as returned by `parsePatch`).
pub fn format_patch_array(patches: &[StructuredPatch], header_options: Option<HeaderOptions>) -> Result<String> {
    let header_options = header_options.unwrap_or(INCLUDE_HEADERS);
    if patches.len() > 1 && !header_options.include_file_headers {
        return Err(Error::msg(
            "Cannot omit file headers on a multi-file patch. \
             (The result would be unparseable; how would a tool trying to apply \
             the patch know which changes are to which file?)",
        ));
    }
    Ok(patches
        .iter()
        .map(|p| format_patch(p, Some(header_options)))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// creates a unified diff patch by first computing a diff with `diffLines` and then serializing it to unified diff format.
/// @param oldFileName String to be output in the filename section of the patch for the removals
/// @param newFileName String to be output in the filename section of the patch for the additions
/// @param oldStr Original string value
/// @param newStr New string value
/// @param oldHeader Optional additional information to include in the old file header.
/// @param newHeader Optional additional information to include in the new file header.
pub fn create_two_files_patch(
    old_file_name: &str,
    new_file_name: &str,
    old_str: &str,
    new_str: &str,
    old_header: Option<&str>,
    new_header: Option<&str>,
    options: Option<CreatePatchOptions>,
) -> String {
    // PORT: Without maxEditLength/timeout the diff always completes.
    create_two_files_patch_abortable(
        old_file_name,
        new_file_name,
        old_str,
        new_str,
        old_header,
        new_header,
        options,
        AbortableDiffOptions::default(),
    )
    .unwrap_or_default()
}

// PORT: `createTwoFilesPatch` with `maxEditLength` / `timeout`; `None` where jsdiff returns `undefined`.
#[allow(clippy::too_many_arguments)]
pub fn create_two_files_patch_abortable(
    old_file_name: &str,
    new_file_name: &str,
    old_str: &str,
    new_str: &str,
    old_header: Option<&str>,
    new_header: Option<&str>,
    options: Option<CreatePatchOptions>,
    abortable: AbortableDiffOptions,
) -> Option<String> {
    let options = options.unwrap_or_default();
    let patch_obj = structured_patch_abortable(
        old_file_name,
        new_file_name,
        old_str,
        new_str,
        old_header,
        new_header,
        Some(options.structured()),
        abortable,
    )?;
    Some(format_patch(&patch_obj, options.header_options))
}

/// creates a unified diff patch.
///
/// Just like createTwoFilesPatch, but with oldFileName being equal to newFileName.
/// @param fileName String to be output in the filename section of the patch
/// @param oldStr Original string value
/// @param newStr New string value
/// @param oldHeader Optional additional information to include in the old file header.
/// @param newHeader Optional additional information to include in the new file header.
pub fn create_patch(
    file_name: &str,
    old_str: &str,
    new_str: &str,
    old_header: Option<&str>,
    new_header: Option<&str>,
    options: Option<CreatePatchOptions>,
) -> String {
    create_two_files_patch(file_name, file_name, old_str, new_str, old_header, new_header, options)
}

// PORT: `createPatch` with `maxEditLength` / `timeout`; `None` where jsdiff returns `undefined`.
pub fn create_patch_abortable(
    file_name: &str,
    old_str: &str,
    new_str: &str,
    old_header: Option<&str>,
    new_header: Option<&str>,
    options: Option<CreatePatchOptions>,
    abortable: AbortableDiffOptions,
) -> Option<String> {
    create_two_files_patch_abortable(
        file_name, file_name, old_str, new_str, old_header, new_header, options, abortable,
    )
}

/// Split `text` into an array of lines, including the trailing newline character (where present)
fn split_lines(text: &str) -> Vec<String> {
    let has_trailing_nl = text.ends_with('\n');
    let mut result: Vec<String> = text.split('\n').map(|line| format!("{line}\n")).collect();
    if has_trailing_nl {
        result.pop();
    } else if let Some(last) = result.last_mut() {
        last.pop();
    }
    result
}

// PORT: JS truthiness of a number.
fn js_truthy(n: f64) -> bool {
    n != 0.0 && !n.is_nan()
}

// PORT: `Math.min` (NaN-propagating).
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}

// PORT: `Array.prototype.slice(start, end)` index resolution.
fn js_slice<T>(items: &[T], start: f64, end: Option<f64>) -> &[T] {
    let len = items.len() as f64;
    let resolve = |rel: f64| -> f64 {
        // PORT: ToIntegerOrInfinity
        let rel = if rel.is_nan() { 0.0 } else { rel.trunc() };
        if rel < 0.0 { (len + rel).max(0.0) } else { rel.min(len) }
    };
    let from = resolve(start);
    let to = end.map_or(len, resolve);
    if from >= to {
        return &items[0..0];
    }
    &items[from as usize..to as usize]
}

// PORT: `String(n)` for a JS number.
pub(crate) fn js_number(n: f64) -> String {
    ryu_js::Buffer::new().format(n).to_string()
}

// PORT: String concatenation of a possibly-`undefined` string.
fn js_opt_str(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("undefined")
}

//! Port of `diff/libesm/patch/apply.js` (diff@8.0.4).

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::vendor::jsdiff::patch::line_endings::{is_unix, is_win, unix_to_win, win_to_unix};
use crate::vendor::jsdiff::patch::parse::parse_patch;
use crate::vendor::jsdiff::types::StructuredPatch;
use crate::vendor::jsdiff::util::distance_iterator::distance_iterator;
use crate::vendor::jsdiff::util::string::{has_only_unix_line_endings, has_only_win_line_endings};

// PORT: `compareLine(lineNumber, line, operation, patchContent)`. `line` is `None` where jsdiff passes
// `undefined` (a position outside the source text).
pub type CompareLine = Arc<CompareLineFn>;

// PORT: The function type behind [`CompareLine`].
pub type CompareLineFn = dyn Fn(f64, Option<&str>, &str, &str) -> bool + Send + Sync;

#[derive(Clone, Default)]
pub struct ApplyPatchOptions {
    /// Maximum Levenshtein distance (in lines deleted, added, or subtituted) between the context shown in a patch hunk and the lines found in the file.
    /// @default 0
    pub fuzz_factor: Option<f64>,
    /// If `true`, and if the file to be patched consistently uses different line endings to the patch (i.e. either the file always uses Unix line endings while the patch uses Windows ones, or vice versa), then `applyPatch` will behave as if the line endings in the patch were the same as those in the source file.
    /// (If `false`, the patch will usually fail to apply in such circumstances since lines deleted in the patch won't be considered to match those in the source file.)
    /// @default true
    pub auto_convert_line_endings: Option<bool>,
    /// Callback used to compare to given lines to determine if they should be considered equal when patching.
    /// Defaults to strict equality but may be overridden to provide fuzzier comparison.
    /// Should return false if the lines should be rejected.
    pub compare_line: Option<CompareLine>,
}

impl std::fmt::Debug for ApplyPatchOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplyPatchOptions")
            .field("fuzz_factor", &self.fuzz_factor)
            .field("auto_convert_line_endings", &self.auto_convert_line_endings)
            .field("compare_line", &self.compare_line.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

// PORT: The `patch` argument of `applyPatch`: a unified diff string, a structured patch, or an array of
// structured patches (as returned by `parsePatch`).
#[derive(Clone, Copy, Debug)]
pub enum PatchInput<'a> {
    Text(&'a str),
    Patch(&'a StructuredPatch),
    Array(&'a [StructuredPatch]),
}

impl<'a> From<&'a str> for PatchInput<'a> {
    fn from(value: &'a str) -> Self {
        PatchInput::Text(value)
    }
}

impl<'a> From<&'a String> for PatchInput<'a> {
    fn from(value: &'a String) -> Self {
        PatchInput::Text(value)
    }
}

impl<'a> From<&'a StructuredPatch> for PatchInput<'a> {
    fn from(value: &'a StructuredPatch) -> Self {
        PatchInput::Patch(value)
    }
}

impl<'a> From<&'a [StructuredPatch]> for PatchInput<'a> {
    fn from(value: &'a [StructuredPatch]) -> Self {
        PatchInput::Array(value)
    }
}

impl<'a> From<&'a Vec<StructuredPatch>> for PatchInput<'a> {
    fn from(value: &'a Vec<StructuredPatch>) -> Self {
        PatchInput::Array(value)
    }
}

/// attempts to apply a unified diff patch.
///
/// Hunks are applied first to last.
/// `applyPatch` first tries to apply the first hunk at the line number specified in the hunk header, and with all context lines matching exactly.
/// If that fails, it tries scanning backwards and forwards, one line at a time, to find a place to apply the hunk where the context lines match exactly.
/// If that still fails, and `fuzzFactor` is greater than zero, it increments the maximum number of mismatches (missing, extra, or changed context lines) that there can be between the hunk context and a region where we are trying to apply the patch such that the hunk will still be considered to match.
/// Regardless of `fuzzFactor`, lines to be deleted in the hunk *must* be present for a hunk to match, and the context lines *immediately* before and after an insertion must match exactly.
///
/// Once a hunk is successfully fitted, the process begins again with the next hunk.
/// Regardless of `fuzzFactor`, later hunks must be applied later in the file than earlier hunks.
///
/// If a hunk cannot be successfully fitted *anywhere* with fewer than `fuzzFactor` mismatches, `applyPatch` fails and returns `false`.
///
/// If a hunk is successfully fitted but not at the line number specified by the hunk header, all subsequent hunks have their target line number adjusted accordingly.
/// (e.g. if the first hunk is applied 10 lines below where the hunk header said it should fit, `applyPatch` will *start* looking for somewhere to apply the second hunk 10 lines below where its hunk header says it goes.)
///
/// If the patch was applied successfully, returns a string containing the patched text.
/// If the patch could not be applied (because some hunks in the patch couldn't be fitted to the text in `source`), `applyPatch` returns false.
///
/// @param patch a string diff or the output from the `parsePatch` or `structuredPatch` methods.
// PORT: Returns `Ok(None)` where jsdiff returns `false`; errors are the exceptions jsdiff throws.
pub fn apply_patch<'a>(
    source: &str,
    patch: impl Into<PatchInput<'a>>,
    options: Option<ApplyPatchOptions>,
) -> Result<Option<String>> {
    let options = options.unwrap_or_default();
    let parsed;
    let patches: &[StructuredPatch] = match patch.into() {
        PatchInput::Text(text) => {
            parsed = parse_patch(text)?;
            &parsed
        }
        PatchInput::Patch(patch) => std::slice::from_ref(patch),
        PatchInput::Array(patches) => patches,
    };
    if patches.len() > 1 {
        return Err(Error::msg("applyPatch only works with a single input."));
    }
    apply_structured_patch(source, patches.first(), &options)
}

// PORT: `lines[pos]` with JS property-access semantics (`undefined` outside the array or for a
// non-integer index).
fn line_at(lines: &[String], pos: f64) -> Option<&str> {
    if pos >= 0.0 && pos.fract() == 0.0 && pos < lines.len() as f64 {
        Some(lines[pos as usize].as_str())
    } else {
        None
    }
}

// PORT: `patchedLines[index] = value` on a JS array.
fn set_at(patched: &mut Vec<Option<String>>, index: usize, value: Option<String>) {
    if index < patched.len() {
        patched[index] = value;
    } else {
        patched.resize(index, None);
        patched.push(value);
    }
}

struct HunkFitter<'a> {
    lines: &'a [String],
    compare_line: &'a CompareLineFn,
}

impl HunkFitter<'_> {
    /// Checks if the hunk can be made to fit at the provided location with at most `maxErrors`
    /// insertions, substitutions, or deletions, while ensuring also that:
    /// - lines deleted in the hunk match exactly, and
    /// - wherever an insertion operation or block of insertion operations appears in the hunk, the
    ///   immediately preceding and following lines of context match exactly
    ///
    /// `toPos` should be set such that lines[toPos] is meant to match hunkLines[0].
    ///
    /// If the hunk can be applied, returns an object with properties `oldLineLastI` and
    /// `replacementLines`. Otherwise, returns null.
    // PORT: (Here: `Some(oldLineLastI)`, with `patched_lines` truncated to the replacement lines.)
    #[allow(clippy::too_many_arguments)]
    fn apply_hunk(
        &self,
        hunk_lines: &[String],
        mut to_pos: f64,
        max_errors: f64,
        mut hunk_lines_i: usize,
        mut last_context_line_matched: bool,
        patched_lines: &mut Vec<Option<String>>,
        mut patched_lines_length: usize,
    ) -> Option<f64> {
        let mut n_consecutive_old_context_lines = 0usize;
        let mut next_context_line_must_match = false;
        while hunk_lines_i < hunk_lines.len() {
            let hunk_line = hunk_lines[hunk_lines_i].as_str();
            let (operation, content) = match hunk_line.chars().next() {
                Some(c) => hunk_line.split_at(c.len_utf8()),
                None => (" ", hunk_line),
            };
            if operation == "-" {
                if (self.compare_line)(to_pos + 1.0, line_at(self.lines, to_pos), operation, content) {
                    to_pos += 1.0;
                    n_consecutive_old_context_lines = 0;
                } else {
                    let line = line_at(self.lines, to_pos);
                    if max_errors == 0.0 || line.is_none() {
                        return None;
                    }
                    set_at(patched_lines, patched_lines_length, line.map(String::from));
                    return self.apply_hunk(
                        hunk_lines,
                        to_pos + 1.0,
                        max_errors - 1.0,
                        hunk_lines_i,
                        false,
                        patched_lines,
                        patched_lines_length + 1,
                    );
                }
            }
            if operation == "+" {
                if !last_context_line_matched {
                    return None;
                }
                set_at(patched_lines, patched_lines_length, Some(content.to_string()));
                patched_lines_length += 1;
                n_consecutive_old_context_lines = 0;
                next_context_line_must_match = true;
            }
            if operation == " " {
                n_consecutive_old_context_lines += 1;
                let line = line_at(self.lines, to_pos);
                set_at(patched_lines, patched_lines_length, line.map(String::from));
                if (self.compare_line)(to_pos + 1.0, line, operation, content) {
                    patched_lines_length += 1;
                    last_context_line_matched = true;
                    next_context_line_must_match = false;
                    to_pos += 1.0;
                } else {
                    if next_context_line_must_match || max_errors == 0.0 {
                        return None;
                    }
                    // Consider 3 possibilities in sequence:
                    // 1. lines contains a *substitution* not included in the patch context, or
                    // 2. lines contains an *insertion* not included in the patch context, or
                    // 3. lines contains a *deletion* not included in the patch context
                    // The first two options are of course only possible if the line from lines is non-null -
                    // i.e. only option 3 is possible if we've overrun the end of the old file.
                    if line.is_some_and(|l| !l.is_empty()) {
                        if let Some(result) = self.apply_hunk(
                            hunk_lines,
                            to_pos + 1.0,
                            max_errors - 1.0,
                            hunk_lines_i + 1,
                            false,
                            patched_lines,
                            patched_lines_length + 1,
                        ) {
                            return Some(result);
                        }
                        if let Some(result) = self.apply_hunk(
                            hunk_lines,
                            to_pos + 1.0,
                            max_errors - 1.0,
                            hunk_lines_i,
                            false,
                            patched_lines,
                            patched_lines_length + 1,
                        ) {
                            return Some(result);
                        }
                    }
                    return self.apply_hunk(
                        hunk_lines,
                        to_pos,
                        max_errors - 1.0,
                        hunk_lines_i + 1,
                        false,
                        patched_lines,
                        patched_lines_length,
                    );
                }
            }
            hunk_lines_i += 1;
        }
        // Before returning, trim any unmodified context lines off the end of patchedLines and reduce
        // toPos (and thus oldLineLastI) accordingly. This allows later hunks to be applied to a region
        // that starts in this hunk's trailing context.
        patched_lines_length -= n_consecutive_old_context_lines;
        to_pos -= n_consecutive_old_context_lines as f64;
        patched_lines.resize(patched_lines_length, None);
        Some(to_pos - 1.0)
    }
}

fn apply_structured_patch(
    source: &str,
    patch: Option<&StructuredPatch>,
    options: &ApplyPatchOptions,
) -> Result<Option<String>> {
    let Some(mut patch) = patch else {
        return Err(Error::js(
            "TypeError",
            "Cannot read properties of undefined (reading 'hunks')",
        ));
    };
    let converted;
    if options.auto_convert_line_endings.unwrap_or(true) {
        if has_only_win_line_endings(source) && is_unix(patch) {
            converted = unix_to_win(patch);
            patch = &converted;
        } else if has_only_unix_line_endings(source) && is_win(patch) {
            converted = win_to_unix(patch);
            patch = &converted;
        }
    }
    // Apply the diff to the input
    let mut lines: Vec<String> = source.split('\n').map(String::from).collect();
    let hunks = &patch.hunks;
    let default_compare_line =
        |_line_number: f64, line: Option<&str>, _operation: &str, patch_content: &str| line == Some(patch_content);
    let compare_line: &CompareLineFn = match &options.compare_line {
        Some(compare_line) => compare_line.as_ref(),
        None => &default_compare_line,
    };
    // options.fuzzFactor || 0
    let fuzz_factor = match options.fuzz_factor {
        Some(f) if f != 0.0 && !f.is_nan() => f,
        _ => 0.0,
    };
    let mut min_line = 0.0f64;
    if fuzz_factor < 0.0 || !(fuzz_factor.is_finite() && fuzz_factor.fract() == 0.0) {
        return Err(Error::msg("fuzzFactor must be a non-negative integer"));
    }
    // Special case for empty patch.
    let Some(last_hunk) = hunks.last() else {
        return Ok(Some(source.to_string()));
    };
    // Before anything else, handle EOFNL insertion/removal. If the patch tells us to make a change
    // to the EOFNL that is redundant/impossible - i.e. to remove a newline that's not there, or add a
    // newline that already exists - then we either return false and fail to apply the patch (if
    // fuzzFactor is 0) or simply ignore the problem and do nothing (if fuzzFactor is >0).
    // If we do need to remove/add a newline at EOF, this will always be in the final hunk:
    let mut prev_line = "";
    let mut remove_eofnl = false;
    let mut add_eofnl = false;
    for line in &last_hunk.lines {
        if line.starts_with('\\') {
            if prev_line.starts_with('+') {
                remove_eofnl = true;
            } else if prev_line.starts_with('-') {
                add_eofnl = true;
            }
        }
        prev_line = line;
    }
    let last_line_is_empty = |lines: &Vec<String>| lines.last().is_some_and(|l| l.is_empty());
    if remove_eofnl {
        if add_eofnl {
            // This means the final line gets changed but doesn't have a trailing newline in either the
            // original or patched version. In that case, we do nothing if fuzzFactor > 0, and if
            // fuzzFactor is 0, we simply validate that the source file has no trailing newline.
            if fuzz_factor == 0.0 && last_line_is_empty(&lines) {
                return Ok(None);
            }
        } else if last_line_is_empty(&lines) {
            lines.pop();
        } else if fuzz_factor == 0.0 {
            return Ok(None);
        }
    } else if add_eofnl {
        if !last_line_is_empty(&lines) {
            lines.push(String::new());
        } else if fuzz_factor == 0.0 {
            return Ok(None);
        }
    }

    let fitter = HunkFitter {
        lines: &lines,
        compare_line,
    };
    let mut result_lines: Vec<Option<String>> = Vec::new();
    // Search best fit offsets for each hunk based on the previous ones
    let mut prev_hunk_offset = 0.0f64;
    for hunk in hunks {
        let mut hunk_result: Option<(Vec<Option<String>>, f64)> = None;
        let max_line = lines.len() as f64 - hunk.old_lines + fuzz_factor;
        let mut to_pos = f64::NAN;
        let mut max_errors = 0.0f64;
        while max_errors <= fuzz_factor {
            to_pos = hunk.old_start + prev_hunk_offset - 1.0;
            let mut iterator = distance_iterator(to_pos, min_line, max_line);
            loop {
                let mut patched_lines = Vec::new();
                if let Some(old_line_last_i) =
                    fitter.apply_hunk(&hunk.lines, to_pos, max_errors, 0, true, &mut patched_lines, 0)
                {
                    hunk_result = Some((patched_lines, old_line_last_i));
                    break;
                }
                match iterator.next() {
                    Some(next) => to_pos = next,
                    None => break,
                }
            }
            if hunk_result.is_some() {
                break;
            }
            max_errors += 1.0;
        }
        let Some((patched_lines, old_line_last_i)) = hunk_result else {
            return Ok(None);
        };
        // Copy everything from the end of where we applied the last hunk to the start of this hunk
        let mut i = min_line;
        while i < to_pos {
            result_lines.push(line_at(&lines, i).map(String::from));
            i += 1.0;
        }
        // Add the lines produced by applying the hunk:
        result_lines.extend(patched_lines);
        // Set lower text limit to end of the current hunk, so next ones don't try
        // to fit over already patched text
        min_line = old_line_last_i + 1.0;
        // Note the offset between where the patch said the hunk should've applied and where we
        // applied it, so we can adjust future hunks accordingly:
        prev_hunk_offset = to_pos + 1.0 - hunk.old_start;
    }
    // Copy over the rest of the lines from the old text
    let mut i = min_line;
    while i < lines.len() as f64 {
        result_lines.push(line_at(&lines, i).map(String::from));
        i += 1.0;
    }
    // PORT: Array#join renders `undefined` entries as empty strings.
    Ok(Some(
        result_lines
            .iter()
            .map(|l| l.as_deref().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n"),
    ))
}

// PORT: `ApplyPatchesOptions`: `ApplyPatchOptions` plus the per-file callbacks.
// PORT: the node-style callbacks `loadFile(index, cb)`, `patched(index, content, cb)` and
// `complete(err)` become synchronous closures returning `Result`; `complete(err)` is the return
// value of `apply_patches`.
pub struct ApplyPatchesOptions<'a> {
    pub fuzz_factor: Option<f64>,
    pub auto_convert_line_endings: Option<bool>,
    pub compare_line: Option<CompareLine>,
    pub load_file: LoadFile<'a>,
    pub patched: Patched<'a>,
}

// PORT: `loadFile(index, callback)`: returns the contents of the file `index` patches.
pub type LoadFile<'a> = Box<dyn FnMut(&StructuredPatch) -> Result<String> + 'a>;

// PORT: `patched(index, content, callback)`: receives the `applyPatch` result for `index`.
pub type Patched<'a> = Box<dyn FnMut(&StructuredPatch, Option<String>) -> Result<()> + 'a>;

// PORT: The `uniDiff` argument of `applyPatches`.
#[derive(Clone, Copy, Debug)]
pub enum PatchesInput<'a> {
    Text(&'a str),
    Array(&'a [StructuredPatch]),
}

impl<'a> From<&'a str> for PatchesInput<'a> {
    fn from(value: &'a str) -> Self {
        PatchesInput::Text(value)
    }
}

impl<'a> From<&'a String> for PatchesInput<'a> {
    fn from(value: &'a String) -> Self {
        PatchesInput::Text(value)
    }
}

impl<'a> From<&'a [StructuredPatch]> for PatchesInput<'a> {
    fn from(value: &'a [StructuredPatch]) -> Self {
        PatchesInput::Array(value)
    }
}

impl<'a> From<&'a Vec<StructuredPatch>> for PatchesInput<'a> {
    fn from(value: &'a Vec<StructuredPatch>) -> Self {
        PatchesInput::Array(value)
    }
}

/// applies one or more patches.
///
/// `patch` may be either an array of structured patch objects, or a string representing a patch in unified diff format (which may patch one or more files).
///
/// This method will iterate over the contents of the patch and apply to data provided through callbacks. The general flow for each patch index is:
///
/// - `options.loadFile(index, callback)` is called. The caller should then load the contents of the file and then pass that to the `callback(err, data)` callback. Passing an `err` will terminate further patch execution.
/// - `options.patched(index, content, callback)` is called once the patch has been applied. `content` will be the return value from `applyPatch`. When it's ready, the caller should call `callback(err)` callback. Passing an `err` will terminate further patch execution.
///
/// Once all patches have been applied or an error occurs, the `options.complete(err)` callback is made.
pub fn apply_patches<'p>(uni_diff: impl Into<PatchesInput<'p>>, mut options: ApplyPatchesOptions<'_>) -> Result<()> {
    let parsed;
    let sp_diff: &[StructuredPatch] = match uni_diff.into() {
        PatchesInput::Text(text) => {
            parsed = parse_patch(text)?;
            &parsed
        }
        PatchesInput::Array(patches) => patches,
    };
    let apply_options = ApplyPatchOptions {
        fuzz_factor: options.fuzz_factor,
        auto_convert_line_endings: options.auto_convert_line_endings,
        compare_line: options.compare_line.clone(),
    };
    for index in sp_diff {
        let data = (options.load_file)(index)?;
        let updated_content = apply_patch(&data, index, Some(apply_options.clone()))?;
        (options.patched)(index, updated_content)?;
    }
    Ok(())
}

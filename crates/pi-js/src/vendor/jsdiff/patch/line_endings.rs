//! Port of `diff/libesm/patch/line-endings.js` (diff@8.0.4).

// PORT: The JS functions accept a single patch or an array; the array forms are the `*_array` functions.

use crate::vendor::jsdiff::types::{StructuredPatch, StructuredPatchHunk};

fn map_lines(patch: &StructuredPatch, f: impl Fn(&StructuredPatchHunk, usize, &String) -> String) -> StructuredPatch {
    StructuredPatch {
        hunks: patch
            .hunks
            .iter()
            .map(|hunk| StructuredPatchHunk {
                lines: hunk
                    .lines
                    .iter()
                    .enumerate()
                    .map(|(i, line)| f(hunk, i, line))
                    .collect(),
                ..hunk.clone()
            })
            .collect(),
        ..patch.clone()
    }
}

// PORT: `hunk.lines[i + 1]?.startsWith('\\')`
fn next_is_marker(hunk: &StructuredPatchHunk, i: usize) -> bool {
    hunk.lines.get(i + 1).is_some_and(|next| next.starts_with('\\'))
}

pub fn unix_to_win(patch: &StructuredPatch) -> StructuredPatch {
    map_lines(patch, |hunk, i, line| {
        if line.starts_with('\\') || line.ends_with('\r') || next_is_marker(hunk, i) {
            line.clone()
        } else {
            format!("{line}\r")
        }
    })
}

pub fn unix_to_win_array(patches: &[StructuredPatch]) -> Vec<StructuredPatch> {
    patches.iter().map(unix_to_win).collect()
}

pub fn win_to_unix(patch: &StructuredPatch) -> StructuredPatch {
    map_lines(patch, |_, _, line| line.strip_suffix('\r').unwrap_or(line).to_string())
}

pub fn win_to_unix_array(patches: &[StructuredPatch]) -> Vec<StructuredPatch> {
    patches.iter().map(win_to_unix).collect()
}

/// Returns true if the patch consistently uses Unix line endings (or only involves one line and has
/// no line endings).
pub fn is_unix(patch: &StructuredPatch) -> bool {
    is_unix_array(std::slice::from_ref(patch))
}

// PORT: `isUnix` for an array of patches.
pub fn is_unix_array(patches: &[StructuredPatch]) -> bool {
    !patches.iter().any(|index| {
        index.hunks.iter().any(|hunk| {
            hunk.lines
                .iter()
                .any(|line| !line.starts_with('\\') && line.ends_with('\r'))
        })
    })
}

/// Returns true if the patch uses Windows line endings and only Windows line endings.
pub fn is_win(patch: &StructuredPatch) -> bool {
    is_win_array(std::slice::from_ref(patch))
}

// PORT: `isWin` for an array of patches.
pub fn is_win_array(patches: &[StructuredPatch]) -> bool {
    patches.iter().any(|index| {
        index
            .hunks
            .iter()
            .any(|hunk| hunk.lines.iter().any(|line| line.ends_with('\r')))
    }) && patches.iter().all(|index| {
        index.hunks.iter().all(|hunk| {
            hunk.lines
                .iter()
                .enumerate()
                .all(|(i, line)| line.starts_with('\\') || line.ends_with('\r') || next_is_marker(hunk, i))
        })
    })
}

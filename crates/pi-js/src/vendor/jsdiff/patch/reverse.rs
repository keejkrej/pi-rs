//! Port of `diff/libesm/patch/reverse.js` (diff@8.0.4).

use crate::vendor::jsdiff::types::{StructuredPatch, StructuredPatchHunk};

/// @param patch either a single structured patch object (as returned by `structuredPatch`) or an array of them (as returned by `parsePatch`).
/// @returns a new structured patch which when applied will undo the original `patch`.
pub fn reverse_patch(structured_patch: &StructuredPatch) -> StructuredPatch {
    StructuredPatch {
        old_file_name: structured_patch.new_file_name.clone(),
        old_header: structured_patch.new_header.clone(),
        new_file_name: structured_patch.old_file_name.clone(),
        new_header: structured_patch.old_header.clone(),
        hunks: structured_patch
            .hunks
            .iter()
            .map(|hunk| StructuredPatchHunk {
                old_lines: hunk.new_lines,
                old_start: hunk.new_start,
                new_lines: hunk.old_lines,
                new_start: hunk.old_start,
                lines: hunk
                    .lines
                    .iter()
                    .map(|l| {
                        if let Some(rest) = l.strip_prefix('-') {
                            return format!("+{rest}");
                        }
                        if let Some(rest) = l.strip_prefix('+') {
                            return format!("-{rest}");
                        }
                        l.clone()
                    })
                    .collect(),
            })
            .collect(),
        index: structured_patch.index.clone(),
    }
}

// PORT: `reversePatch` for an array of patches: each patch is reversed and the order of the array too.
pub fn reverse_patch_array(structured_patches: &[StructuredPatch]) -> Vec<StructuredPatch> {
    structured_patches.iter().rev().map(reverse_patch).collect()
}

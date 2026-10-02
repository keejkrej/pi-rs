//! Port of `diff/libesm/convert/dmp.js` (diff@8.0.4).

use crate::vendor::jsdiff::types::ChangeObject;

/// converts a list of change objects to the format returned by Google's [diff-match-patch](https://github.com/google/diff-match-patch) library
pub fn convert_changes_to_dmp<V: Clone>(changes: &[ChangeObject<V>]) -> Vec<(i32, V)> {
    changes
        .iter()
        .map(|change| {
            let operation = if change.added {
                1
            } else if change.removed {
                -1
            } else {
                0
            };
            (operation, change.value.clone())
        })
        .collect()
}

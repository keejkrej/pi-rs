//! Port of packages/coding-agent/src/utils/deprecation.ts

use std::sync::{LazyLock, Mutex};

use indexmap::IndexSet;

use crate::vendor::chalk::chalk;

static EMITTED_DEPRECATION_WARNINGS: LazyLock<Mutex<IndexSet<String>>> = LazyLock::new(|| Mutex::new(IndexSet::new()));

pub fn warn_deprecation(message: &str) {
    if let Ok(mut guard) = EMITTED_DEPRECATION_WARNINGS.lock() {
        if guard.contains(message) {
            return;
        }
        guard.insert(message.to_owned());
    }
    pi_js::console::warn(&chalk().yellow().apply(&format!("Deprecation warning: {message}")));
}

/// Clear deprecation warning state. Exported for tests.
pub fn clear_deprecation_warnings_for_tests() {
    if let Ok(mut guard) = EMITTED_DEPRECATION_WARNINGS.lock() {
        guard.clear();
    }
}

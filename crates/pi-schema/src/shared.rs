//! Shared ECMAScript regular expressions cached weakly by pattern source.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::types::SchemaError;

fn regex_cache() -> &'static Mutex<HashMap<String, Weak<regress::Regex>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Weak<regress::Regex>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Compiles `source` as an ECMAScript regular expression with the `u` flag, falling back to flagless mode if `u`
/// rejects an older escape, and shares the compiled `Regex` weakly across schemas.
pub fn shared_regex(source: &str) -> Result<Arc<regress::Regex>, SchemaError> {
    if let Ok(guard) = regex_cache().lock()
        && let Some(hit) = guard.get(source).and_then(Weak::upgrade)
    {
        return Ok(hit);
    }

    let compiled = compile_ecma_regex(source)?;
    let arc = Arc::new(compiled);

    if let Ok(mut guard) = regex_cache().lock() {
        // Re-check in case another thread compiled the same pattern concurrently.
        if let Some(hit) = guard.get(source).and_then(Weak::upgrade) {
            return Ok(hit);
        }
        guard.retain(|_, w| w.strong_count() > 0);
        guard.insert(source.to_string(), Arc::downgrade(&arc));
    }

    Ok(arc)
}

/// Number of live cached regular expressions (prunes dropped entries first).
pub fn shared_regex_count() -> usize {
    let Ok(mut guard) = regex_cache().lock() else {
        return 0;
    };
    guard.retain(|_, w| w.strong_count() > 0);
    guard.len()
}

fn compile_ecma_regex(source: &str) -> Result<regress::Regex, SchemaError> {
    match pi_js::regex::ecma(source, "u") {
        Ok(re) => Ok(re),
        Err(_) => pi_js::regex::ecma(source, "").map_err(|e| SchemaError::InvalidRegex {
            pattern: source.to_string(),
            message: e.to_string(),
        }),
    }
}

//! Port of packages/ai/src/session-resources.ts

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use indexmap::IndexMap;

use pi_js::{Error, Result};

pub type SessionResourceCleanup = Arc<dyn Fn(Option<&str>) -> Result<()> + Send + Sync>;
pub type UnregisterSessionResourceCleanup = Box<dyn FnOnce() + Send + Sync>;

struct CleanupRegistry {
    next_id: AtomicU64,
    hooks: Mutex<IndexMap<u64, SessionResourceCleanup>>,
}

static REGISTRY: OnceLock<CleanupRegistry> = OnceLock::new();

fn registry() -> &'static CleanupRegistry {
    REGISTRY.get_or_init(|| CleanupRegistry {
        next_id: AtomicU64::new(1),
        hooks: Mutex::new(IndexMap::new()),
    })
}

/// Register a callback invoked when session-scoped resources should be released.
/// Returns a handle closure that unregisters the callback.
pub fn register_session_resource_cleanup<F>(cleanup: F) -> UnregisterSessionResourceCleanup
where
    F: Fn(Option<&str>) + Send + Sync + 'static,
{
    register_session_resource_cleanup_fallible(move |session_id| {
        cleanup(session_id);
        Ok(())
    })
}

/// Register a fallible session cleanup callback whose errors are aggregated by [`cleanup_session_resources`].
pub fn register_session_resource_cleanup_fallible<F>(cleanup: F) -> UnregisterSessionResourceCleanup
where
    F: Fn(Option<&str>) -> Result<()> + Send + Sync + 'static,
{
    let reg = registry();
    let id = reg.next_id.fetch_add(1, Ordering::Relaxed);
    let mut guard = match reg.hooks.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.insert(id, Arc::new(cleanup));
    Box::new(move || {
        let reg = registry();
        let mut guard = match reg.hooks.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.shift_remove(&id);
    })
}

/// Run all registered cleanup hooks for the optional `session_id`.
/// Continues through all hooks even if an earlier hook fails, returning an
/// error only when at least one hook failed.
pub fn cleanup_session_resources(session_id: Option<&str>) -> Result<()> {
    let hooks: Vec<SessionResourceCleanup> = {
        let guard = match registry().hooks.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.values().cloned().collect()
    };

    let mut errors: Vec<String> = Vec::new();
    for cleanup in hooks {
        if let Err(err) = cleanup(session_id) {
            errors.push(err.to_string());
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(Error::msg(format!(
            "Failed to cleanup session resources: {}",
            errors.join("; ")
        )))
    }
}

//! Port of packages/coding-agent/src/core/tools/file-mutation-queue.ts

use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use tokio::sync::Mutex as AsyncMutex;

struct QueueEntry {
    mutex: Arc<AsyncMutex<()>>,
    waiters: usize,
}

static FILE_MUTATION_QUEUES: LazyLock<Mutex<IndexMap<String, QueueEntry>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

struct QueueCleanupGuard {
    key: String,
}

impl Drop for QueueCleanupGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = FILE_MUTATION_QUEUES.lock()
            && let Some(entry) = map.get_mut(&self.key)
        {
            entry.waiters = entry.waiters.saturating_sub(1);
            if entry.waiters == 0 {
                map.shift_remove(&self.key);
            }
        }
    }
}

fn find_nearest_existing_realpath(resolved: &str) -> String {
    if let Ok(real) = pi_js::fs::realpath(resolved) {
        return real;
    }

    let mut current = resolved.to_string();
    let mut missing_segments = Vec::new();
    loop {
        let parent = pi_js::path::dirname(&current);
        let base = pi_js::path::basename(&current, None);
        if parent == current || base.is_empty() {
            break;
        }
        missing_segments.push(base);
        if let Ok(real_parent) = pi_js::fs::realpath(&parent) {
            missing_segments.reverse();
            let mut parts = Vec::with_capacity(1 + missing_segments.len());
            parts.push(real_parent.as_str());
            for seg in &missing_segments {
                parts.push(seg.as_str());
            }
            return pi_js::path::join(&parts);
        }
        current = parent;
    }

    resolved.to_string()
}

fn get_file_mutation_key(file_path: &str) -> String {
    let resolved = pi_js::path::resolve(&[file_path]);
    let canonical = find_nearest_existing_realpath(&resolved);
    if pi_js::env::platform() == "win32" {
        canonical.to_lowercase()
    } else {
        canonical
    }
}

/// Serialize mutations targeting the same file on disk (even through symlinks or uncreated paths).
pub async fn with_file_mutation_queue<F, Fut, T>(file_path: &str, f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let key = get_file_mutation_key(file_path);

    let file_mutex = {
        let mut map = FILE_MUTATION_QUEUES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = map.entry(key.clone()).or_insert_with(|| QueueEntry {
            mutex: Arc::new(AsyncMutex::new(())),
            waiters: 0,
        });
        entry.waiters += 1;
        Arc::clone(&entry.mutex)
    };

    let _cleanup = QueueCleanupGuard { key };
    let _permit = file_mutex.lock().await;
    f().await
}

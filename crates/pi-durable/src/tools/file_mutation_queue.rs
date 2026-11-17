//! Port of packages/durable/src/tools/file-mutation-queue.ts

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use pi_chord::Context;

use crate::env::{ExecutionEnv, FileErrorCode, get_or_throw};

/// Per-file FIFO locks, keyed by `{env.id}\0{canonical_path}`.
static QUEUES: LazyLock<Mutex<IndexMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

async fn mutation_key(env: &dyn ExecutionEnv, path: &str, context: &Context) -> pi_js::Result<String> {
    let absolute_path = get_or_throw(env.absolute_path(path, context).await)?;
    let canon = canonical(env, &absolute_path, context).await?;
    Ok(format!("{}\0{canon}", env.id()))
}

/// The canonical path; for a file that does not exist yet, its canonical parent joined with its name,
/// so a `write` that creates a file and a later mutation of it share one key even under a symlinked directory.
fn canonical<'a>(
    env: &'a dyn ExecutionEnv,
    absolute_path: &'a str,
    context: &'a Context,
) -> Pin<Box<dyn Future<Output = pi_js::Result<String>> + Send + 'a>> {
    Box::pin(async move {
        match env.canonical_path(absolute_path, context).await {
            Ok(value) => Ok(value),
            Err(error) if error.code == FileErrorCode::NotSupported => Ok(absolute_path.to_owned()),
            Err(error) if error.code != FileErrorCode::NotFound => Err(error.into()),
            Err(_) => {
                // The file system splits the path, so a name may contain characters that are separators elsewhere.
                let parent = get_or_throw(env.join_path(&[absolute_path, ".."], context).await)?;
                if parent == absolute_path || !absolute_path.starts_with(&parent) {
                    return Ok(absolute_path.to_owned());
                }
                let sep_len = usize::from(!(parent.ends_with('/') || parent.ends_with('\\')));
                let Some(name) = absolute_path.get(parent.len() + sep_len..) else {
                    return Ok(absolute_path.to_owned());
                };
                let canon_parent = canonical(env, &parent, context).await?;
                get_or_throw(env.join_path(&[&canon_parent, name], context).await)
            }
        }
    })
}

struct QueueSlot {
    // Dropped first so the mutex is released before map pruning runs.
    _guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    _cleanup: QueueCleanup,
}

struct QueueCleanup {
    key: String,
    mutex: Arc<tokio::sync::Mutex<()>>,
}

impl Drop for QueueCleanup {
    fn drop(&mut self) {
        if let Ok(mut map) = QUEUES.lock()
            && let Some(current) = map.get(&self.key)
            && Arc::ptr_eq(current, &self.mutex)
            && Arc::strong_count(&self.mutex) == 2
        {
            map.shift_remove(&self.key);
        }
    }
}

/// Serialize `edit` and `write` mutations of one file within this process: same file system id and
/// canonical path, whichever environment object the call got. Other files, and other file systems,
/// never wait. Concurrent calls on one file run in the order their keys resolve. Not a lock against
/// `bash` or other processes.
pub async fn with_file_mutation_queue<T, F, Fut>(
    env: &dyn ExecutionEnv,
    path: &str,
    fn_call: F,
    context: &Context,
) -> pi_js::Result<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = pi_js::Result<T>>,
{
    let key = mutation_key(env, path, context).await?;
    let mutex = {
        let mut map = QUEUES.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(
            map.entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    };
    let cleanup = QueueCleanup {
        key,
        mutex: Arc::clone(&mutex),
    };
    let guard = mutex.lock_owned().await;
    let _slot = QueueSlot {
        _guard: Some(guard),
        _cleanup: cleanup,
    };
    fn_call().await
}

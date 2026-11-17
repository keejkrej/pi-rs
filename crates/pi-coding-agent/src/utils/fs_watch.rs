//! Port of packages/coding-agent/src/utils/fs-watch.ts

use std::path::Path;
use std::sync::{Arc, Mutex};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

pub const FS_WATCH_RETRY_DELAY_MS: u64 = 5000;

/// Handle owning a `notify` filesystem watcher.
pub struct FsWatcher {
    inner: Mutex<Option<RecommendedWatcher>>,
}

impl FsWatcher {
    pub fn close(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.take();
        }
    }
}

pub fn close_watcher(watcher: Option<&FsWatcher>) {
    if let Some(w) = watcher {
        w.close();
    }
}

pub fn watch_with_error_handler<F, E>(path: &str, listener: F, on_error: E) -> Option<Arc<FsWatcher>>
where
    F: Fn(&str, &str) + Send + Sync + 'static,
    E: Fn() + Send + Sync + 'static,
{
    let listener = Arc::new(listener);
    let on_error = Arc::new(on_error);
    let listener_cb = listener.clone();
    let on_error_cb = on_error.clone();

    let watcher_res = notify::recommended_watcher(move |res: notify::Result<Event>| match res {
        Ok(event) => {
            let event_type = match event.kind {
                notify::EventKind::Create(_) | notify::EventKind::Remove(_) => "rename",
                _ => "change",
            };
            let filename = event
                .paths
                .first()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            listener_cb(event_type, &filename);
        }
        Err(_) => {
            on_error_cb();
        }
    });

    let mut watcher = match watcher_res {
        Ok(w) => w,
        Err(_) => {
            on_error();
            return None;
        }
    };

    if watcher.watch(Path::new(path), RecursiveMode::NonRecursive).is_err() {
        on_error();
        return None;
    }

    Some(Arc::new(FsWatcher {
        inner: Mutex::new(Some(watcher)),
    }))
}

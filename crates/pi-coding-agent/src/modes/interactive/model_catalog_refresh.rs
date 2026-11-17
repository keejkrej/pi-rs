//! Port of packages/coding-agent/src/modes/interactive/model-catalog-refresh.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use pi_ai::Model;
use pi_js::abort::{AbortController, AbortReason, AbortSignal};
use pi_js::{BoxFuture, Error, Result};
use tokio::sync::watch;

// WIRE(@tui-w3): Implement `ModelCatalogRuntime` for `Arc<crate::core::model_runtime::ModelRuntime>` (and use `crate::utils::abort::race_with_abort_signal`) once @mcp's `core-models-pkg` merges onto main.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelCatalogRefreshResult {
    pub aborted: bool,
    pub errors: IndexMap<String, String>,
}

pub trait ModelCatalogRuntime: Send + Sync {
    fn runtime_ptr_key(&self) -> usize;
    fn refresh(&self, signal: AbortSignal) -> BoxFuture<Result<ModelCatalogRefreshResult>>;
    fn get_available_snapshot(&self) -> Vec<Model>;
    fn get_model(&self, provider: &str, id: &str) -> Option<Model>;
    fn get_error(&self) -> Option<String>;
}

fn abort_error(signal: &AbortSignal) -> Error {
    match signal.reason() {
        Some(reason) => Error::Abort(reason),
        None => Error::Abort(AbortReason::abort()),
    }
}

async fn race_with_abort<T, F>(operation: F, signal: Option<&AbortSignal>) -> Result<T>
where
    F: std::future::Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let Some(signal) = signal else {
        return operation.await;
    };
    if signal.aborted() {
        tokio::spawn(async move {
            let _ = operation.await;
        });
        return Err(abort_error(signal));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = tx.send(operation.await);
    });
    tokio::select! {
        biased;
        _ = signal.cancelled() => Err(abort_error(signal)),
        res = rx => match res {
            Ok(inner) => inner,
            Err(_) => Err(Error::msg("aborted task dropped before settling")),
        },
    }
}

struct ActiveModelCatalogRefresh {
    controller: AbortController,
    rx: watch::Receiver<Option<Result<ModelCatalogRefreshResult>>>,
    waiters: AtomicUsize,
}

#[derive(Default)]
struct ModelCatalogRefreshCoordinator {
    active_by_runtime: Mutex<IndexMap<usize, Arc<ActiveModelCatalogRefresh>>>,
}

static COORDINATOR: LazyLock<ModelCatalogRefreshCoordinator> = LazyLock::new(ModelCatalogRefreshCoordinator::default);

impl ModelCatalogRefreshCoordinator {
    async fn refresh(
        &self,
        runtime: Arc<dyn ModelCatalogRuntime>,
        signal: AbortSignal,
    ) -> Result<ModelCatalogRefreshResult> {
        if signal.aborted() {
            return Err(abort_error(&signal));
        }
        let key = runtime.runtime_ptr_key();
        let active = {
            let mut map = self.active_by_runtime.lock().unwrap();
            if let Some(existing) = map.get(&key) {
                Arc::clone(existing)
            } else {
                let controller = AbortController::new();
                let op_signal = controller.signal();
                let (tx, rx) = watch::channel(None);
                let created = Arc::new(ActiveModelCatalogRefresh {
                    controller,
                    rx,
                    waiters: AtomicUsize::new(0),
                });
                map.insert(key, Arc::clone(&created));

                let created_c = Arc::clone(&created);
                let op = runtime.refresh(op_signal.clone());
                tokio::spawn(async move {
                    let result = race_with_abort(op, Some(&op_signal)).await;
                    let _ = tx.send(Some(result));
                    let mut guard = COORDINATOR.active_by_runtime.lock().unwrap();
                    if guard.get(&key).is_some_and(|e| Arc::ptr_eq(e, &created_c)) {
                        guard.shift_remove(&key);
                    }
                });
                created
            }
        };

        active.waiters.fetch_add(1, Ordering::SeqCst);
        let mut rx = active.rx.clone();
        let wait_fut = async move {
            loop {
                if let Some(ref res) = *rx.borrow_and_update() {
                    return match res {
                        Ok(val) => Ok(val.clone()),
                        Err(err) => Err(Error::msg(err.to_string())),
                    };
                }
                if rx.changed().await.is_err() {
                    return Err(Error::msg("refresh coordinator channel closed"));
                }
            }
        };
        let result = race_with_abort(wait_fut, Some(&signal)).await;

        let remaining = active.waiters.fetch_sub(1, Ordering::SeqCst).saturating_sub(1);
        if remaining == 0 {
            let still_active = {
                let guard = self.active_by_runtime.lock().unwrap();
                guard.get(&key).is_some_and(|e| Arc::ptr_eq(e, &active))
            };
            if still_active {
                active.controller.abort(None);
            }
        }
        result
    }
}

/// Share concurrent interactive all-catalog refreshes while keeping each caller's cancellation independent.
pub async fn refresh_model_catalogs(
    model_runtime: Arc<dyn ModelCatalogRuntime>,
    signal: AbortSignal,
) -> Result<ModelCatalogRefreshResult> {
    COORDINATOR.refresh(model_runtime, signal).await
}

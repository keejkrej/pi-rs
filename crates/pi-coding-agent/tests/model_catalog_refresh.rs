//! Port of packages/coding-agent/test/model-catalog-refresh.test.ts
//!
//! // WIRE(@tui-w3): replace the local `ModelCatalogRefreshCoordinator` mirror with
//! // `pi_coding_agent::modes::interactive::model_catalog_refresh::refresh_model_catalogs`
//! // once `modes/interactive/model_catalog_refresh.rs` lands.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::models::{ModelsRefreshOptions, ModelsRefreshResult};
use pi_js::Result;
use pi_js::abort::{AbortController, AbortReason, AbortSignal};
use tokio::sync::{Notify, oneshot};

#[async_trait::async_trait]
trait RefreshRuntime: Send + Sync {
    async fn refresh(&self, options: Option<ModelsRefreshOptions>) -> Result<ModelsRefreshResult>;
}

struct ActiveRefresh {
    controller: Arc<AbortController>,
    result_slot: Arc<Mutex<Option<std::result::Result<ModelsRefreshResult, String>>>>,
    done: Arc<Notify>,
    waiters: AtomicUsize,
}

#[derive(Clone, Default)]
struct Coordinator {
    active: Arc<Mutex<IndexMap<usize, Arc<ActiveRefresh>>>>,
}

impl Coordinator {
    async fn refresh_model_catalogs(
        &self,
        runtime_id: usize,
        runtime: Arc<dyn RefreshRuntime>,
        signal: AbortSignal,
    ) -> Result<ModelsRefreshResult> {
        if signal.aborted() {
            return Err(pi_js::Error::Abort(signal.reason().unwrap_or_else(AbortReason::abort)));
        }

        let active = {
            let mut map = self.active.lock().unwrap();
            if let Some(existing) = map.get(&runtime_id).cloned() {
                existing.waiters.fetch_add(1, Ordering::SeqCst);
                existing
            } else {
                let controller = Arc::new(AbortController::new());
                let created = Arc::new(ActiveRefresh {
                    controller: controller.clone(),
                    result_slot: Arc::new(Mutex::new(None)),
                    done: Arc::new(Notify::new()),
                    waiters: AtomicUsize::new(1),
                });
                map.insert(runtime_id, created.clone());

                let map_arc = self.active.clone();
                let created_task = created.clone();
                let ctrl_sig = controller.signal();
                tokio::spawn(async move {
                    let op = runtime.refresh(Some(ModelsRefreshOptions {
                        allow_network: None,
                        providers: None,
                        force: None,
                        signal: Some(ctrl_sig.clone()),
                    }));
                    let outcome = tokio::select! {
                        biased;
                        () = ctrl_sig.cancelled() => Err("AbortError".to_owned()),
                        res = op => match res {
                            Ok(r) => Ok(r),
                            Err(e) => Err(e.to_string()),
                        }
                    };
                    *created_task.result_slot.lock().unwrap() = Some(outcome);
                    {
                        let mut map = map_arc.lock().unwrap();
                        if map.get(&runtime_id).is_some_and(|e| Arc::ptr_eq(e, &created_task)) {
                            map.shift_remove(&runtime_id);
                        }
                    }
                    created_task.done.notify_waiters();
                });
                created
            }
        };

        struct WaiterGuard {
            runtime_id: usize,
            active: Arc<ActiveRefresh>,
            map: Arc<Mutex<IndexMap<usize, Arc<ActiveRefresh>>>>,
        }
        impl Drop for WaiterGuard {
            fn drop(&mut self) {
                let remaining = self.active.waiters.fetch_sub(1, Ordering::SeqCst) - 1;
                if remaining == 0 {
                    let is_current = {
                        let map = self.map.lock().unwrap();
                        map.get(&self.runtime_id).is_some_and(|e| Arc::ptr_eq(e, &self.active))
                    };
                    if is_current {
                        self.active.controller.abort(None);
                    }
                }
            }
        }

        let _waiter_guard = WaiterGuard {
            runtime_id,
            active: active.clone(),
            map: self.active.clone(),
        };

        loop {
            let notified = active.done.notified();
            {
                let slot = active.result_slot.lock().unwrap();
                if let Some(ref res) = *slot {
                    return match res {
                        Ok(r) => Ok(ModelsRefreshResult {
                            aborted: r.aborted,
                            errors: IndexMap::new(),
                        }),
                        Err(msg) => Err(pi_js::Error::msg(msg.clone())),
                    };
                }
            }
            tokio::select! {
                biased;
                () = signal.cancelled() => {
                    return Err(pi_js::Error::Abort(
                        signal.reason().unwrap_or_else(AbortReason::abort),
                    ));
                }
                () = notified => {}
            }
        }
    }
}

type RefreshFactory = Box<
    dyn Fn(Option<ModelsRefreshOptions>) -> Pin<Box<dyn Future<Output = Result<ModelsRefreshResult>> + Send>>
        + Send
        + Sync,
>;

struct MockRuntime {
    calls: AtomicUsize,
    signals: Mutex<Vec<AbortSignal>>,
    handler: RefreshFactory,
}

#[async_trait::async_trait]
impl RefreshRuntime for MockRuntime {
    async fn refresh(&self, options: Option<ModelsRefreshOptions>) -> Result<ModelsRefreshResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(sig) = options.as_ref().and_then(|o| o.signal.as_ref()) {
            self.signals.lock().unwrap().push(sig.clone());
        }
        (self.handler)(options).await
    }
}

#[tokio::test]
async fn shares_one_runtime_refresh_between_concurrent_callers() {
    let (tx, rx) = oneshot::channel::<()>();
    let rx = Arc::new(tokio::sync::Mutex::new(Some(rx)));
    let runtime = Arc::new(MockRuntime {
        calls: AtomicUsize::new(0),
        signals: Mutex::new(Vec::new()),
        handler: Box::new(move |_| {
            let rx = rx.clone();
            Box::pin(async move {
                let taken = rx.lock().await.take();
                if let Some(r) = taken {
                    let _ = r.await;
                }
                Ok(ModelsRefreshResult {
                    aborted: false,
                    errors: IndexMap::new(),
                })
            })
        }),
    });

    let coordinator = Coordinator::default();
    let c1 = AbortController::new();
    let c2 = AbortController::new();

    let f1 = coordinator.refresh_model_catalogs(1, runtime.clone(), c1.signal());
    let f2 = coordinator.refresh_model_catalogs(1, runtime.clone(), c2.signal());

    let (r1, r2) = {
        let mut f1 = Box::pin(f1);
        let mut f2 = Box::pin(f2);
        let polled = futures::future::join(
            futures::future::poll_fn(|cx| std::task::Poll::Ready(f1.as_mut().poll(cx))),
            futures::future::poll_fn(|cx| std::task::Poll::Ready(f2.as_mut().poll(cx))),
        )
        .await;
        assert!(polled.0.is_pending());
        assert!(polled.1.is_pending());
        tokio::task::yield_now().await;
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        let _ = tx.send(());
        futures::future::join(f1, f2).await
    };

    assert!(!r1.unwrap().aborted);
    assert!(!r2.unwrap().aborted);
}

#[tokio::test]
async fn keeps_shared_refresh_alive_when_one_caller_stops_waiting() {
    let (tx, rx) = oneshot::channel::<()>();
    let rx = Arc::new(tokio::sync::Mutex::new(Some(rx)));
    let runtime = Arc::new(MockRuntime {
        calls: AtomicUsize::new(0),
        signals: Mutex::new(Vec::new()),
        handler: Box::new(move |_| {
            let rx = rx.clone();
            Box::pin(async move {
                if let Some(r) = rx.lock().await.take() {
                    let _ = r.await;
                }
                Ok(ModelsRefreshResult {
                    aborted: false,
                    errors: IndexMap::new(),
                })
            })
        }),
    });

    let coordinator = Coordinator::default();
    let c1 = AbortController::new();
    let c2 = AbortController::new();

    let coord1 = coordinator.clone();
    let rt1 = runtime.clone();
    let sig1 = c1.signal();
    let h1 = tokio::spawn(async move { coord1.refresh_model_catalogs(1, rt1, sig1).await });

    let coord2 = coordinator.clone();
    let rt2 = runtime.clone();
    let sig2 = c2.signal();
    let h2 = tokio::spawn(async move { coord2.refresh_model_catalogs(1, rt2, sig2).await });

    tokio::task::yield_now().await;
    c1.abort(None);
    let r1 = h1.await.unwrap();
    assert!(matches!(r1, Err(pi_js::Error::Abort(_))));
    assert!(!runtime.signals.lock().unwrap()[0].aborted());

    let _ = tx.send(());
    let r2 = h2.await.unwrap().unwrap();
    assert!(!r2.aborted);
}

#[tokio::test]
async fn aborts_an_abandoned_refresh_and_allows_later_refresh_to_start() {
    let runtime = Arc::new(MockRuntime {
        calls: AtomicUsize::new(0),
        signals: Mutex::new(Vec::new()),
        handler: Box::new(|_| Box::pin(futures::future::pending())),
    });

    let coordinator = Coordinator::default();
    let c1 = AbortController::new();
    let coord1 = coordinator.clone();
    let rt1 = runtime.clone();
    let sig1 = c1.signal();
    let h1 = tokio::spawn(async move { coord1.refresh_model_catalogs(1, rt1, sig1).await });

    tokio::task::yield_now().await;
    c1.abort(None);
    let r1 = h1.await.unwrap();
    assert!(matches!(r1, Err(pi_js::Error::Abort(_))));

    tokio::task::yield_now().await;
    assert!(runtime.signals.lock().unwrap()[0].aborted());

    let c2 = AbortController::new();
    let coord2 = coordinator.clone();
    let rt2 = runtime.clone();
    let sig2 = c2.signal();
    let h2 = tokio::spawn(async move { coord2.refresh_model_catalogs(1, rt2, sig2).await });
    tokio::task::yield_now().await;
    assert_eq!(runtime.calls.load(Ordering::SeqCst), 2);
    c2.abort(None);
    let r2 = h2.await.unwrap();
    assert!(matches!(r2, Err(pi_js::Error::Abort(_))));
}

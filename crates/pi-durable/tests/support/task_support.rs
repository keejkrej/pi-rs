//! Port of packages/durable/test/task-support.ts
#![allow(dead_code)]

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_ai::create_models;
use pi_js::BoxFuture;
use pi_js::abort::{AbortReason, AbortSignal};
use tokio::sync::Notify;

use pi_durable::harness::define::define_extension;
use pi_durable::harness::harness::Harness;
use pi_durable::harness::registry::{Registry, RegistryImpl, create_registry};
use pi_durable::harness::types::{
    Extension, HarnessOptions, HarnessSettings, HarnessSettingsSnapshot, NowFn, RegistryReader, RegistrySnapshot,
};
use pi_durable::tasks::AnyTask;
use pi_durable::types::{NextTaskState, Storage, TaskOutcome};

use super::session_support::{context, flush};

fn clone_js_error(err: &pi_js::Error) -> pi_js::Error {
    match err {
        pi_js::Error::Js(j) => match &j.code {
            Some(code) => pi_js::Error::js_with_code(&j.name, &j.message, code),
            None => pi_js::Error::js(&j.name, &j.message),
        },
        pi_js::Error::Abort(reason) => pi_js::Error::Abort(reason.clone()),
        pi_js::Error::Node(node) => pi_js::Error::Node(node.clone()),
        pi_js::Error::Typed(name, inner) => pi_js::Error::js(name, inner.to_string()),
        pi_js::Error::Exit(code) => pi_js::Error::Exit(*code),
    }
}

struct DeferredInner<T> {
    result: Mutex<Option<pi_js::Result<T>>>,
    notify: Notify,
}

/// Promise-like manual gate matching TS `Deferred<T>`.
#[derive(Clone)]
pub struct Deferred<T = ()> {
    inner: Arc<DeferredInner<T>>,
}

impl<T: Clone + Send + Sync + 'static> Deferred<T> {
    pub fn resolve(&self, value: T) {
        let mut guard = self.inner.result.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = Some(Ok(value));
            drop(guard);
            self.inner.notify.notify_waiters();
        }
    }

    pub fn reject(&self, error: pi_js::Error) {
        let mut guard = self.inner.result.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = Some(Err(error));
            drop(guard);
            self.inner.notify.notify_waiters();
        }
    }

    pub fn is_settled(&self) -> bool {
        self.inner.result.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    pub async fn wait(&self) -> pi_js::Result<T> {
        loop {
            let notified = self.inner.notify.notified();
            {
                let guard = self.inner.result.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(res) = guard.as_ref() {
                    return match res {
                        Ok(val) => Ok(val.clone()),
                        Err(err) => Err(clone_js_error(err)),
                    };
                }
            }
            notified.await;
        }
    }

    pub fn promise(&self) -> BoxFuture<pi_js::Result<T>> {
        let clone = self.clone();
        Box::pin(async move { clone.wait().await })
    }
}

pub fn deferred<T: Clone + Send + Sync + 'static>() -> Deferred<T> {
    Deferred {
        inner: Arc::new(DeferredInner {
            result: Mutex::new(None),
            notify: Notify::new(),
        }),
    }
}

/// Reject with the signal's reason once it aborts; for handlers that block until cancelled.
pub async fn aborted<T>(signal: &AbortSignal) -> pi_js::Result<T> {
    if signal.aborted() {
        return Err(pi_js::Error::Abort(signal.reason().unwrap_or_else(AbortReason::abort)));
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<AbortReason>();
    let tx = Mutex::new(Some(tx));
    let _guard = signal.on_abort(move |reason| {
        if let Some(sender) = tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = sender.send(reason.clone());
        }
    });
    if signal.aborted() {
        return Err(pi_js::Error::Abort(signal.reason().unwrap_or_else(AbortReason::abort)));
    }
    let reason = rx
        .await
        .unwrap_or_else(|_| signal.reason().unwrap_or_else(AbortReason::abort));
    Err(pi_js::Error::Abort(reason))
}

/// Flush macrotask turns until `check` holds.
pub async fn eventually<F, Fut>(mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    for _ in 0..200 {
        if check().await {
            return;
        }
        flush().await;
    }
    panic!("Condition was not reached");
}

/// Synchronous predicate variant of [`eventually`].
pub async fn eventually_sync<F: FnMut() -> bool>(mut check: F) {
    eventually(|| {
        let ok = check();
        std::future::ready(ok)
    })
    .await;
}

struct TrackedInner<T> {
    done: AtomicBool,
    result: Mutex<Option<T>>,
    notify: Notify,
}

/// Eagerly spawned future handle whose settlement can be probed without losing its result.
pub struct Tracked<T> {
    inner: Arc<TrackedInner<T>>,
}

impl<T: Send + 'static> Tracked<T> {
    pub fn is_settled(&self) -> bool {
        self.inner.done.load(Ordering::Acquire)
    }

    pub async fn wait(self) -> T {
        loop {
            let notified = self.inner.notify.notified();
            if self.inner.done.load(Ordering::Acquire) {
                return self
                    .inner
                    .result
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .expect("tracked future settled without output");
            }
            notified.await;
        }
    }
}

impl<T: Send + 'static> IntoFuture for Tracked<T> {
    type Output = T;
    type IntoFuture = Pin<Box<dyn Future<Output = T> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.wait())
    }
}

/// Start `future` immediately (like a JS `Promise`) so `settled(&tracked)` can observe it.
pub fn track<F>(future: F) -> Tracked<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let inner = Arc::new(TrackedInner {
        done: AtomicBool::new(false),
        result: Mutex::new(None),
        notify: Notify::new(),
    });
    let inner_task = Arc::clone(&inner);
    tokio::spawn(async move {
        let value = future.await;
        *inner_task.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
        inner_task.done.store(true, Ordering::Release);
        inner_task.notify.notify_waiters();
    });
    Tracked { inner }
}

/// Whether `tracked` has settled after pending work flushes.
pub async fn settled<T: Send + 'static>(tracked: &Tracked<T>) -> bool {
    flush().await;
    tracked.is_settled()
}

#[derive(Default)]
pub struct OpenTasksOptions {
    pub registry: Option<Arc<RegistryImpl>>,
    pub now: Option<NowFn>,
    pub settings: Option<HarnessSettingsSnapshot>,
}

pub struct OpenedTasks {
    pub harness: Harness,
    pub registry: Arc<RegistryImpl>,
    pub reports: Arc<Mutex<Vec<pi_js::Error>>>,
}

/// Open a Harness whose registry holds `tasks`; failures passed to `on_report` are collected.
pub async fn open_tasks(
    storage: Arc<dyn Storage>,
    tasks: Vec<AnyTask>,
    options: OpenTasksOptions,
) -> pi_js::Result<OpenedTasks> {
    let registry = options.registry.unwrap_or_else(create_registry);
    if !tasks.is_empty() {
        registry.install(define_extension(Extension {
            name: "tasks".to_owned(),
            tasks,
            ..Default::default()
        }))?;
    }
    let reports: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let reports_cb = Arc::clone(&reports);
    let settings: Option<Arc<dyn HarnessSettings>> = options.settings.map(|s| Arc::new(s) as Arc<dyn HarnessSettings>);
    let harness = Harness::open(
        storage,
        HarnessOptions {
            models: create_models(None),
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings,
            env: None,
            conversation_created: None,
            now: options.now,
            on_report: Some(Arc::new(move |err| {
                reports_cb.lock().unwrap_or_else(|e| e.into_inner()).push(err);
            })),
        },
        context(),
    )
    .await?;
    Ok(OpenedTasks {
        harness,
        registry,
        reports,
    })
}

/// Next state that completes a task with `result`.
pub fn completed<S, R>(result: R) -> NextTaskState<S, R> {
    NextTaskState::Terminal {
        outcome: TaskOutcome::Completed { result },
    }
}

/// Next state that aborts a task.
pub fn aborted_with<S, R>(reason: impl Into<String>) -> NextTaskState<S, R> {
    NextTaskState::Terminal {
        outcome: TaskOutcome::Aborted {
            reason: Some(reason.into()),
            result: None,
        },
    }
}

/// Registry reader that counts live subscriptions.
pub struct CountingReader {
    inner: Arc<dyn RegistryReader>,
    count: Arc<AtomicUsize>,
}

impl CountingReader {
    pub fn subscriptions(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl RegistryReader for CountingReader {
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        self.inner.snapshot()
    }

    fn subscribe(&self, listener: Arc<dyn Fn() + Send + Sync>) -> pi_js::Unsubscribe {
        self.count.fetch_add(1, Ordering::SeqCst);
        let unsubscribe = self.inner.subscribe(listener);
        let count = Arc::clone(&self.count);
        let active = AtomicBool::new(true);
        pi_js::Unsubscribe::new(move || {
            if active.swap(false, Ordering::SeqCst) {
                count.fetch_sub(1, Ordering::SeqCst);
                unsubscribe.call();
            }
        })
    }
}

/// Wrap `registry` in a [`CountingReader`].
pub fn counting_reader(registry: Arc<dyn RegistryReader>) -> Arc<CountingReader> {
    Arc::new(CountingReader {
        inner: registry,
        count: Arc::new(AtomicUsize::new(0)),
    })
}

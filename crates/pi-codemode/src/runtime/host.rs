//! Port of packages/codemode/src/runtime/host.ts

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use futures::FutureExt;

use crate::runtime::execution::{
    CodemodeSandboxBase, RawVmMessage, VmChannel, VmEvents, VmLauncher, error_message, lock_unpoisoned, wasm_cache_cell,
};
use crate::runtime::protocol::{HostToWorkerMessage, VmStartData, WorkerData, WorkerToHostMessage};
use crate::runtime::worker;
use crate::types::{
    CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeResult, CodemodeRun, CodemodeRunOptions,
    CodemodeSandboxBaseOptions, CodemodeSandboxOptions, CodemodeTool, CodemodeWorkerHooks,
};

struct WorkerChannel {
    stopped: AtomicBool,
    interrupt: Arc<AtomicI32>,
    to_worker: Mutex<Option<std::sync::mpsc::Sender<HostToWorkerMessage>>>,
}

impl VmChannel for WorkerChannel {
    fn post(&self, message: HostToWorkerMessage) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        if let Some(ref tx) = *lock_unpoisoned(&self.to_worker) {
            let _ = tx.send(message);
        }
    }

    fn stop(&self) -> pi_js::BoxFuture<()> {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            self.interrupt.store(1, Ordering::Release);
            *lock_unpoisoned(&self.to_worker) = None;
        }
        Box::pin(async {})
    }
}

#[derive(Clone)]
struct ReleaseHandle(Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>);

impl ReleaseHandle {
    fn new(release: Option<Box<dyn FnOnce() + Send>>) -> Self {
        Self(Arc::new(Mutex::new(release)))
    }

    fn release(&self) {
        if let Some(cb) = lock_unpoisoned(&self.0).take() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cb));
        }
    }
}

struct WorkerCleanupGuard {
    release: ReleaseHandle,
    exited: Option<tokio::sync::watch::Sender<bool>>,
}

impl Drop for WorkerCleanupGuard {
    fn drop(&mut self) {
        self.release.release();
        if let Some(tx) = self.exited.take() {
            let _ = tx.send(true);
        }
    }
}

/// Returns true for `--watch` dependency reports that must be ignored on raw worker channels (#10725).
pub fn is_watch_worker_message(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .is_some_and(|obj| obj.contains_key("watch:import") || obj.contains_key("watch:require"))
}

fn worker_launcher(wasm: Option<crate::types::CodemodeWasmInput>, hooks: CodemodeWorkerHooks) -> VmLauncher {
    let wasm_cache = Arc::new(wasm_cache_cell(wasm));

    Arc::new(move |data: VmStartData, events: VmEvents| {
        let wasm_cache = Arc::clone(&wasm_cache);
        let hooks = hooks.clone();
        Box::pin(async move {
            let module = wasm_cache
                .get()
                .await
                .map_err(|e| pi_js::Error::msg(format!("Failed to load QuickJS: {}", error_message(&e))))?;

            let release_handle = ReleaseHandle::new(match hooks.acquire_worker {
                Some(ref acquire) => Some(acquire()?),
                None => None,
            });
            let thread_release = release_handle.clone();

            let interrupt = Arc::new(AtomicI32::new(0));
            let worker_data = WorkerData {
                code: data.code,
                tools: data.tools,
                globals: data.globals,
                wasm: module,
                memory_limit_bytes: data.memory_limit_bytes,
                store: data.store,
                deterministic: data.deterministic,
                interrupt: Arc::clone(&interrupt),
                tz: pi_js::env::var("TZ"),
            };

            let (to_worker_tx, from_host_rx) = std::sync::mpsc::channel::<HostToWorkerMessage>();
            let (to_host_tx, from_worker_rx) = std::sync::mpsc::channel::<WorkerToHostMessage>();
            let (exited_tx, mut exited_rx) = tokio::sync::watch::channel(false);

            let worker_panicked = Arc::new(AtomicBool::new(false));
            let worker_panicked_inner = Arc::clone(&worker_panicked);

            let spawn_res = std::thread::Builder::new()
                .name("pi-codemode-worker".to_string())
                .stack_size(4 * 1024 * 1024)
                .spawn(move || {
                    let _cleanup_guard = WorkerCleanupGuard {
                        release: thread_release,
                        exited: Some(exited_tx),
                    };
                    let run_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        worker::run(worker_data, from_host_rx, to_host_tx);
                    }));
                    if run_res.is_err() {
                        worker_panicked_inner.store(true, Ordering::Release);
                    }
                });

            if let Err(err) = spawn_res {
                release_handle.release();
                return Err(pi_js::Error::msg(format!("Failed to start worker: {err}")));
            }

            let channel = Arc::new(WorkerChannel {
                stopped: AtomicBool::new(false),
                interrupt,
                to_worker: Mutex::new(Some(to_worker_tx)),
            });

            let exited_fut: pi_js::BoxFuture<()> = Box::pin(async move {
                let _ = exited_rx.wait_for(|&done| done).await;
            });

            if let Some(ref on_started) = hooks.on_worker_started
                && let Err(err) = on_started(exited_fut)
            {
                channel.stop().await;
                return Err(err);
            }

            let (fwd_tx, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerToHostMessage>();
            tokio::task::spawn_blocking(move || {
                while let Ok(msg) = from_worker_rx.recv() {
                    if fwd_tx.send(msg).is_err() {
                        break;
                    }
                }
            });

            let pump_channel = Arc::clone(&channel);
            let pump_events = events;
            let settled = Arc::new(AtomicBool::new(false));
            tokio::spawn(async move {
                while let Some(msg) = fwd_rx.recv().await {
                    if pump_channel.stopped.load(Ordering::Acquire) {
                        break;
                    }
                    if matches!(
                        msg,
                        WorkerToHostMessage::Done { .. } | WorkerToHostMessage::Crash { .. }
                    ) {
                        settled.store(true, Ordering::Release);
                    }
                    (pump_events.message)(RawVmMessage::Typed(msg));
                }
                if !pump_channel.stopped.load(Ordering::Acquire) && !settled.load(Ordering::Acquire) {
                    let code = i32::from(worker_panicked.load(Ordering::Acquire));
                    (pump_events.failure)(CodemodeError {
                        kind: CodemodeErrorKind::Sandbox,
                        name: None,
                        message: format!("Worker exited with code {code} before the script settled"),
                        stack: None,
                    });
                }
            });

            Ok(channel as Arc<dyn VmChannel>)
        })
        .boxed()
    })
}

/// Runs JavaScript in a QuickJS VM (a separate wasm instance) inside a worker thread.
#[derive(Clone, Debug)]
pub struct CodemodeSandbox {
    base: CodemodeSandboxBase,
}

impl CodemodeSandbox {
    pub fn new(options: Option<CodemodeSandboxOptions>) -> pi_js::Result<Self> {
        Self::with_worker_hooks(options, CodemodeWorkerHooks::default())
    }

    pub fn with_worker_hooks(
        options: Option<CodemodeSandboxOptions>,
        hooks: CodemodeWorkerHooks,
    ) -> pi_js::Result<Self> {
        let options = options.unwrap_or_default();
        let _worker_url = options.worker_url.as_ref();
        let base_opts = CodemodeSandboxBaseOptions::from(&options);
        let launcher = worker_launcher(options.wasm, hooks);
        Ok(Self {
            base: CodemodeSandboxBase::new(base_opts, launcher)?,
        })
    }

    /// Returns an error if a tool with the same name is already registered.
    pub fn register_tool(&self, tool: CodemodeTool) -> pi_js::Result<()> {
        self.base.register_tool(tool)
    }

    pub fn unregister_tool(&self, name: &str) -> bool {
        self.base.unregister_tool(name)
    }

    pub fn tools(&self) -> Vec<CodemodeTool> {
        self.base.tools()
    }

    pub fn globals(&self) -> Vec<CodemodeTool> {
        self.base.globals()
    }

    /// `code` is an async function body: `return` and top-level `await` work.
    /// Never fails for script errors; those come back as `CodemodeResult::Failure`.
    /// Rejects with `Sandbox is closed` when [`Self::close`] has been called.
    pub async fn execute(&self, code: &str, options: Option<CodemodeExecuteOptions>) -> pi_js::Result<CodemodeResult> {
        self.base.execute(code, options).await
    }

    /// Run `code` deterministically, with the caller answering tool and global calls via
    /// [`CodemodeRun::deliver`].
    pub fn start(&self, code: &str, options: CodemodeRunOptions) -> pi_js::Result<CodemodeRun> {
        self.base.start(code, options)
    }

    /// Aborts in-flight executions (they resolve with `kind: "aborted"`) and rejects new ones.
    pub async fn close(&self) {
        self.base.close().await;
    }
}

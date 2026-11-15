//! Port of packages/codemode/src/runtime/host.ts

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use indexmap::{IndexMap, IndexSet};
use serde_json::Value;
use tokio::sync::watch;

use crate::identifier::to_codemode_identifier;
use crate::runtime::protocol::{
    HostToWorkerMessage, WorkerCallTarget, WorkerData, WorkerGlobalSpec, WorkerToHostMessage, WorkerToolSpec,
};
use crate::runtime::worker;
use crate::types::{
    CodemodeCall, CodemodeCallStatus, CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeOutputItem,
    CodemodeResult, CodemodeSandboxOptions, CodemodeStoreWrites, CodemodeTool, CodemodeToolContext, CodemodeWasmInput,
};
use crate::wasm::{CodemodeWasmModule, load_quick_js_wasm};
use pi_js::abort::{AbortController, AbortReason, AbortSignal, ListenerGuard};

const DEFAULT_TIMEOUT_MS: f64 = 300_000.0;
const RESERVED_GLOBALS: &[&str] = &[
    "tools",
    "ALL_TOOLS",
    "console",
    "text",
    "image",
    "exit",
    "globalThis",
    "store",
    "load",
];

fn lock_unpoisoned<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn error_message(error: &pi_js::Error) -> String {
    error.message().to_string()
}

fn serialize_store(store: Option<&IndexMap<String, Value>>) -> IndexMap<String, String> {
    let Some(store) = store else {
        return IndexMap::new();
    };
    let mut out = IndexMap::with_capacity(store.len());
    for (key, value) in store {
        out.insert(key.clone(), pi_js::json::stringify(value));
    }
    out
}

fn parse_store_writes(json: &str) -> pi_js::Result<CodemodeStoreWrites> {
    if json.is_empty() {
        return Ok(CodemodeStoreWrites::default());
    }
    let parsed = pi_js::json::parse(json)?;
    let entries = parsed
        .as_array()
        .ok_or_else(|| pi_js::Error::msg("Expected array for store writes"))?;
    let mut set = IndexMap::new();
    let mut delete = Vec::new();
    for entry in entries {
        let tuple = entry
            .as_array()
            .ok_or_else(|| pi_js::Error::msg("Expected tuple entry in store writes"))?;
        let key = tuple
            .first()
            .and_then(Value::as_str)
            .ok_or_else(|| pi_js::Error::msg("Expected string key in store writes"))?;
        match tuple.get(1) {
            Some(val_json_val) => {
                let val_json = val_json_val
                    .as_str()
                    .ok_or_else(|| pi_js::Error::msg("Expected JSON string value in store writes"))?;
                let val = pi_js::json::parse(val_json)?;
                set.insert(key.to_string(), val);
            }
            None => {
                delete.push(key.to_string());
            }
        }
    }
    Ok(CodemodeStoreWrites { set, r#delete: delete })
}

fn default_worker_url() -> String {
    "worker.js".to_string()
}

/// One in-flight host call. `record` is the same object as the entry in `calls` (globals have none).
struct PendingCall {
    record: Option<Arc<Mutex<CodemodeCall>>>,
    started_at: f64,
    controller: AbortController,
}

struct ExecutionOptions {
    code: String,
    tools: IndexMap<String, CodemodeTool>,
    globals: IndexMap<String, CodemodeTool>,
    timeout_ms: f64,
    signal: Option<AbortSignal>,
    memory_limit_bytes: Option<f64>,
    store: IndexMap<String, String>,
    wasm: pi_js::BoxFuture<pi_js::Result<CodemodeWasmModule>>,
    worker_url: String,
}

/// One script run on its own thread and QuickJS VM. A fresh thread per run keeps
/// termination simple: a runaway script, including one that only spins the
/// microtask queue, is stopped and cannot poison a later run.
///
/// // PORT: the Node `Worker` is a dedicated [`std::thread`] running [`crate::runtime::worker::run`].
#[derive(Clone)]
struct Execution {
    inner: Arc<ExecutionInner>,
}

struct ExecutionInner {
    settled_tx: watch::Sender<bool>,
    settled_rx: watch::Receiver<bool>,
    settled_result: Mutex<Option<CodemodeResult>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// One `Int32` the host sets to non-zero before stopping the thread. The VM interrupt handler polls it.
    interrupt: Arc<AtomicI32>,
    to_worker: Mutex<Option<std::sync::mpsc::Sender<HostToWorkerMessage>>>,
    tools: IndexMap<String, CodemodeTool>,
    globals: IndexMap<String, CodemodeTool>,
    signal: Option<AbortSignal>,
    abort_listener: Mutex<Option<ListenerGuard>>,
    timer: Mutex<Option<pi_js::time::Timeout>>,
    output: Mutex<Vec<CodemodeOutputItem>>,
    /// Shared with [`PendingCall::record`] so a status update is visible on the result.
    calls: Mutex<Vec<Arc<Mutex<CodemodeCall>>>>,
    pending: Mutex<IndexMap<i64, PendingCall>>,
    finished: AtomicBool,
}

impl PartialEq for Execution {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Execution {}

impl std::hash::Hash for Execution {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.inner), state);
    }
}

impl Execution {
    fn new(options: ExecutionOptions) -> Self {
        let (settled_tx, settled_rx) = watch::channel(false);
        let exec = Execution {
            inner: Arc::new(ExecutionInner {
                settled_tx,
                settled_rx,
                settled_result: Mutex::new(None),
                worker: Mutex::new(None),
                interrupt: Arc::new(AtomicI32::new(0)),
                to_worker: Mutex::new(None),
                tools: options.tools,
                globals: options.globals,
                signal: options.signal.clone(),
                abort_listener: Mutex::new(None),
                timer: Mutex::new(None),
                output: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
                pending: Mutex::new(IndexMap::new()),
                finished: AtomicBool::new(false),
            }),
        };

        if options.signal.as_ref().is_some_and(|s| s.aborted()) {
            exec.on_abort();
            return exec;
        }

        if let Some(ref signal) = exec.inner.signal {
            let weak = Arc::downgrade(&exec.inner);
            let guard = signal.on_abort(move |_| {
                if let Some(inner) = weak.upgrade() {
                    Execution { inner }.on_abort();
                }
            });
            *lock_unpoisoned(&exec.inner.abort_listener) = Some(guard);
        }

        if options.timeout_ms.is_finite() {
            let delay_ms = if options.timeout_ms <= 0.0 {
                0
            } else {
                options.timeout_ms.min(u64::MAX as f64) as u64
            };
            let weak = Arc::downgrade(&exec.inner);
            let timeout_ms = options.timeout_ms;
            let timer = pi_js::time::set_timeout(delay_ms, move || {
                if let Some(inner) = weak.upgrade() {
                    Execution { inner }.finish(
                        Some(CodemodeError {
                            kind: CodemodeErrorKind::Timeout,
                            name: None,
                            message: format!("Execution timed out after {timeout_ms}ms"),
                            stack: None,
                        }),
                        None,
                        None,
                    );
                }
            });
            *lock_unpoisoned(&exec.inner.timer) = Some(timer);
        }

        let exec_start = exec.clone();
        let wasm_fut = options.wasm;
        let code = options.code;
        let store = options.store;
        let memory_limit_bytes = options.memory_limit_bytes;
        let _worker_url = options.worker_url;
        tokio::spawn(async move {
            match wasm_fut.await {
                Ok(wasm) => {
                    if exec_start.inner.finished.load(Ordering::Acquire) {
                        return;
                    }
                    exec_start.start(code, store, memory_limit_bytes, wasm);
                }
                Err(err) => {
                    exec_start.finish(
                        Some(CodemodeError {
                            kind: CodemodeErrorKind::Sandbox,
                            name: None,
                            message: error_message(&err),
                            stack: None,
                        }),
                        None,
                        None,
                    );
                }
            }
        });

        exec
    }

    fn result(&self) -> pi_js::BoxFuture<CodemodeResult> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let mut rx = inner.settled_rx.clone();
            let _ = rx.wait_for(|&done| done).await;
            lock_unpoisoned(&inner.settled_result)
                .clone()
                .unwrap_or_else(|| CodemodeResult::Failure {
                    ok: false,
                    error: CodemodeError {
                        kind: CodemodeErrorKind::Sandbox,
                        name: None,
                        message: "Execution ended without a result".to_string(),
                        stack: None,
                    },
                    output: Vec::new(),
                    calls: Vec::new(),
                })
        })
    }

    fn abort(&self, message: &str) -> pi_js::BoxFuture<CodemodeResult> {
        self.finish(
            Some(CodemodeError {
                kind: CodemodeErrorKind::Aborted,
                name: None,
                message: if message.is_empty() {
                    "Execution aborted".to_string()
                } else {
                    message.to_string()
                },
                stack: None,
            }),
            None,
            None,
        );
        self.result()
    }

    fn start(
        &self,
        code: String,
        store: IndexMap<String, String>,
        memory_limit_bytes: Option<f64>,
        wasm: CodemodeWasmModule,
    ) {
        if self.inner.finished.load(Ordering::Acquire) {
            return;
        }

        let (to_worker_tx, from_host_rx) = std::sync::mpsc::channel::<HostToWorkerMessage>();
        let (to_host_tx, from_worker_rx) = std::sync::mpsc::channel::<WorkerToHostMessage>();
        *lock_unpoisoned(&self.inner.to_worker) = Some(to_worker_tx);

        let tools: Vec<WorkerToolSpec> = self
            .inner
            .tools
            .values()
            .map(|tool| WorkerToolSpec {
                name: tool.name.clone(),
                js_name: to_codemode_identifier(&tool.name),
                description: tool.description.clone().unwrap_or_default(),
            })
            .collect();
        let globals: Vec<WorkerGlobalSpec> = self
            .inner
            .globals
            .values()
            .map(|global| WorkerGlobalSpec {
                name: global.name.clone(),
                spread: global.spread.unwrap_or(false),
            })
            .collect();

        let data = WorkerData {
            code,
            tools,
            globals,
            wasm,
            memory_limit_bytes,
            store,
            interrupt: Arc::clone(&self.inner.interrupt),
        };

        let spawn_res = std::thread::Builder::new()
            .name("pi-codemode-worker".to_string())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || {
                worker::run(data, from_host_rx, to_host_tx);
            });

        match spawn_res {
            Ok(handle) => {
                *lock_unpoisoned(&self.inner.worker) = Some(handle);
            }
            Err(err) => {
                self.finish(
                    Some(CodemodeError {
                        kind: CodemodeErrorKind::Sandbox,
                        name: None,
                        message: format!("Failed to spawn codemode worker: {err}"),
                        stack: None,
                    }),
                    None,
                    None,
                );
                return;
            }
        }

        if self.inner.finished.load(Ordering::Acquire) {
            self.inner.interrupt.store(1, Ordering::Release);
            *lock_unpoisoned(&self.inner.to_worker) = None;
            return;
        }

        let (fwd_tx, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerToHostMessage>();
        tokio::task::spawn_blocking(move || {
            while let Ok(msg) = from_worker_rx.recv() {
                if fwd_tx.send(msg).is_err() {
                    break;
                }
            }
        });

        let exec = self.clone();
        tokio::spawn(async move {
            while let Some(msg) = fwd_rx.recv().await {
                if exec.inner.finished.load(Ordering::Acquire) {
                    break;
                }
                exec.handle_message(msg);
            }
            if !exec.inner.finished.load(Ordering::Acquire) {
                exec.finish(
                    Some(CodemodeError {
                        kind: CodemodeErrorKind::Sandbox,
                        name: None,
                        message: "Codemode worker exited unexpectedly".to_string(),
                        stack: None,
                    }),
                    None,
                    None,
                );
            }
        });
    }

    fn on_abort(&self) {
        let message = self
            .inner
            .signal
            .as_ref()
            .and_then(|s| s.reason())
            .map(|r| r.message)
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "Execution aborted".to_string());
        self.finish(
            Some(CodemodeError {
                kind: CodemodeErrorKind::Aborted,
                name: None,
                message,
                stack: None,
            }),
            None,
            None,
        );
    }

    fn post(&self, message: HostToWorkerMessage) {
        if self.inner.finished.load(Ordering::Acquire) {
            return;
        }
        let guard = lock_unpoisoned(&self.inner.to_worker);
        if let Some(ref tx) = *guard {
            let _ = tx.send(message);
        }
    }

    fn handle_message(&self, message: WorkerToHostMessage) {
        if self.inner.finished.load(Ordering::Acquire) {
            return;
        }
        match message {
            WorkerToHostMessage::Call { id, target, name, args } => {
                self.handle_call(id, target, name, args);
            }
            WorkerToHostMessage::Output { item } => {
                lock_unpoisoned(&self.inner.output).push(item);
            }
            done @ WorkerToHostMessage::Done { .. } => {
                self.handle_done(&done);
            }
            WorkerToHostMessage::Crash { message } => {
                self.finish(
                    Some(CodemodeError {
                        kind: CodemodeErrorKind::Sandbox,
                        name: None,
                        message,
                        stack: None,
                    }),
                    None,
                    None,
                );
            }
        }
    }

    fn handle_done(&self, message: &WorkerToHostMessage) {
        let WorkerToHostMessage::Done {
            ok,
            value,
            writes,
            error,
        } = message
        else {
            return;
        };

        if !ok {
            let err_obj = error.as_deref().and_then(|raw| pi_js::json::parse(raw).ok());
            let err = match err_obj {
                Some(Value::Object(map)) => CodemodeError {
                    kind: CodemodeErrorKind::Script,
                    name: map
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                    message: map
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Script failed")
                        .to_owned(),
                    stack: map
                        .get("stack")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                },
                _ => CodemodeError {
                    kind: CodemodeErrorKind::Sandbox,
                    name: None,
                    message: error
                        .clone()
                        .unwrap_or_else(|| "Malformed error payload from worker".to_string()),
                    stack: None,
                },
            };
            self.finish(Some(err), None, None);
            return;
        }

        let parsed_value = match value {
            Some(raw) => match pi_js::json::parse(raw) {
                Ok(v) => Some(v),
                Err(err) => {
                    self.finish(
                        Some(CodemodeError {
                            kind: CodemodeErrorKind::Sandbox,
                            name: None,
                            message: format!("Malformed result from worker: {}", error_message(&err)),
                            stack: None,
                        }),
                        None,
                        None,
                    );
                    return;
                }
            },
            None => None,
        };
        self.finish(None, parsed_value, writes.as_deref());
    }

    fn handle_call(&self, id: i64, target: WorkerCallTarget, name: String, args: Option<String>) {
        let is_global = target == WorkerCallTarget::Global;
        let record = if is_global {
            None
        } else {
            let rec = Arc::new(Mutex::new(CodemodeCall {
                name: name.clone(),
                status: CodemodeCallStatus::Ok,
                duration_ms: 0.0,
            }));
            lock_unpoisoned(&self.inner.calls).push(Arc::clone(&rec));
            Some(rec)
        };

        let tool = if is_global {
            self.inner.globals.get(&name).cloned()
        } else {
            self.inner.tools.get(&name).cloned()
        };

        let Some(tool) = tool else {
            let err_msg = format!("Unknown {}: {name}", if is_global { "global" } else { "tool" });
            if let Some(ref rec) = record {
                lock_unpoisoned(rec).status = CodemodeCallStatus::Error;
            }
            self.post(HostToWorkerMessage::Result {
                id,
                ok: false,
                payload: Some(err_msg),
            });
            return;
        };

        let parsed_args = match args {
            Some(ref raw) => match pi_js::json::parse(raw) {
                Ok(v) => Some(v),
                Err(err) => {
                    if let Some(ref rec) = record {
                        lock_unpoisoned(rec).status = CodemodeCallStatus::Error;
                    }
                    self.post(HostToWorkerMessage::Result {
                        id,
                        ok: false,
                        payload: Some(error_message(&err)),
                    });
                    return;
                }
            },
            None => None,
        };

        let controller = AbortController::new();
        let signal = controller.signal();
        let started_at = pi_js::time::performance_now();
        {
            let mut pending = lock_unpoisoned(&self.inner.pending);
            if self.inner.finished.load(Ordering::Acquire) {
                controller.abort(Some(AbortReason::error("AbortError", "Execution ended")));
                if let Some(ref rec) = record {
                    lock_unpoisoned(rec).status = CodemodeCallStatus::Cancelled;
                }
                return;
            }
            pending.insert(
                id,
                PendingCall {
                    record: record.clone(),
                    started_at,
                    controller,
                },
            );
        }

        let exec = self.clone();
        let fut = (tool.execute)(parsed_args, CodemodeToolContext { signal });
        tokio::spawn(async move {
            let result = fut.await;
            let pending_entry = lock_unpoisoned(&exec.inner.pending).shift_remove(&id);
            let Some(_pending_entry) = pending_entry else {
                return;
            };

            let duration_ms = (pi_js::time::performance_now() - started_at).max(0.0);
            match result {
                Ok(opt_val) => {
                    let serialized = opt_val.as_ref().map(pi_js::json::stringify);
                    if let Some(ref rec) = record {
                        let mut r = lock_unpoisoned(rec);
                        r.status = CodemodeCallStatus::Ok;
                        r.duration_ms = duration_ms;
                    }
                    exec.post(HostToWorkerMessage::Result {
                        id,
                        ok: true,
                        payload: serialized,
                    });
                }
                Err(err) => {
                    if let Some(ref rec) = record {
                        let mut r = lock_unpoisoned(rec);
                        r.status = CodemodeCallStatus::Error;
                        r.duration_ms = duration_ms;
                    }
                    exec.post(HostToWorkerMessage::Result {
                        id,
                        ok: false,
                        payload: Some(error_message(&err)),
                    });
                }
            }
        });
    }

    fn finish(&self, error: Option<CodemodeError>, value: Option<Value>, writes: Option<&str>) {
        if self.inner.finished.swap(true, Ordering::AcqRel) {
            return;
        }

        if let Some(timer) = lock_unpoisoned(&self.inner.timer).take() {
            timer.clear();
        }
        if let Some(guard) = lock_unpoisoned(&self.inner.abort_listener).take() {
            guard.remove();
        }

        let pending = std::mem::take(&mut *lock_unpoisoned(&self.inner.pending));
        let ended_at = pi_js::time::performance_now();
        for (_id, entry) in pending {
            entry
                .controller
                .abort(Some(AbortReason::error("AbortError", "Execution ended")));
            if let Some(rec) = entry.record {
                let mut r = lock_unpoisoned(&rec);
                r.status = CodemodeCallStatus::Cancelled;
                r.duration_ms = (ended_at - entry.started_at).max(0.0);
            }
        }

        self.inner.interrupt.store(1, Ordering::Release);
        *lock_unpoisoned(&self.inner.to_worker) = None;
        let worker_handle = lock_unpoisoned(&self.inner.worker).take();
        let writes_owned = writes.map(str::to_owned);
        let inner = Arc::clone(&self.inner);

        tokio::spawn(async move {
            if let Some(handle) = worker_handle {
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = handle.join();
                })
                .await;
            }

            let output = lock_unpoisoned(&inner.output).clone();
            let calls: Vec<CodemodeCall> = lock_unpoisoned(&inner.calls)
                .iter()
                .map(|c| lock_unpoisoned(c).clone())
                .collect();

            let result = match error {
                Some(error) => CodemodeResult::Failure {
                    ok: false,
                    error,
                    output,
                    calls,
                },
                None => match writes_owned.as_deref().map(parse_store_writes).transpose() {
                    Ok(store_writes) => CodemodeResult::Success {
                        ok: true,
                        value,
                        output,
                        calls,
                        store_writes: store_writes.unwrap_or_default(),
                    },
                    Err(err) => CodemodeResult::Failure {
                        ok: false,
                        error: CodemodeError {
                            kind: CodemodeErrorKind::Sandbox,
                            name: None,
                            message: format!("Malformed store writes from worker: {}", error_message(&err)),
                            stack: None,
                        },
                        output,
                        calls,
                    },
                },
            };

            *lock_unpoisoned(&inner.settled_result) = Some(result);
            let _ = inner.settled_tx.send(true);
        });
    }
}

struct CodemodeSandboxInner {
    tools_by_name: Mutex<IndexMap<String, CodemodeTool>>,
    globals_by_name: IndexMap<String, CodemodeTool>,
    timeout_ms: f64,
    memory_limit_bytes: Option<f64>,
    wasm_input: Mutex<Option<CodemodeWasmInput>>,
    wasm_cache: tokio::sync::OnceCell<std::result::Result<CodemodeWasmModule, String>>,
    worker_url: String,
    running: Mutex<IndexSet<Execution>>,
    closed: Mutex<bool>,
}

/// Runs JavaScript in a QuickJS VM (a separate wasm instance) inside a worker
/// thread. The script sees `tools.<name>(args)` for every registered tool, `ALL_TOOLS`,
/// the output helpers `text`, `image`, `exit`, and `console.*`, `store`/`load`, and the
/// configured globals; nothing else (no timers, `fetch`, `process`, `require`, modules).
///
/// Each `execute()` gets its own worker and VM; the sandbox only holds the tool
/// table and defaults. `close()` aborts in-flight executions.
#[derive(Clone)]
pub struct CodemodeSandbox {
    inner: Arc<CodemodeSandboxInner>,
}

impl CodemodeSandbox {
    pub fn new(options: Option<CodemodeSandboxOptions>) -> pi_js::Result<Self> {
        let options = options.unwrap_or_default();
        let mut globals_by_name = IndexMap::<String, CodemodeTool>::new();
        let mut namespace_roots = IndexSet::<String>::new();
        let mut flat_globals = IndexSet::<String>::new();

        if let Some(globals) = options.globals {
            for global in globals {
                if globals_by_name.contains_key(&global.name) {
                    return Err(pi_js::Error::msg(format!("Duplicate global \"{}\"", global.name)));
                }
                if let Some((ns, member)) = global.name.split_once('.') {
                    if !is_identifier(ns) || !is_identifier(member) || member.contains('.') {
                        return Err(pi_js::Error::msg(format!("Invalid global name \"{}\"", global.name)));
                    }
                    if RESERVED_GLOBALS.contains(&ns) {
                        return Err(pi_js::Error::msg(format!(
                            "Global name \"{}\" conflicts with reserved built-in \"{}\"",
                            global.name, ns
                        )));
                    }
                    if flat_globals.contains(ns) {
                        return Err(pi_js::Error::msg(format!(
                            "Global \"{}\" conflicts with namespace \"{}\"",
                            global.name, ns
                        )));
                    }
                    namespace_roots.insert(ns.to_string());
                } else {
                    if !is_identifier(&global.name) {
                        return Err(pi_js::Error::msg(format!("Invalid global name \"{}\"", global.name)));
                    }
                    if RESERVED_GLOBALS.contains(&global.name.as_str()) {
                        return Err(pi_js::Error::msg(format!(
                            "Global name \"{}\" is reserved",
                            global.name
                        )));
                    }
                    if namespace_roots.contains(&global.name) {
                        return Err(pi_js::Error::msg(format!(
                            "Global \"{}\" conflicts with namespace",
                            global.name
                        )));
                    }
                    flat_globals.insert(global.name.clone());
                }
                globals_by_name.insert(global.name.clone(), global);
            }
        }

        let sandbox = CodemodeSandbox {
            inner: Arc::new(CodemodeSandboxInner {
                tools_by_name: Mutex::new(IndexMap::new()),
                globals_by_name,
                timeout_ms: options.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
                memory_limit_bytes: options.memory_limit_bytes,
                wasm_input: Mutex::new(options.wasm),
                wasm_cache: tokio::sync::OnceCell::new(),
                worker_url: options.worker_url.unwrap_or_else(default_worker_url),
                running: Mutex::new(IndexSet::new()),
                closed: Mutex::new(false),
            }),
        };

        if let Some(tools) = options.tools {
            for tool in tools {
                sandbox.register_tool(tool)?;
            }
        }

        Ok(sandbox)
    }

    /// Throws if a tool with the same name is already registered.
    pub fn register_tool(&self, tool: CodemodeTool) -> pi_js::Result<()> {
        let mut map = lock_unpoisoned(&self.inner.tools_by_name);
        if map.contains_key(&tool.name) {
            return Err(pi_js::Error::msg(format!(
                "Tool \"{}\" is already registered",
                tool.name
            )));
        }
        map.insert(tool.name.clone(), tool);
        Ok(())
    }

    pub fn unregister_tool(&self, name: &str) -> bool {
        lock_unpoisoned(&self.inner.tools_by_name).shift_remove(name).is_some()
    }

    pub fn tools(&self) -> Vec<CodemodeTool> {
        lock_unpoisoned(&self.inner.tools_by_name).values().cloned().collect()
    }

    pub fn globals(&self) -> Vec<CodemodeTool> {
        self.inner.globals_by_name.values().cloned().collect()
    }

    /// `code` is an async function body: `return` and top-level `await` work.
    /// Never rejects for script failures; those come back as `{ ok: false }`.
    /// The script can use `store(key, value)` and `load(key)` on `options.store`.
    ///
    /// Rejects with `Sandbox is closed` when [`Self::close`] has been called.
    pub async fn execute(&self, code: &str, options: Option<CodemodeExecuteOptions>) -> pi_js::Result<CodemodeResult> {
        if *lock_unpoisoned(&self.inner.closed) {
            return Err(pi_js::Error::msg("Sandbox is closed"));
        }

        let options = options.unwrap_or_default();
        let inner_wasm = Arc::clone(&self.inner);
        let wasm_fut: pi_js::BoxFuture<pi_js::Result<CodemodeWasmModule>> = Box::pin(async move {
            let res = inner_wasm
                .wasm_cache
                .get_or_init(|| async {
                    let input = lock_unpoisoned(&inner_wasm.wasm_input).take();
                    match input {
                        Some(CodemodeWasmInput::Module(module)) => Ok(module),
                        Some(CodemodeWasmInput::Pending(pending)) => {
                            let fut = lock_unpoisoned(&pending).take();
                            match fut {
                                Some(fut) => fut.await.map_err(|e| error_message(&e)),
                                None => Err("Pending wasm loader future already consumed".to_string()),
                            }
                        }
                        None => load_quick_js_wasm(None).await.map_err(|e| error_message(&e)),
                    }
                })
                .await;
            res.clone().map_err(pi_js::Error::msg)
        });

        let tools = lock_unpoisoned(&self.inner.tools_by_name).clone();
        let execution = Execution::new(ExecutionOptions {
            code: code.to_string(),
            tools,
            globals: self.inner.globals_by_name.clone(),
            timeout_ms: options.timeout_ms.unwrap_or(self.inner.timeout_ms),
            signal: options.signal,
            memory_limit_bytes: self.inner.memory_limit_bytes,
            store: serialize_store(options.store.as_ref()),
            wasm: wasm_fut,
            worker_url: self.inner.worker_url.clone(),
        });

        lock_unpoisoned(&self.inner.running).insert(execution.clone());
        let result = execution.result().await;
        lock_unpoisoned(&self.inner.running).shift_remove(&execution);
        Ok(result)
    }

    /// Aborts in-flight executions (they resolve with `kind: "aborted"`) and rejects new ones.
    pub async fn close(&self) {
        *lock_unpoisoned(&self.inner.closed) = true;
        let active: Vec<Execution> = lock_unpoisoned(&self.inner.running).iter().cloned().collect();
        let futures: Vec<_> = active.iter().map(|e| e.abort("Sandbox closed")).collect();
        for fut in futures {
            fut.await;
        }
    }
}

//! Port of packages/codemode/src/runtime/execution.ts
//!
//! Host side of an execution, shared by every place a VM can run.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::FutureExt;
use indexmap::{IndexMap, IndexSet};
use serde_json::Value;

use crate::identifier::to_codemode_identifier;
use crate::runtime::protocol::{
    DeterministicConfig, HostToWorkerMessage, VmStartData, WorkerCallTarget, WorkerGlobalSpec, WorkerToHostMessage,
    WorkerToolSpec, is_worker_to_host_message,
};
use crate::types::{
    CodemodeCall, CodemodeCallStatus, CodemodeDeclaredGlobal, CodemodeDeclaredTool, CodemodeDuring, CodemodeError,
    CodemodeErrorKind, CodemodeExecuteOptions, CodemodeHostCall, CodemodeOutcome, CodemodeOutputItem, CodemodeResult,
    CodemodeRun, CodemodeRunInner, CodemodeRunOptions, CodemodeSandboxBaseOptions, CodemodeStoreWrites, CodemodeTool,
    CodemodeToolContext, DeliverFn,
};
use pi_js::abort::{AbortController, AbortReason, AbortSignal, ListenerGuard};

pub(crate) const DEFAULT_TIMEOUT_MS: f64 = 300_000.0;

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

pub(crate) fn lock_unpoisoned<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn error_message(error: &pi_js::Error) -> String {
    error.message().to_string()
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

/// A message arriving from a VM launcher (`WorkerToHostMessage` or an arbitrary unvalidated JSON payload).
#[derive(Clone, Debug)]
pub enum RawVmMessage {
    Typed(WorkerToHostMessage),
    Json(Value),
}

impl From<WorkerToHostMessage> for RawVmMessage {
    fn from(value: WorkerToHostMessage) -> Self {
        Self::Typed(value)
    }
}

/// Callbacks a launcher uses to report what its VM does.
#[derive(Clone)]
pub struct VmEvents {
    /// A message from the VM. Validated inside [`Execution`], so launchers can forward anything they receive.
    pub message: Arc<dyn Fn(RawVmMessage) + Send + Sync>,
    /// The VM failed or stopped outside the bridge (for example a worker crash or remote error).
    pub failure: Arc<dyn Fn(CodemodeError) + Send + Sync>,
}

/// The host's handle on a running VM.
pub trait VmChannel: Send + Sync {
    fn post(&self, message: HostToWorkerMessage);
    /// Requests VM shutdown. Called once; may resolve before a worker or remote VM actually exits.
    fn stop(&self) -> pi_js::BoxFuture<()>;
}

/// Starts a VM for one execution. Failures become `sandbox` errors.
pub type VmLauncher =
    Arc<dyn Fn(VmStartData, VmEvents) -> pi_js::BoxFuture<pi_js::Result<Arc<dyn VmChannel>>> + Send + Sync>;

struct BridgeError(String);

fn parse_bridge_json(json: &str, what: &str) -> Result<Value, BridgeError> {
    pi_js::json::parse(json).map_err(|_| BridgeError(format!("{what} is not valid JSON")))
}

fn parse_store_writes(json: &str) -> Result<CodemodeStoreWrites, BridgeError> {
    let entries = parse_bridge_json(json, "store writes")?;
    let Some(arr) = entries.as_array() else {
        return Err(BridgeError("store writes are not an array".to_string()));
    };
    let mut set = IndexMap::new();
    let mut delete = Vec::new();
    for entry in arr {
        let Some(tuple) = entry.as_array() else {
            return Err(BridgeError("store writes contain a malformed entry".to_string()));
        };
        let Some(Value::String(key)) = tuple.first() else {
            return Err(BridgeError("store writes contain a malformed entry".to_string()));
        };
        match tuple.len() {
            1 => delete.push(key.clone()),
            2 => {
                let Some(Value::String(val_json)) = tuple.get(1) else {
                    return Err(BridgeError("store writes contain a malformed entry".to_string()));
                };
                let parsed = parse_bridge_json(
                    val_json,
                    &format!(
                        "store value for {}",
                        pi_js::json::stringify(&Value::String(key.clone()))
                    ),
                )?;
                set.insert(key.clone(), parsed);
            }
            _ => return Err(BridgeError("store writes contain a malformed entry".to_string())),
        }
    }
    Ok(CodemodeStoreWrites { set, r#delete: delete })
}

fn parse_script_error(json: &str) -> Result<CodemodeError, BridgeError> {
    let parsed = parse_bridge_json(json, "script error")?;
    let Some(obj) = parsed.as_object() else {
        return Err(BridgeError("script error is not an object".to_string()));
    };
    let Some(Value::String(message)) = obj.get("message") else {
        return Err(BridgeError("script error is malformed".to_string()));
    };
    let name = match obj.get("name") {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(BridgeError("script error is malformed".to_string())),
    };
    let stack = match obj.get("stack") {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(BridgeError("script error is malformed".to_string())),
    };
    Ok(CodemodeError {
        kind: CodemodeErrorKind::Script,
        name,
        message: message.clone(),
        stack,
    })
}

/// Checks global names: identifiers or `namespace.member`, not shadowing a built-in global or each other.
pub(crate) fn check_declared_globals(globals: &[CodemodeDeclaredGlobal]) -> pi_js::Result<()> {
    let mut names = IndexSet::new();
    let mut namespaces = IndexSet::new();
    for global in globals {
        let parts: Vec<&str> = global.name.split('.').collect();
        if parts.len() > 2 || !parts.iter().all(|part| is_identifier(part)) || RESERVED_GLOBALS.contains(&parts[0]) {
            return Err(pi_js::Error::msg(format!("Invalid global name \"{}\"", global.name)));
        }
        if !names.insert(global.name.clone()) {
            return Err(pi_js::Error::msg(format!(
                "Global \"{}\" is already registered",
                global.name
            )));
        }
        if parts.len() == 2 {
            namespaces.insert(parts[0].to_string());
        }
    }
    for name in namespaces {
        if names.contains(&name) {
            return Err(pi_js::Error::msg(format!(
                "Global \"{name}\" conflicts with the namespace \"{name}\""
            )));
        }
    }
    Ok(())
}

struct PendingCall {
    record: Option<Arc<Mutex<CodemodeCall>>>,
    started_at: f64,
    controller: AbortController,
}

#[derive(Clone)]
struct ExecutionControl {
    on_started: Option<Arc<dyn Fn(String) + Send + Sync>>,
    on_call: Arc<dyn Fn(CodemodeHostCall, CodemodeRun) + Send + Sync>,
    on_idle: Option<Arc<dyn Fn(i64, i64, CodemodeRun) + Send + Sync>>,
    run: CodemodeRun,
}

pub(crate) struct ExecutionOptions {
    pub code: String,
    pub tools: Vec<CodemodeDeclaredTool>,
    pub globals: Vec<CodemodeDeclaredGlobal>,
    pub executors_tools: IndexMap<String, CodemodeTool>,
    pub executors_globals: IndexMap<String, CodemodeTool>,
    pub timeout_ms: f64,
    pub signal: Option<AbortSignal>,
    pub memory_limit_bytes: Option<f64>,
    pub store: IndexMap<String, String>,
    pub deterministic: Option<DeterministicConfig>,
    pub control_on_started: Option<Arc<dyn Fn(String) + Send + Sync>>,
    pub control_on_call: Option<Arc<dyn Fn(CodemodeHostCall, CodemodeRun) + Send + Sync>>,
    pub control_on_idle: Option<Arc<dyn Fn(i64, i64, CodemodeRun) + Send + Sync>>,
    pub launch: VmLauncher,
}

struct ExecutionInner {
    channel: Mutex<Option<Arc<dyn VmChannel>>>,
    unsent: Mutex<Vec<HostToWorkerMessage>>,
    executors_tools: IndexMap<String, CodemodeTool>,
    executors_globals: IndexMap<String, CodemodeTool>,
    control: Mutex<Option<ExecutionControl>>,
    delivered: Mutex<IndexSet<i64>>,
    signal: Option<AbortSignal>,
    abort_listener: Mutex<Option<ListenerGuard>>,
    timer: Mutex<Option<pi_js::time::Timeout>>,
    output: Mutex<Vec<CodemodeOutputItem>>,
    console_indices: Mutex<Vec<usize>>,
    calls: Mutex<Vec<Arc<Mutex<CodemodeCall>>>>,
    pending: Mutex<IndexMap<i64, PendingCall>>,
    finished: Arc<AtomicBool>,
    settled_result: Mutex<Option<CodemodeResult>>,
    settled_tx: tokio::sync::watch::Sender<bool>,
    settled_rx: tokio::sync::watch::Receiver<bool>,
}

#[derive(Clone)]
pub(crate) struct Execution {
    inner: Arc<ExecutionInner>,
}

impl Execution {
    pub fn start_with_run(options: ExecutionOptions) -> (Self, Option<CodemodeRun>) {
        let (settled_tx, settled_rx) = tokio::sync::watch::channel(false);
        let finished_flag = Arc::new(AtomicBool::new(false));
        let exec = Self {
            inner: Arc::new(ExecutionInner {
                channel: Mutex::new(None),
                unsent: Mutex::new(Vec::new()),
                executors_tools: options.executors_tools,
                executors_globals: options.executors_globals,
                control: Mutex::new(None),
                delivered: Mutex::new(IndexSet::new()),
                signal: options.signal.clone(),
                abort_listener: Mutex::new(None),
                timer: Mutex::new(None),
                output: Mutex::new(Vec::new()),
                console_indices: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
                pending: Mutex::new(IndexMap::new()),
                finished: Arc::clone(&finished_flag),
                settled_result: Mutex::new(None),
                settled_tx,
                settled_rx,
            }),
        };

        let run_handle = if let Some(on_call) = options.control_on_call {
            let exec_for_deliver = exec.clone();
            let deliver_fn: DeliverFn =
                Arc::new(move |seq, now, id, outcome| exec_for_deliver.deliver(seq, now, id, outcome));
            let result_fut: futures::future::Shared<pi_js::BoxFuture<CodemodeResult>> = exec.result().shared();
            let run = CodemodeRun::from_parts(Arc::new(CodemodeRunInner {
                finished: Arc::clone(&finished_flag),
                deliver: Mutex::new(Some(deliver_fn)),
                result: result_fut,
            }));
            *lock_unpoisoned(&exec.inner.control) = Some(ExecutionControl {
                on_started: options.control_on_started,
                on_call,
                on_idle: options.control_on_idle,
                run: run.clone(),
            });
            Some(run)
        } else {
            None
        };

        if let Some(ref signal) = exec.inner.signal {
            if signal.aborted() {
                exec.on_abort();
                return (exec, run_handle);
            }
            let weak = Arc::downgrade(&exec.inner);
            let guard = signal.on_abort(move |_| {
                if let Some(inner) = weak.upgrade() {
                    Execution { inner }.on_abort();
                }
            });
            *lock_unpoisoned(&exec.inner.abort_listener) = Some(guard);
        }

        if exec.inner.finished.load(Ordering::Acquire) {
            return (exec, run_handle);
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
                            message: format!(
                                "Execution timed out after {} ms",
                                pi_js::json::number_to_string(timeout_ms)
                            ),
                            stack: None,
                        }),
                        None,
                        None,
                        None,
                    );
                }
            });
            *lock_unpoisoned(&exec.inner.timer) = Some(timer);
        }

        let data = VmStartData {
            code: options.code,
            tools: options
                .tools
                .into_iter()
                .map(|t| WorkerToolSpec {
                    js_name: t.js_name.unwrap_or_else(|| to_codemode_identifier(&t.name)),
                    name: t.name,
                    description: t.description.unwrap_or_default(),
                })
                .collect(),
            globals: options
                .globals
                .into_iter()
                .map(|g| WorkerGlobalSpec {
                    name: g.name,
                    spread: g.spread == Some(true),
                })
                .collect(),
            memory_limit_bytes: options.memory_limit_bytes,
            store: options.store,
            deterministic: options.deterministic,
        };

        let weak_msg = Arc::downgrade(&exec.inner);
        let weak_fail = Arc::downgrade(&exec.inner);
        let events = VmEvents {
            message: Arc::new(move |msg| {
                if let Some(inner) = weak_msg.upgrade() {
                    Execution { inner }.receive(msg);
                }
            }),
            failure: Arc::new(move |error| {
                if let Some(inner) = weak_fail.upgrade() {
                    Execution { inner }.finish(Some(error), None, None, None);
                }
            }),
        };

        let launch_exec = exec.clone();
        let launch_fut = (options.launch)(data, events);
        tokio::spawn(async move {
            match launch_fut.await {
                Ok(channel) => {
                    if launch_exec.inner.finished.load(Ordering::Acquire) {
                        channel.stop().await;
                        return;
                    }
                    *lock_unpoisoned(&launch_exec.inner.channel) = Some(Arc::clone(&channel));
                    let unsent = std::mem::take(&mut *lock_unpoisoned(&launch_exec.inner.unsent));
                    for msg in unsent {
                        channel.post(msg);
                    }
                }
                Err(err) => {
                    launch_exec.finish(
                        Some(CodemodeError {
                            kind: CodemodeErrorKind::Sandbox,
                            name: None,
                            message: error_message(&err),
                            stack: None,
                        }),
                        None,
                        None,
                        None,
                    );
                }
            }
        });

        (exec, run_handle)
    }

    pub fn result(&self) -> pi_js::BoxFuture<CodemodeResult> {
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
                    console_output_indices: Vec::new(),
                    calls: Vec::new(),
                    during: None,
                })
        })
    }

    pub fn abort(&self, message: &str) -> pi_js::BoxFuture<CodemodeResult> {
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
            None,
        );
        self.result()
    }

    fn deliver(&self, seq: i64, now: f64, id: i64, outcome: CodemodeOutcome) -> pi_js::Result<()> {
        if self.inner.finished.load(Ordering::Acquire) {
            return Ok(());
        }
        if !now.is_finite() {
            return Err(pi_js::Error::msg(format!(
                "The clock value of result {seq} is not a finite number"
            )));
        }
        if seq < 1 || lock_unpoisoned(&self.inner.delivered).contains(&seq) {
            return Err(pi_js::Error::msg(format!(
                "Result number {seq} is not a positive integer used once"
            )));
        }
        if !lock_unpoisoned(&self.inner.pending).contains_key(&id) {
            return Err(pi_js::Error::msg(format!("Call {id} is not waiting for a result")));
        }

        // Serialized before anything is recorded, so a value JSON cannot hold leaves the call waiting.
        let (ok, payload, properties) = match &outcome {
            CodemodeOutcome::Ok { value } => (true, value.as_ref().map(pi_js::json::stringify), None),
            CodemodeOutcome::Err { message, properties } => (
                false,
                Some(message.clone()),
                properties.as_ref().map(|p| {
                    pi_js::json::stringify(&Value::Object(p.iter().map(|(k, v)| (k.clone(), v.clone())).collect()))
                }),
            ),
        };

        lock_unpoisoned(&self.inner.delivered).insert(seq);
        let pending_entry = lock_unpoisoned(&self.inner.pending).shift_remove(&id);
        if let Some(entry) = pending_entry
            && let Some(ref rec) = entry.record
        {
            let mut r = lock_unpoisoned(rec);
            r.status = if ok {
                CodemodeCallStatus::Ok
            } else {
                CodemodeCallStatus::Error
            };
            r.duration_ms = (pi_js::time::performance_now() - entry.started_at).max(0.0);
        }

        self.post(HostToWorkerMessage::Result {
            id,
            ok,
            payload,
            seq: Some(seq),
            now: Some(now),
            properties,
        });
        Ok(())
    }

    fn on_abort(&self) {
        let message = self
            .inner
            .signal
            .as_ref()
            .and_then(|s| s.reason())
            .filter(|r| r.value.is_none())
            .map(|r| r.message)
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
            None,
        );
    }

    fn post(&self, message: HostToWorkerMessage) {
        if self.inner.finished.load(Ordering::Acquire) {
            return;
        }
        let channel = lock_unpoisoned(&self.inner.channel).clone();
        if let Some(ch) = channel {
            ch.post(message);
        } else {
            lock_unpoisoned(&self.inner.unsent).push(message);
        }
    }

    fn receive(&self, raw: RawVmMessage) {
        if let Err(err) = self.handle_message(raw) {
            self.finish(
                Some(CodemodeError {
                    kind: CodemodeErrorKind::Sandbox,
                    name: None,
                    message: format!(
                        "Sandbox bridge broken: {}. The script may have modified built-ins such as a prototype's toJSON.",
                        err.0
                    ),
                    stack: None,
                }),
                None,
                None,
                None,
            );
        }
    }

    fn handle_message(&self, raw: RawVmMessage) -> Result<(), BridgeError> {
        if self.inner.finished.load(Ordering::Acquire) {
            return Ok(());
        }
        let message = match raw {
            RawVmMessage::Typed(msg) => msg,
            RawVmMessage::Json(val) => {
                if !is_worker_to_host_message(&val) {
                    return Err(BridgeError("unknown message from the worker".to_string()));
                }
                serde_json::from_value::<WorkerToHostMessage>(val)
                    .map_err(|_| BridgeError("unknown message from the worker".to_string()))?
            }
        };

        match message {
            WorkerToHostMessage::Output { item, console } => {
                let mut output = lock_unpoisoned(&self.inner.output);
                let idx = output.len();
                output.push(item);
                if console {
                    lock_unpoisoned(&self.inner.console_indices).push(idx);
                }
            }
            WorkerToHostMessage::Call { id, target, name, args } => {
                if lock_unpoisoned(&self.inner.pending).contains_key(&id) {
                    return Err(BridgeError(format!("duplicate call id {id}")));
                }
                let control = lock_unpoisoned(&self.inner.control).clone();
                if let Some(ctrl) = control {
                    self.forward_call(id, target, name, args, &ctrl)?;
                } else {
                    self.handle_call(id, target, name, args);
                }
            }
            WorkerToHostMessage::Started { fingerprint } => {
                if let Some(ref ctrl) = *lock_unpoisoned(&self.inner.control)
                    && let Some(ref cb) = ctrl.on_started
                {
                    cb(fingerprint);
                }
            }
            WorkerToHostMessage::Idle { seq, issued } => {
                if let Some(ref ctrl) = *lock_unpoisoned(&self.inner.control)
                    && let Some(ref cb) = ctrl.on_idle
                {
                    cb(seq, issued, ctrl.run.clone());
                }
            }
            WorkerToHostMessage::Done {
                ok,
                value,
                writes,
                error,
                during,
            } => {
                self.handle_done(ok, value, writes, error, during)?;
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
                    None,
                );
            }
        }
        Ok(())
    }

    fn handle_done(
        &self,
        ok: bool,
        value: Option<String>,
        writes: Option<String>,
        error: Option<String>,
        during: Option<CodemodeDuring>,
    ) -> Result<(), BridgeError> {
        if !ok {
            let err = parse_script_error(error.as_deref().unwrap_or(""))?;
            self.finish(Some(err), None, None, during);
            return Ok(());
        }
        let decoded_value = match value {
            Some(ref json) => Some(parse_bridge_json(json, "return value")?),
            None => None,
        };
        let decoded_writes = parse_store_writes(writes.as_deref().unwrap_or(""))?;
        self.finish(None, decoded_value, Some(decoded_writes), during);
        Ok(())
    }

    fn forward_call(
        &self,
        id: i64,
        target: WorkerCallTarget,
        name: String,
        args: Option<String>,
        control: &ExecutionControl,
    ) -> Result<(), BridgeError> {
        let parsed_args = match args {
            Some(ref raw) => Some(parse_bridge_json(raw, "call arguments")?),
            None => None,
        };
        let record = if target == WorkerCallTarget::Tool {
            let rec = Arc::new(Mutex::new(CodemodeCall {
                name: name.clone(),
                status: CodemodeCallStatus::Cancelled,
                duration_ms: 0.0,
            }));
            lock_unpoisoned(&self.inner.calls).push(Arc::clone(&rec));
            Some(rec)
        } else {
            None
        };
        lock_unpoisoned(&self.inner.pending).insert(
            id,
            PendingCall {
                record,
                started_at: pi_js::time::performance_now(),
                controller: AbortController::new(),
            },
        );
        (control.on_call)(
            CodemodeHostCall {
                id,
                target,
                name,
                args: parsed_args,
            },
            control.run.clone(),
        );
        Ok(())
    }

    fn handle_call(&self, id: i64, target: WorkerCallTarget, name: String, args: Option<String>) {
        let is_tool = target == WorkerCallTarget::Tool;
        let record = if is_tool {
            let rec = Arc::new(Mutex::new(CodemodeCall {
                name: name.clone(),
                status: CodemodeCallStatus::Cancelled,
                duration_ms: 0.0,
            }));
            lock_unpoisoned(&self.inner.calls).push(Arc::clone(&rec));
            Some(rec)
        } else {
            None
        };

        let controller = AbortController::new();
        let signal = controller.signal();
        let started_at = pi_js::time::performance_now();
        {
            let mut pending = lock_unpoisoned(&self.inner.pending);
            if self.inner.finished.load(Ordering::Acquire) {
                controller.abort(Some(AbortReason::error("AbortError", "Execution ended")));
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

        let tool = if is_tool {
            self.inner.executors_tools.get(&name).cloned()
        } else {
            self.inner.executors_globals.get(&name).cloned()
        };

        let exec = self.clone();
        tokio::spawn(async move {
            let call_res: pi_js::Result<Option<Value>> = match tool {
                None => Err(pi_js::Error::msg(format!(
                    "Unknown {} \"{name}\"",
                    if is_tool { "tool" } else { "global" }
                ))),
                Some(tool) => match args.as_deref().map(pi_js::json::parse).transpose() {
                    Ok(parsed_args) => (tool.execute)(parsed_args, CodemodeToolContext { signal }).await,
                    Err(err) => Err(err),
                },
            };

            let Some(_) = lock_unpoisoned(&exec.inner.pending).shift_remove(&id) else {
                return;
            };
            let duration_ms = (pi_js::time::performance_now() - started_at).max(0.0);
            let (status, reply) = match call_res {
                Ok(val) => (
                    CodemodeCallStatus::Ok,
                    HostToWorkerMessage::Result {
                        id,
                        ok: true,
                        payload: val.as_ref().map(pi_js::json::stringify),
                        seq: None,
                        now: None,
                        properties: None,
                    },
                ),
                Err(err) => (
                    CodemodeCallStatus::Error,
                    HostToWorkerMessage::Result {
                        id,
                        ok: false,
                        payload: Some(error_message(&err)),
                        seq: None,
                        now: None,
                        properties: None,
                    },
                ),
            };
            if let Some(ref rec) = record {
                let mut r = lock_unpoisoned(rec);
                r.status = status;
                r.duration_ms = duration_ms;
            }
            exec.post(reply);
        });
    }

    fn finish(
        &self,
        error: Option<CodemodeError>,
        value: Option<Value>,
        writes: Option<CodemodeStoreWrites>,
        during: Option<CodemodeDuring>,
    ) {
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
        lock_unpoisoned(&self.inner.unsent).clear();
        let channel = lock_unpoisoned(&self.inner.channel).take();

        let output = lock_unpoisoned(&self.inner.output).clone();
        let console_output_indices = lock_unpoisoned(&self.inner.console_indices).clone();
        let calls: Vec<CodemodeCall> = lock_unpoisoned(&self.inner.calls)
            .iter()
            .map(|c| lock_unpoisoned(c).clone())
            .collect();

        let result = match error {
            Some(error) => CodemodeResult::Failure {
                ok: false,
                error,
                output,
                console_output_indices,
                calls,
                during,
            },
            None => CodemodeResult::Success {
                ok: true,
                value,
                output,
                console_output_indices,
                calls,
                store_writes: writes.unwrap_or_default(),
                during,
            },
        };

        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            if let Some(ch) = channel {
                ch.stop().await;
            }
            *lock_unpoisoned(&inner.settled_result) = Some(result);
            let _ = inner.settled_tx.send(true);
        });
    }
}

pub(crate) fn wasm_cache_cell(input: Option<crate::types::CodemodeWasmInput>) -> WasmCache {
    WasmCache {
        input: Mutex::new(input),
        cell: tokio::sync::OnceCell::new(),
    }
}

pub(crate) struct WasmCache {
    input: Mutex<Option<crate::types::CodemodeWasmInput>>,
    cell: tokio::sync::OnceCell<std::result::Result<crate::wasm::CodemodeWasmModule, String>>,
}

impl WasmCache {
    pub async fn get(&self) -> pi_js::Result<crate::wasm::CodemodeWasmModule> {
        let res = self
            .cell
            .get_or_init(|| async {
                let input = lock_unpoisoned(&self.input).take();
                match input {
                    Some(crate::types::CodemodeWasmInput::Module(module)) => Ok(module),
                    Some(crate::types::CodemodeWasmInput::Pending(pending)) => {
                        let fut = lock_unpoisoned(&pending).take();
                        match fut {
                            Some(fut) => fut.await.map_err(|e| error_message(&e)),
                            None => Err("Pending wasm loader future already consumed".to_string()),
                        }
                    }
                    None => crate::wasm::load_quick_js_wasm(None)
                        .await
                        .map_err(|e| error_message(&e)),
                }
            })
            .await;
        res.clone().map_err(pi_js::Error::msg)
    }
}

struct SandboxBaseState {
    tools_by_name: Mutex<IndexMap<String, CodemodeTool>>,
    globals_by_name: IndexMap<String, CodemodeTool>,
    timeout_ms: f64,
    memory_limit_bytes: Option<f64>,
    launch: VmLauncher,
    running: Mutex<Vec<(usize, Execution)>>,
    next_run_id: std::sync::atomic::AtomicUsize,
    closed: Mutex<bool>,
}

/// Tool table, defaults, and in-flight executions shared by every sandbox implementation.
#[derive(Clone)]
pub struct CodemodeSandboxBase {
    state: Arc<SandboxBaseState>,
}

impl std::fmt::Debug for CodemodeSandboxBase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodemodeSandboxBase")
            .field("timeout_ms", &self.state.timeout_ms)
            .field("memory_limit_bytes", &self.state.memory_limit_bytes)
            .finish()
    }
}

impl CodemodeSandboxBase {
    pub fn new(options: CodemodeSandboxBaseOptions, launch: VmLauncher) -> pi_js::Result<Self> {
        let globals_list = options.globals.unwrap_or_default();
        let declared_globals: Vec<CodemodeDeclaredGlobal> = globals_list
            .iter()
            .map(|g| CodemodeDeclaredGlobal {
                name: g.name.clone(),
                spread: g.spread,
            })
            .collect();
        check_declared_globals(&declared_globals)?;

        let mut globals_by_name = IndexMap::new();
        for g in globals_list {
            globals_by_name.insert(g.name.clone(), g);
        }

        let base = Self {
            state: Arc::new(SandboxBaseState {
                tools_by_name: Mutex::new(IndexMap::new()),
                globals_by_name,
                timeout_ms: options.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
                memory_limit_bytes: options.memory_limit_bytes,
                launch,
                running: Mutex::new(Vec::new()),
                next_run_id: std::sync::atomic::AtomicUsize::new(1),
                closed: Mutex::new(false),
            }),
        };

        for tool in options.tools.unwrap_or_default() {
            base.register_tool(tool)?;
        }

        Ok(base)
    }

    /// Returns an error if a tool with the same name is already registered.
    pub fn register_tool(&self, tool: CodemodeTool) -> pi_js::Result<()> {
        let mut map = lock_unpoisoned(&self.state.tools_by_name);
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
        lock_unpoisoned(&self.state.tools_by_name).shift_remove(name).is_some()
    }

    pub fn tools(&self) -> Vec<CodemodeTool> {
        lock_unpoisoned(&self.state.tools_by_name).values().cloned().collect()
    }

    pub fn globals(&self) -> Vec<CodemodeTool> {
        self.state.globals_by_name.values().cloned().collect()
    }

    /// `code` is an async function body: `return` and top-level `await` work.
    pub async fn execute(&self, code: &str, options: Option<CodemodeExecuteOptions>) -> pi_js::Result<CodemodeResult> {
        if *lock_unpoisoned(&self.state.closed) {
            return Err(pi_js::Error::msg("Sandbox is closed"));
        }
        let options = options.unwrap_or_default();
        let tools_map = lock_unpoisoned(&self.state.tools_by_name).clone();
        let declared_tools: Vec<CodemodeDeclaredTool> = tools_map
            .values()
            .map(|t| CodemodeDeclaredTool {
                name: t.name.clone(),
                js_name: None,
                description: t.description.clone(),
            })
            .collect();
        let declared_globals: Vec<CodemodeDeclaredGlobal> = self
            .state
            .globals_by_name
            .values()
            .map(|g| CodemodeDeclaredGlobal {
                name: g.name.clone(),
                spread: g.spread,
            })
            .collect();

        let (exec, _) = Execution::start_with_run(ExecutionOptions {
            code: code.to_string(),
            tools: declared_tools,
            globals: declared_globals,
            executors_tools: tools_map,
            executors_globals: self.state.globals_by_name.clone(),
            timeout_ms: options.timeout_ms.unwrap_or(self.state.timeout_ms),
            signal: options.signal,
            memory_limit_bytes: self.state.memory_limit_bytes,
            store: serialize_store(options.store.as_ref()),
            deterministic: None,
            control_on_started: None,
            control_on_call: None,
            control_on_idle: None,
            launch: Arc::clone(&self.state.launch),
        });

        let id = self.state.next_run_id.fetch_add(1, Ordering::Relaxed);
        lock_unpoisoned(&self.state.running).push((id, exec.clone()));
        let result = exec.result().await;
        lock_unpoisoned(&self.state.running).retain(|(entry_id, _)| *entry_id != id);
        Ok(result)
    }

    /// Run `code` deterministically, with the caller answering its calls via [`CodemodeRun::deliver`].
    pub fn start(&self, code: &str, options: CodemodeRunOptions) -> pi_js::Result<CodemodeRun> {
        if *lock_unpoisoned(&self.state.closed) {
            return Err(pi_js::Error::msg("Sandbox is closed"));
        }
        if !options.started_at.is_finite() || options.started_at < 0.0 {
            return Err(pi_js::Error::msg(
                "startedAt must be a non-negative number of milliseconds since the epoch",
            ));
        }
        let tools = options.tools.unwrap_or_default();
        let mut raw_names = IndexSet::new();
        let mut script_names = IndexSet::new();
        for tool in &tools {
            if !raw_names.insert(tool.name.clone()) {
                return Err(pi_js::Error::msg("Tool names must be unique"));
            }
            let js_name = tool
                .js_name
                .clone()
                .unwrap_or_else(|| to_codemode_identifier(&tool.name));
            if !script_names.insert(js_name) {
                return Err(pi_js::Error::msg("Tool script names must be unique"));
            }
        }
        let globals = options.globals.unwrap_or_default();
        check_declared_globals(&globals)?;

        let (exec, run) = Execution::start_with_run(ExecutionOptions {
            code: code.to_string(),
            tools,
            globals,
            executors_tools: IndexMap::new(),
            executors_globals: IndexMap::new(),
            timeout_ms: options.timeout_ms.unwrap_or(f64::INFINITY),
            signal: options.signal,
            memory_limit_bytes: self.state.memory_limit_bytes,
            store: serialize_store(options.store.as_ref()),
            deterministic: Some(DeterministicConfig {
                started_at: options.started_at,
                seed: options.seed,
            }),
            control_on_started: options.on_started,
            control_on_call: Some(options.on_call),
            control_on_idle: options.on_idle,
            launch: Arc::clone(&self.state.launch),
        });

        let id = self.state.next_run_id.fetch_add(1, Ordering::Relaxed);
        lock_unpoisoned(&self.state.running).push((id, exec.clone()));
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let _ = exec.result().await;
            lock_unpoisoned(&state.running).retain(|(entry_id, _)| *entry_id != id);
        });

        Ok(run.expect("control_on_call guarantees CodemodeRun"))
    }

    /// Aborts in-flight executions (they resolve with `kind: "aborted"`) and rejects new ones.
    pub async fn close(&self) {
        *lock_unpoisoned(&self.state.closed) = true;
        let active: Vec<Execution> = lock_unpoisoned(&self.state.running)
            .iter()
            .map(|(_, exec)| exec.clone())
            .collect();
        let futs: Vec<_> = active.iter().map(|e| e.abort("Sandbox closed")).collect();
        for fut in futs {
            fut.await;
        }
    }
}

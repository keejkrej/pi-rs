//! Port of packages/codemode/src/runtime/remote.ts
//!
//! Runs the VM in another isolate or process reached over an RPC-like exchange boundary while tools
//! keep running on the host.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::FutureExt;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::execution::{
    CodemodeSandboxBase, RawVmMessage, VmChannel, VmEvents, VmLauncher, error_message, lock_unpoisoned, wasm_cache_cell,
};
use crate::runtime::inline::{DEFAULT_INTERRUPT_BUDGET, inline_launcher};
use crate::runtime::protocol::{HostToWorkerMessage, VmStartData, WorkerToHostMessage, is_worker_to_host_message};
use crate::types::{
    CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeResult, CodemodeRun, CodemodeRunOptions,
    CodemodeSandboxBaseOptions, CodemodeTool, CodemodeWasmInput,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeRemoteRequest {
    pub data: VmStartData,
    /// Requested interrupt budget. The remote may lower it; see [`CodemodeServeOptions`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt_budget: Option<usize>,
}

/// What the remote sends to the client: VM messages, a failure outside the VM bridge, or raw
/// unvalidated messages passed through for bridge testing.
#[derive(Clone, Debug, PartialEq)]
pub enum CodemodeRemoteMessage {
    Worker(WorkerToHostMessage),
    Failure { error: CodemodeError },
    Raw(Value),
}

impl From<WorkerToHostMessage> for CodemodeRemoteMessage {
    fn from(value: WorkerToHostMessage) -> Self {
        Self::Worker(value)
    }
}

impl Serialize for CodemodeRemoteMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Worker(msg) => msg.serialize(serializer),
            Self::Failure { error } => {
                #[derive(Serialize)]
                struct FailureWire<'a> {
                    #[serde(rename = "type")]
                    kind: &'static str,
                    error: &'a CodemodeError,
                }
                FailureWire { kind: "failure", error }.serialize(serializer)
            }
            Self::Raw(val) => val.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for CodemodeRemoteMessage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if value.as_object().and_then(|o| o.get("type")).and_then(Value::as_str) == Some("failure")
            && let Some(err_val) = value.as_object().and_then(|o| o.get("error"))
            && let Ok(error) = serde_json::from_value::<CodemodeError>(err_val.clone())
        {
            return Ok(Self::Failure { error });
        }
        if is_worker_to_host_message(&value)
            && let Ok(msg) = serde_json::from_value::<WorkerToHostMessage>(value.clone())
        {
            return Ok(Self::Worker(msg));
        }
        Ok(Self::Raw(value))
    }
}

/// The client's answer to an exchange: a tool result, or `stop` once the client no longer wants the execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CodemodeRemoteReply {
    #[serde(rename = "result")]
    Result {
        id: i64,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        now: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        properties: Option<String>,
    },
    #[serde(rename = "stop")]
    Stop,
}

impl From<HostToWorkerMessage> for CodemodeRemoteReply {
    fn from(msg: HostToWorkerMessage) -> Self {
        match msg {
            HostToWorkerMessage::Result {
                id,
                ok,
                payload,
                seq,
                now,
                properties,
            } => Self::Result {
                id,
                ok,
                payload,
                seq,
                now,
                properties,
            },
        }
    }
}

pub type CodemodeRemoteExchange = Arc<
    dyn Fn(Vec<CodemodeRemoteMessage>) -> pi_js::BoxFuture<pi_js::Result<Option<CodemodeRemoteReply>>> + Send + Sync,
>;

/// Remote VM executor called once per `execute()` or `start()`.
#[async_trait]
pub trait CodemodeRemote: Send + Sync {
    async fn execute(&self, request: CodemodeRemoteRequest, exchange: CodemodeRemoteExchange) -> pi_js::Result<()>;
}

#[derive(Clone)]
pub struct RemoteCodemodeSandboxOptions {
    pub tools: Option<Vec<CodemodeTool>>,
    pub globals: Option<Vec<CodemodeTool>>,
    pub timeout_ms: Option<f64>,
    pub memory_limit_bytes: Option<f64>,
    pub remote: Arc<dyn Fn() -> pi_js::Result<Arc<dyn CodemodeRemote>> + Send + Sync>,
    /// Interrupt budget requested from the remote. Default: the remote's default.
    pub interrupt_budget: Option<usize>,
}

impl From<&RemoteCodemodeSandboxOptions> for CodemodeSandboxBaseOptions {
    fn from(opts: &RemoteCodemodeSandboxOptions) -> Self {
        Self {
            tools: opts.tools.clone(),
            globals: opts.globals.clone(),
            timeout_ms: opts.timeout_ms,
            memory_limit_bytes: opts.memory_limit_bytes,
        }
    }
}

const FLUSH_ITEMS: usize = 1_000;
const FLUSH_CHARS: usize = 1 << 20;

#[derive(Clone)]
pub struct CodemodeServeOptions {
    pub wasm: CodemodeWasmInput,
    /// Upper bound for a request's interrupt budget, and the budget when a request names none.
    pub max_interrupt_budget: Option<usize>,
    /// Upper bound for a request's VM memory, and the limit when a request names none.
    pub max_memory_limit_bytes: Option<f64>,
}

struct ServeState {
    channel: Option<Arc<dyn VmChannel>>,
    ended: bool,
    buffered: Vec<CodemodeRemoteMessage>,
    buffered_chars: usize,
    unsent: Vec<HostToWorkerMessage>,
}

/// Remote side of [`RemoteCodemodeSandbox`]: runs one execution's VM and forwards everything it
/// reports through `exchange`. Resolves once the VM stops and the terminal message has been
/// delivered, or the client asked to stop. Never rejects.
pub async fn serve_codemode_remote(
    request: CodemodeRemoteRequest,
    exchange: CodemodeRemoteExchange,
    options: CodemodeServeOptions,
) {
    let max_budget = options.max_interrupt_budget.unwrap_or(DEFAULT_INTERRUPT_BUDGET);
    let budget = request.interrupt_budget.map_or(max_budget, |b| b.min(max_budget));
    let requested_mem = request.data.memory_limit_bytes;
    let max_mem = options.max_memory_limit_bytes;
    let memory_limit_bytes = match (requested_mem, max_mem) {
        (None, max) => max,
        (Some(req), None) => Some(req),
        (Some(req), Some(max)) => Some(req.min(max)),
    };

    let data = VmStartData {
        memory_limit_bytes,
        ..request.data
    };

    let cache = Arc::new(wasm_cache_cell(Some(options.wasm)));
    let wasm_fut = async move { cache.get().await };
    let wrapped_wasm = CodemodeWasmInput::Pending(Arc::new(Mutex::new(Some(Box::pin(wasm_fut)))));
    let launch = inline_launcher(wrapped_wasm, Some(budget));

    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let done_tx_slot = Arc::new(Mutex::new(Some(done_tx)));
    let state = Arc::new(Mutex::new(ServeState {
        channel: None,
        ended: false,
        buffered: Vec::new(),
        buffered_chars: 0,
        unsent: Vec::new(),
    }));

    fn trigger_end(
        state: &Arc<Mutex<ServeState>>,
        exchange: &CodemodeRemoteExchange,
        done_tx_slot: &Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
        terminal: Option<CodemodeRemoteMessage>,
    ) {
        let (batch, channel) = {
            let mut st = lock_unpoisoned(state);
            if st.ended {
                return;
            }
            st.ended = true;
            let mut batch = std::mem::take(&mut st.buffered);
            st.buffered_chars = 0;
            if let Some(term) = terminal {
                batch.push(term);
            }
            let ch = st.channel.clone();
            (batch, ch)
        };

        let exchange = Arc::clone(exchange);
        let done_slot = Arc::clone(done_tx_slot);
        let has_terminal = !batch.is_empty();
        tokio::spawn(async move {
            if has_terminal {
                let _ = exchange(batch).await;
            }
            if let Some(ch) = channel {
                ch.stop().await;
            }
            if let Some(tx) = lock_unpoisoned(&done_slot).take() {
                let _ = tx.send(());
            }
        });
    }

    fn send_batch(
        state: Arc<Mutex<ServeState>>,
        exchange: CodemodeRemoteExchange,
        done_slot: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
        batch: Vec<CodemodeRemoteMessage>,
    ) {
        tokio::spawn(async move {
            match exchange(batch).await {
                Ok(None) => {}
                Ok(Some(CodemodeRemoteReply::Stop)) | Err(_) => {
                    trigger_end(&state, &exchange, &done_slot, None);
                }
                Ok(Some(CodemodeRemoteReply::Result {
                    id,
                    ok,
                    payload,
                    seq,
                    now,
                    properties,
                })) => {
                    let msg = HostToWorkerMessage::Result {
                        id,
                        ok,
                        payload,
                        seq,
                        now,
                        properties,
                    };
                    let ch = {
                        let mut st = lock_unpoisoned(&state);
                        if st.ended {
                            return;
                        }
                        if let Some(ref ch) = st.channel {
                            Some(Arc::clone(ch))
                        } else {
                            st.unsent.push(msg);
                            return;
                        }
                    };
                    if let Some(ch) = ch {
                        ch.post(msg);
                    }
                }
            }
        });
    }

    let msg_state = Arc::clone(&state);
    let msg_exchange = Arc::clone(&exchange);
    let msg_done = Arc::clone(&done_tx_slot);
    let fail_state = Arc::clone(&state);
    let fail_exchange = Arc::clone(&exchange);
    let fail_done = Arc::clone(&done_tx_slot);

    let events = VmEvents {
        message: Arc::new(move |raw| {
            let RawVmMessage::Typed(typed) = raw else {
                return;
            };
            match &typed {
                WorkerToHostMessage::Output { item, .. } => {
                    let chars = match item {
                        crate::types::CodemodeOutputItem::Text { text } => text.len(),
                        crate::types::CodemodeOutputItem::Image { data, .. } => data.len(),
                    };
                    let flush_batch = {
                        let mut st = lock_unpoisoned(&msg_state);
                        if st.ended {
                            return;
                        }
                        st.buffered.push(CodemodeRemoteMessage::Worker(typed));
                        st.buffered_chars += chars;
                        if st.buffered.len() >= FLUSH_ITEMS || st.buffered_chars >= FLUSH_CHARS {
                            st.buffered_chars = 0;
                            Some(std::mem::take(&mut st.buffered))
                        } else {
                            None
                        }
                    };
                    if let Some(batch) = flush_batch {
                        send_batch(
                            Arc::clone(&msg_state),
                            Arc::clone(&msg_exchange),
                            Arc::clone(&msg_done),
                            batch,
                        );
                    }
                }
                WorkerToHostMessage::Started { .. } => {
                    let mut st = lock_unpoisoned(&msg_state);
                    if st.ended {
                        return;
                    }
                    st.buffered.push(CodemodeRemoteMessage::Worker(typed));
                }
                WorkerToHostMessage::Call { .. } | WorkerToHostMessage::Idle { .. } => {
                    let batch = {
                        let mut st = lock_unpoisoned(&msg_state);
                        if st.ended {
                            return;
                        }
                        let mut b = std::mem::take(&mut st.buffered);
                        st.buffered_chars = 0;
                        b.push(CodemodeRemoteMessage::Worker(typed));
                        b
                    };
                    send_batch(
                        Arc::clone(&msg_state),
                        Arc::clone(&msg_exchange),
                        Arc::clone(&msg_done),
                        batch,
                    );
                }
                WorkerToHostMessage::Done { .. } | WorkerToHostMessage::Crash { .. } => {
                    trigger_end(
                        &msg_state,
                        &msg_exchange,
                        &msg_done,
                        Some(CodemodeRemoteMessage::Worker(typed)),
                    );
                }
            }
        }),
        failure: Arc::new(move |error| {
            trigger_end(
                &fail_state,
                &fail_exchange,
                &fail_done,
                Some(CodemodeRemoteMessage::Failure { error }),
            );
        }),
    };

    match launch(data, events).await {
        Ok(started) => {
            let (ended, unsent) = {
                let mut st = lock_unpoisoned(&state);
                st.channel = Some(Arc::clone(&started));
                (st.ended, std::mem::take(&mut st.unsent))
            };
            if ended {
                started.stop().await;
            } else {
                for msg in unsent {
                    started.post(msg);
                }
            }
        }
        Err(err) => {
            trigger_end(
                &state,
                &exchange,
                &done_tx_slot,
                Some(CodemodeRemoteMessage::Failure {
                    error: CodemodeError {
                        kind: CodemodeErrorKind::Sandbox,
                        name: None,
                        message: error_message(&err),
                        stack: None,
                    },
                }),
            );
        }
    }

    let _ = done_rx.await;
}

struct RemoteChannel {
    stopped: AtomicBool,
    waiting: Mutex<IndexMap<i64, tokio::sync::oneshot::Sender<Option<CodemodeRemoteReply>>>>,
}

impl VmChannel for RemoteChannel {
    fn post(&self, message: HostToWorkerMessage) {
        let HostToWorkerMessage::Result { id, .. } = &message;
        let waiter = lock_unpoisoned(&self.waiting).shift_remove(id);
        if let Some(tx) = waiter {
            let _ = tx.send(Some(CodemodeRemoteReply::from(message)));
        }
    }

    fn stop(&self) -> pi_js::BoxFuture<()> {
        self.stopped.store(true, Ordering::Release);
        let waiters = std::mem::take(&mut *lock_unpoisoned(&self.waiting));
        for (_id, tx) in waiters {
            let _ = tx.send(Some(CodemodeRemoteReply::Stop));
        }
        Box::pin(async {})
    }
}

fn remote_launcher(
    remote: Arc<dyn Fn() -> pi_js::Result<Arc<dyn CodemodeRemote>> + Send + Sync>,
    interrupt_budget: Option<usize>,
) -> VmLauncher {
    Arc::new(move |data: VmStartData, events: VmEvents| {
        let remote = Arc::clone(&remote);
        Box::pin(async move {
            let target = remote().map_err(|err| {
                pi_js::Error::msg(format!("Failed to reach the remote sandbox: {}", error_message(&err)))
            })?;

            let channel = Arc::new(RemoteChannel {
                stopped: AtomicBool::new(false),
                waiting: Mutex::new(IndexMap::new()),
            });
            let settled = Arc::new(AtomicBool::new(false));

            let ex_channel = Arc::clone(&channel);
            let ex_settled = Arc::clone(&settled);
            let ex_events = events.clone();

            let exchange: CodemodeRemoteExchange = Arc::new(move |messages: Vec<CodemodeRemoteMessage>| {
                let channel = Arc::clone(&ex_channel);
                let settled = Arc::clone(&ex_settled);
                let events = ex_events.clone();
                Box::pin(async move {
                    if channel.stopped.load(Ordering::Acquire) {
                        return Ok(Some(CodemodeRemoteReply::Stop));
                    }

                    let call_id = messages.iter().find_map(|m| match m {
                        CodemodeRemoteMessage::Worker(WorkerToHostMessage::Call { id, .. }) => Some(*id),
                        _ => None,
                    });
                    let rx = call_id.map(|id| {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        lock_unpoisoned(&channel.waiting).insert(id, tx);
                        rx
                    });

                    for msg in messages {
                        match msg {
                            CodemodeRemoteMessage::Failure { error } => {
                                settled.store(true, Ordering::Release);
                                (events.failure)(error);
                            }
                            CodemodeRemoteMessage::Worker(w) => {
                                if matches!(w, WorkerToHostMessage::Done { .. } | WorkerToHostMessage::Crash { .. }) {
                                    settled.store(true, Ordering::Release);
                                }
                                (events.message)(RawVmMessage::Typed(w));
                            }
                            CodemodeRemoteMessage::Raw(val) => {
                                if val
                                    .as_object()
                                    .and_then(|o| o.get("type"))
                                    .and_then(Value::as_str)
                                    .is_some_and(|t| t == "done" || t == "crash")
                                {
                                    settled.store(true, Ordering::Release);
                                }
                                (events.message)(RawVmMessage::Json(val));
                            }
                        }
                    }

                    match rx {
                        Some(rx) => Ok(rx.await.unwrap_or(Some(CodemodeRemoteReply::Stop))),
                        None => Ok(None),
                    }
                })
            });

            let request = CodemodeRemoteRequest { data, interrupt_budget };

            let run_channel = Arc::clone(&channel);
            let run_settled = Arc::clone(&settled);
            let run_events = events;
            tokio::spawn(async move {
                match target.execute(request, exchange).await {
                    Ok(()) => {
                        if !run_settled.load(Ordering::Acquire) && !run_channel.stopped.load(Ordering::Acquire) {
                            (run_events.failure)(CodemodeError {
                                kind: CodemodeErrorKind::Sandbox,
                                name: None,
                                message: "Remote sandbox returned before the script settled".to_string(),
                                stack: None,
                            });
                        }
                    }
                    Err(err) => {
                        if !run_channel.stopped.load(Ordering::Acquire) {
                            (run_events.failure)(CodemodeError {
                                kind: CodemodeErrorKind::Sandbox,
                                name: None,
                                message: format!("Remote sandbox failed: {}", error_message(&err)),
                                stack: None,
                            });
                        }
                    }
                }
            });

            Ok(channel as Arc<dyn VmChannel>)
        })
        .boxed()
    })
}

/// Runs each execution's VM on a remote executor reached over an RPC-like exchange while tools run
/// on the host.
#[derive(Clone, Debug)]
pub struct RemoteCodemodeSandbox {
    base: CodemodeSandboxBase,
}

impl RemoteCodemodeSandbox {
    pub fn new(options: RemoteCodemodeSandboxOptions) -> pi_js::Result<Self> {
        let base_opts = CodemodeSandboxBaseOptions::from(&options);
        let launcher = remote_launcher(options.remote, options.interrupt_budget);
        Ok(Self {
            base: CodemodeSandboxBase::new(base_opts, launcher)?,
        })
    }

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

    pub async fn execute(&self, code: &str, options: Option<CodemodeExecuteOptions>) -> pi_js::Result<CodemodeResult> {
        self.base.execute(code, options).await
    }

    pub fn start(&self, code: &str, options: CodemodeRunOptions) -> pi_js::Result<CodemodeRun> {
        self.base.start(code, options)
    }

    pub async fn close(&self) {
        self.base.close().await;
    }
}

//! Port of packages/codemode/src/runtime/inline.ts
//!
//! Runs the VM on the calling runtime thread using an interrupt budget rather than a worker thread.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::FutureExt;

use crate::runtime::execution::{CodemodeSandboxBase, RawVmMessage, VmChannel, VmEvents, VmLauncher, wasm_cache_cell};
use crate::runtime::protocol::{HostToWorkerMessage, VmStartData, WorkerToHostMessage};
use crate::runtime::vm::{StartVmOptions, VmSession, start_vm};
use crate::types::{
    CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeResult, CodemodeRun, CodemodeRunOptions,
    CodemodeSandboxBaseOptions, CodemodeTool, CodemodeWasmInput,
};

pub const DEFAULT_INTERRUPT_BUDGET: usize = 100_000;

/// Allows recursion about 1,000 calls deep and trips well before a 1 MB host stack overflows.
pub const INLINE_MAX_STACK_SIZE: i64 = 256 * 1024;

#[derive(Clone, Debug)]
pub struct InlineCodemodeSandboxOptions {
    pub tools: Option<Vec<CodemodeTool>>,
    pub globals: Option<Vec<CodemodeTool>>,
    pub timeout_ms: Option<f64>,
    pub memory_limit_bytes: Option<f64>,
    pub wasm: CodemodeWasmInput,
    /// Interrupt polls an execution may use before it fails with `kind: "timeout"`. Default: `DEFAULT_INTERRUPT_BUDGET`.
    pub interrupt_budget: Option<usize>,
}

impl From<&InlineCodemodeSandboxOptions> for CodemodeSandboxBaseOptions {
    fn from(opts: &InlineCodemodeSandboxOptions) -> Self {
        Self {
            tools: opts.tools.clone(),
            globals: opts.globals.clone(),
            timeout_ms: opts.timeout_ms,
            memory_limit_bytes: opts.memory_limit_bytes,
        }
    }
}

enum InlinePumpItem {
    Msg(WorkerToHostMessage),
    Failure(CodemodeError),
}

struct InlineChannel {
    stopped: AtomicBool,
    settled: AtomicBool,
    polls: AtomicUsize,
    exhausted: AtomicBool,
    session: Mutex<Option<Arc<VmSession>>>,
    in_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<HostToWorkerMessage>>>,
}

impl VmChannel for InlineChannel {
    fn post(&self, message: HostToWorkerMessage) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let tx = self.in_tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(tx) = tx {
            let _ = tx.send(message);
        }
    }

    fn stop(&self) -> pi_js::BoxFuture<()> {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            *self.in_tx.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let session = self.session.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(sess) = session {
                sess.dispose();
            }
        }
        Box::pin(async {})
    }
}

pub fn inline_launcher(wasm: CodemodeWasmInput, interrupt_budget: Option<usize>) -> VmLauncher {
    let wasm_cache = Arc::new(wasm_cache_cell(Some(wasm)));
    let budget = interrupt_budget.unwrap_or(DEFAULT_INTERRUPT_BUDGET);

    Arc::new(move |data: VmStartData, events: VmEvents| {
        let wasm_cache = Arc::clone(&wasm_cache);
        Box::pin(async move {
            let module = wasm_cache
                .get()
                .await
                .map_err(|e| pi_js::Error::msg(format!("Failed to load QuickJS: {}", e.message())))?;

            let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<InlinePumpItem>();
            let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<HostToWorkerMessage>();

            let channel = Arc::new(InlineChannel {
                stopped: AtomicBool::new(false),
                settled: AtomicBool::new(false),
                polls: AtomicUsize::new(0),
                exhausted: AtomicBool::new(false),
                session: Mutex::new(None),
                in_tx: Mutex::new(Some(in_tx)),
            });

            let ci = Arc::clone(&channel);
            let should_interrupt = Arc::new(move || {
                if ci.stopped.load(Ordering::Acquire)
                    || ci.settled.load(Ordering::Acquire)
                    || ci.exhausted.load(Ordering::Acquire)
                {
                    return true;
                }
                let next = ci.polls.fetch_add(1, Ordering::AcqRel) + 1;
                if next <= budget {
                    return false;
                }
                ci.exhausted.store(true, Ordering::Release);
                true
            });

            let cp = Arc::clone(&channel);
            let post_tx = out_tx.clone();
            let post = Arc::new(move |message: WorkerToHostMessage| {
                let terminal = matches!(
                    message,
                    WorkerToHostMessage::Done { .. } | WorkerToHostMessage::Crash { .. }
                );
                if terminal && cp.settled.swap(true, Ordering::AcqRel) {
                    return;
                }
                if terminal && cp.exhausted.load(Ordering::Acquire) {
                    let _ = post_tx.send(InlinePumpItem::Failure(CodemodeError {
                        kind: CodemodeErrorKind::Timeout,
                        name: None,
                        message: format!("Execution exceeded its interrupt budget of {budget}"),
                        stack: None,
                    }));
                    return;
                }
                let _ = post_tx.send(InlinePumpItem::Msg(message));
            });

            let session = start_vm(StartVmOptions {
                data,
                wasm: module,
                max_stack_size: Some(INLINE_MAX_STACK_SIZE),
                interrupt_budget: Some(budget),
                tz: pi_js::env::var("TZ"),
                post,
                should_interrupt,
            })?;
            *channel.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(session));

            drop(out_tx);
            let pump_channel = Arc::clone(&channel);
            tokio::spawn(async move {
                let mut in_closed = false;
                loop {
                    tokio::select! {
                        biased;
                        maybe_out = out_rx.recv() => {
                            let Some(item) = maybe_out else { break; };
                            if pump_channel.stopped.load(Ordering::Acquire) {
                                break;
                            }
                            match item {
                                InlinePumpItem::Msg(msg) => (events.message)(RawVmMessage::Typed(msg)),
                                InlinePumpItem::Failure(err) => (events.failure)(err),
                            }
                        }
                        maybe_in = in_rx.recv(), if !in_closed => {
                            let Some(reply) = maybe_in else {
                                in_closed = true;
                                continue;
                            };
                            if pump_channel.stopped.load(Ordering::Acquire) {
                                break;
                            }
                            let sess = pump_channel.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            if let Some(sess) = sess {
                                sess.deliver(reply);
                            }
                        }
                    }
                    if pump_channel.stopped.load(Ordering::Acquire) {
                        break;
                    }
                }
            });

            Ok(channel as Arc<dyn VmChannel>)
        })
        .boxed()
    })
}

/// Runs each execution's QuickJS VM on the calling runtime thread with an interrupt budget.
#[derive(Clone, Debug)]
pub struct InlineCodemodeSandbox {
    base: CodemodeSandboxBase,
}

impl InlineCodemodeSandbox {
    pub fn new(options: InlineCodemodeSandboxOptions) -> pi_js::Result<Self> {
        let base_opts = CodemodeSandboxBaseOptions::from(&options);
        let launcher = inline_launcher(options.wasm, options.interrupt_budget);
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

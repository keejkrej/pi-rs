//! Port of packages/codemode/src/types.ts

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::wasm::CodemodeWasmModule;

/// Aborted when the script finishes (including unawaited calls), the
/// execution times out, the caller aborts, or the sandbox is closed.
#[derive(Clone, Debug)]
pub struct CodemodeToolContext {
    pub signal: pi_js::abort::AbortSignal,
}

/// A JSON Schema document. Only used to render declarations; values are not validated against it.
///
/// `Object` is `{ [key: string]: unknown }`. `Boolean` is a boolean schema (`true` / `false`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodemodeJsonSchema {
    Object(IndexMap<String, Value>),
    Boolean(bool),
}

/// A host tool or global the script can call.
///
/// `execute` receives `None` for a JS `undefined` argument and `Some(Value::Null)` for JSON `null`.
/// `Ok(None)` is a JS `undefined` result; a thrown error is `Err`.
#[derive(Clone)]
pub struct CodemodeTool {
    /// The script calls tools as `tools.<id>(args)`, where `<id>` is the name with characters that
    /// are not valid in identifiers replaced by `_` (see `toCodemodeIdentifier`), and also as
    /// `tools["<name>"](args)`. Globals are called as `<name>(args)` and must be identifiers, or
    /// `<namespace>.<member>`, which groups them into a frozen namespace object.
    pub name: String,
    /// Shown as a doc comment in [`render_declarations`](crate::render_declarations), and listed in `ALL_TOOLS` for tools.
    pub description: Option<String>,
    /// Schema of the single argument. Rendered as the parameter type; `unknown` when omitted.
    pub input_schema: Option<CodemodeJsonSchema>,
    /// Schema of the resolved value. Rendered as the promise type; `unknown` when omitted.
    pub output_schema: Option<CodemodeJsonSchema>,
    /// Globals only: `execute` receives all call arguments as an array instead of the first one.
    pub spread: Option<bool>,
    /// Globals only: TypeScript parameter list and return type for [`render_declarations`](crate::render_declarations),
    /// for example `(type: string, id?: string): Promise<Model[]>`. Replaces the rendering from the schemas.
    pub signature: Option<String>,
    /// `args` is whatever the script passed, after a JSON round trip. The return
    /// value must be JSON-serializable; a thrown error surfaces in the script as
    /// an `Error` with the same message.
    pub execute:
        Arc<dyn Fn(Option<Value>, CodemodeToolContext) -> pi_js::BoxFuture<pi_js::Result<Option<Value>>> + Send + Sync>,
}

impl std::fmt::Debug for CodemodeTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodemodeTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .field("output_schema", &self.output_schema)
            .field("spread", &self.spread)
            .field("signature", &self.signature)
            .finish()
    }
}

/// One item of the script's output, in the order the script produced it: `text()` and `console.*`
/// produce text items, `image()` image items. `data` is base64.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CodemodeOutputItem {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodemodeCallStatus {
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "cancelled")]
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeCall {
    pub name: String,
    pub status: CodemodeCallStatus,
    /// `performance.now()` delta, not an integer epoch.
    pub duration_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodemodeCallTarget {
    #[serde(rename = "tool")]
    Tool,
    #[serde(rename = "global")]
    Global,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodemodeErrorKind {
    /// The script threw or failed to parse. `name` and `stack` come from the script's error.
    #[serde(rename = "script")]
    Script,
    /// The overall deadline expired, or a VM on the calling thread used up its interrupt budget.
    /// The execution ended and the VM was asked to stop; a worker or remote VM may still be computing.
    #[serde(rename = "timeout")]
    Timeout,
    /// The caller's signal fired or the sandbox was closed. The execution ended and the VM was asked to stop.
    #[serde(rename = "aborted")]
    Aborted,
    /// The VM or its transport failed outside the script's control (for example a wasm trap, a missing
    /// worker file, or a remote sandbox that failed).
    #[serde(rename = "sandbox")]
    Sandbox,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeError {
    pub kind: CodemodeErrorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

/// Keys the script changed with `store()`. Only successful executions report writes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CodemodeStoreWrites {
    pub set: IndexMap<String, Value>,
    /// Keys stored as `undefined`.
    #[serde(rename = "delete")]
    pub r#delete: Vec<String>,
}

/// In a [`CodemodeSandboxBase::start`](crate::CodemodeSandboxBase::start) run: the result the VM was processing
/// when the script finished, `0` for its first run, and the calls issued by then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeDuring {
    pub seq: i64,
    pub issued: i64,
}

/// `output` is kept for failed executions too, up to the failure. `exit()` completes with `value: undefined`.
///
/// `value: None` is JS `undefined` (the key is omitted). `Some(Value::Null)` is JSON `null`.
/// `during`: in a [`CodemodeSandboxBase::start`](crate::CodemodeSandboxBase::start) run that the script itself ended
/// (it returned, threw, or exited), the result being processed when it did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodemodeResult {
    Success {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
        output: Vec<CodemodeOutputItem>,
        #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "consoleOutputIndices")]
        console_output_indices: Vec<usize>,
        calls: Vec<CodemodeCall>,
        #[serde(rename = "storeWrites")]
        store_writes: CodemodeStoreWrites,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        during: Option<CodemodeDuring>,
    },
    Failure {
        ok: bool,
        error: CodemodeError,
        output: Vec<CodemodeOutputItem>,
        #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "consoleOutputIndices")]
        console_output_indices: Vec<usize>,
        calls: Vec<CodemodeCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        during: Option<CodemodeDuring>,
    },
}

impl CodemodeResult {
    /// Whether the script finished without error.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Success { ok: true, .. })
    }

    /// Output items emitted by `text()`, `console.*`, and `image()`.
    pub fn output(&self) -> &[CodemodeOutputItem] {
        match self {
            Self::Success { output, .. } | Self::Failure { output, .. } => output,
        }
    }

    /// Whether `output[index]` came from `console.*` (`console: true` in upstream TS).
    ///
    /// // PORT: kept out-of-band on `CodemodeResult` so [`CodemodeOutputItem::Text`] keeps its single `text` field.
    pub fn is_console_output(&self, index: usize) -> bool {
        let indices = match self {
            Self::Success {
                console_output_indices, ..
            }
            | Self::Failure {
                console_output_indices, ..
            } => console_output_indices,
        };
        indices.contains(&index)
    }

    /// Recorded tool calls.
    pub fn calls(&self) -> &[CodemodeCall] {
        match self {
            Self::Success { calls, .. } | Self::Failure { calls, .. } => calls,
        }
    }

    /// `during` checkpoint on `start()` runs when ended by the script itself.
    pub fn during(&self) -> Option<&CodemodeDuring> {
        match self {
            Self::Success { during, .. } | Self::Failure { during, .. } => during.as_ref(),
        }
    }
}

/// `CodemodeWasmModule | Promise<CodemodeWasmModule>`.
///
/// // PORT: a JS `Promise` is shared and polled once. [`CodemodeWasmInput::Pending`] holds that future
/// behind an `Arc` so the options value can be cloned the way a JS object holding a Promise can.
#[derive(Clone)]
pub enum CodemodeWasmInput {
    Module(CodemodeWasmModule),
    Pending(Arc<std::sync::Mutex<Option<pi_js::BoxFuture<pi_js::Result<CodemodeWasmModule>>>>>),
}

impl From<CodemodeWasmModule> for CodemodeWasmInput {
    fn from(module: CodemodeWasmModule) -> Self {
        Self::Module(module)
    }
}

impl std::fmt::Debug for CodemodeWasmInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Module(_) => f.write_str("CodemodeWasmInput::Module"),
            Self::Pending(_) => f.write_str("CodemodeWasmInput::Pending"),
        }
    }
}

/// Options every sandbox accepts, wherever its VM runs.
#[derive(Clone, Debug, Default)]
pub struct CodemodeSandboxBaseOptions {
    pub tools: Option<Vec<CodemodeTool>>,
    /// Functions exposed as top-level identifiers instead of on `tools`, for host helpers such as
    /// attaching an image to the result. They behave like tools (JSON round trip, promise result)
    /// but are not recorded in `result.calls`. Names must be identifiers and may not shadow the
    /// built-in globals (`tools`, `ALL_TOOLS`, `console`, `text`, `image`, `exit`, `store`, `load`).
    pub globals: Option<Vec<CodemodeTool>>,
    /// Overall deadline per execution, including time spent in tools. `Infinity` disables the
    /// deadline; the execution then only ends when the script settles or is aborted.
    /// Default: 300000.
    pub timeout_ms: Option<f64>,
    /// Maximum memory the QuickJS VM may allocate. Allocations beyond it fail inside the script as
    /// `InternalError: out of memory`. Default: no limit beyond wasm32's 4 GiB address space.
    pub memory_limit_bytes: Option<f64>,
}

/// // PORT: `string | URL` is a `String`. `timeout_ms` is `f64` because `Infinity` disables the deadline.
#[derive(Clone, Debug, Default)]
pub struct CodemodeSandboxOptions {
    pub tools: Option<Vec<CodemodeTool>>,
    /// Functions exposed as top-level identifiers instead of on `tools`, for host helpers such as
    /// attaching an image to the result. They behave like tools (JSON round trip, promise result)
    /// but are not recorded in `result.calls`. Names must be identifiers and may not shadow the
    /// built-in globals (`tools`, `ALL_TOOLS`, `console`, `text`, `image`, `exit`, `store`, `load`).
    pub globals: Option<Vec<CodemodeTool>>,
    /// Overall deadline per execution, including time spent in tools. `Infinity` disables the
    /// deadline; the execution then only ends when the script settles or is aborted.
    /// Default: 300000.
    pub timeout_ms: Option<f64>,
    /// Maximum memory the QuickJS VM may allocate. Allocations beyond it fail inside the script as
    /// `InternalError: out of memory`. Default: no limit beyond wasm32's 4 GiB address space.
    pub memory_limit_bytes: Option<f64>,
    /// Compiled `quickjs-wasi/quickjs.wasm`, usually from [`load_quick_js_wasm`](crate::load_quick_js_wasm). Default:
    /// `load_quick_js_wasm()`, the embedded wasm bytes. Pass it when the bytes are supplied by the caller.
    pub wasm: Option<CodemodeWasmInput>,
    /// Worker entry that imports `@earendil-works/pi-codemode/worker`. Default: this package's own
    /// worker. Stored for API compatibility; the Rust sandbox runs [`crate::runtime::worker::run`]
    /// on a `std::thread` instead of loading a worker file.
    pub worker_url: Option<String>,
}

impl From<&CodemodeSandboxOptions> for CodemodeSandboxBaseOptions {
    fn from(opts: &CodemodeSandboxOptions) -> Self {
        Self {
            tools: opts.tools.clone(),
            globals: opts.globals.clone(),
            timeout_ms: opts.timeout_ms,
            memory_limit_bytes: opts.memory_limit_bytes,
        }
    }
}

/// Worker concurrency admission and lifetime hooks for [`CodemodeSandbox`](crate::CodemodeSandbox).
///
/// // PORT: extracted into a separate struct passed to `CodemodeSandbox::with_worker_hooks` so existing
/// callers constructing `CodemodeSandboxOptions` literals remain source-compatible.
#[derive(Clone, Default)]
pub struct CodemodeWorkerHooks {
    /// Synchronously reserves capacity immediately before creating each worker, after loading wasm.
    /// Return `Err` to deny admission without starting a worker. Return a release callback, called once if
    /// worker construction fails or when the worker actually exits, not when the result settles.
    /// The release callback must not panic. Reservations can be shared across sandbox instances.
    pub acquire_worker: Option<Arc<dyn Fn() -> pi_js::Result<Box<dyn FnOnce() + Send>> + Send + Sync>>,
    /// Called once for each worker started by `execute()` or `start()`. `exited` resolves when that worker
    /// actually exits, which may be after `result` and `close()` settle. Returning `Err` fails the
    /// execution and requests worker termination.
    pub on_worker_started: Option<Arc<dyn Fn(pi_js::BoxFuture<()>) -> pi_js::Result<()> + Send + Sync>>,
}

impl std::fmt::Debug for CodemodeWorkerHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodemodeWorkerHooks")
            .field("has_acquire_worker", &self.acquire_worker.is_some())
            .field("has_on_worker_started", &self.on_worker_started.is_some())
            .finish()
    }
}

/// A call the script made in a [`CodemodeSandboxBase::start`](crate::CodemodeSandboxBase::start) run,
/// answered with [`CodemodeRun::deliver`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeHostCall {
    /// The VM's own number for the call, `1`, `2`, ... in the order the script made its calls (tools and globals).
    pub id: i64,
    pub target: CodemodeCallTarget,
    pub name: String,
    /// What the script passed (`None` for JS `undefined`, `Some(Value::Null)` for `null`);
    /// for spread globals the argument list as a JSON array.
    pub args: Option<Value>,
}

/// A tool as a [`CodemodeSandboxBase::start`](crate::CodemodeSandboxBase::start) run's script sees it:
/// `tools.<name>` and its `ALL_TOOLS` entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeDeclaredTool {
    pub name: String,
    /// The identifier the script uses, `tools.<jsName>`. Default: `to_codemode_identifier(&name)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A global as a [`CodemodeSandboxBase::start`](crate::CodemodeSandboxBase::start) run's script sees it;
/// the same names as `globals` allow.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeDeclaredGlobal {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread: Option<bool>,
}

/// A call's answer in a [`CodemodeSandboxBase::start`](crate::CodemodeSandboxBase::start) run.
///
/// A rejection becomes an `Error` with `message` in the script, carrying the own properties of
/// `properties` (JSON values; `name`, `message`, and `stack` are ignored), for example `{ structuredOutput }`.
#[derive(Clone, Debug, PartialEq)]
pub enum CodemodeOutcome {
    Ok {
        value: Option<Value>,
    },
    Err {
        message: String,
        properties: Option<IndexMap<String, Value>>,
    },
}

impl CodemodeOutcome {
    /// Successful return value (`None` becomes JS `undefined`, `Some(Value::Null)` becomes `null`).
    pub fn ok(value: Option<Value>) -> Self {
        Self::Ok { value }
    }

    /// Rejection error without extra own properties.
    pub fn err(message: impl Into<String>) -> Self {
        Self::Err {
            message: message.into(),
            properties: None,
        }
    }

    /// Rejection error with additional own properties attached to the `Error` object in JS.
    pub fn err_with_properties(message: impl Into<String>, properties: IndexMap<String, Value>) -> Self {
        Self::Err {
            message: message.into(),
            properties: Some(properties),
        }
    }
}

/// Options for [`CodemodeSandboxBase::start`](crate::CodemodeSandboxBase::start).
#[derive(Clone)]
pub struct CodemodeRunOptions {
    /// The VM clock's start, in ms since the epoch: `Date.now()` until the first result arrives.
    pub started_at: f64,
    /// Seeds `Math.random()` and the VM's other randomness. A 32-bit unsigned integer.
    pub seed: u32,
    pub tools: Option<Vec<CodemodeDeclaredTool>>,
    pub globals: Option<Vec<CodemodeDeclaredGlobal>>,
    pub store: Option<IndexMap<String, Value>>,
    pub signal: Option<pi_js::abort::AbortSignal>,
    /// Deadline of this run in ms. Default: none (not the sandbox's default).
    pub timeout_ms: Option<f64>,
    /// The VM's fingerprint, before the script runs and before any of its calls.
    pub on_started: Option<Arc<dyn Fn(String) + Send + Sync>>,
    /// The script made a call. Answer it with `run.deliver(...)`, now or later.
    pub on_call: Arc<dyn Fn(CodemodeHostCall, CodemodeRun) + Send + Sync>,
    /// The VM processed result `seq` (`0`: the script's first run, before any result) and waits again,
    /// having issued `issued` calls in total.
    pub on_idle: Option<Arc<dyn Fn(i64, i64, CodemodeRun) + Send + Sync>>,
}

impl std::fmt::Debug for CodemodeRunOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodemodeRunOptions")
            .field("started_at", &self.started_at)
            .field("seed", &self.seed)
            .field("tools", &self.tools)
            .field("globals", &self.globals)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

pub(crate) type DeliverFn = Arc<dyn Fn(i64, f64, i64, CodemodeOutcome) -> pi_js::Result<()> + Send + Sync>;

pub(crate) struct CodemodeRunInner {
    pub(crate) finished: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) deliver: std::sync::Mutex<Option<DeliverFn>>,
    pub(crate) result: futures::future::Shared<pi_js::BoxFuture<CodemodeResult>>,
}

/// A running [`CodemodeSandboxBase::start`](crate::CodemodeSandboxBase::start) execution.
#[derive(Clone)]
pub struct CodemodeRun {
    pub(crate) inner: Arc<CodemodeRunInner>,
}

impl CodemodeRun {
    pub(crate) fn from_parts(inner: Arc<CodemodeRunInner>) -> Self {
        Self { inner }
    }

    /// Future resolving to the final [`CodemodeResult`]. Can be polled or cloned freely.
    pub fn result(&self) -> futures::future::Shared<pi_js::BoxFuture<CodemodeResult>> {
        self.inner.result.clone()
    }

    /// Whether the run has its outcome: the script ended, timed out, or failed, or the run was aborted.
    /// Set when that is decided, before `result()` settles; calls can no longer be answered.
    /// It does not mean the VM has exited.
    pub fn finished(&self) -> bool {
        self.inner.finished.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Answer call `id` as result number `seq`.
    ///
    /// Results are numbered `1, 2, ...` without gaps, and the VM applies them strictly in that order,
    /// whatever order they arrive in: after a gap it waits for the missing number. While it processes
    /// this one, its clock reads `now` (ms since the epoch), or the previous value if `now` is earlier.
    /// Returns an error for a call that is not waiting for an answer, a `seq` used before, or a `now`
    /// that is not finite.
    pub fn deliver(&self, seq: i64, now: f64, id: i64, outcome: CodemodeOutcome) -> pi_js::Result<()> {
        let deliver = self
            .inner
            .deliver
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| pi_js::Error::msg(format!("Call {id} is not waiting for an answer")))?;
        deliver(seq, now, id, outcome)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CodemodeExecuteOptions {
    pub signal: Option<pi_js::abort::AbortSignal>,
    /// Overrides the sandbox default for this execution. `Infinity` disables the deadline.
    pub timeout_ms: Option<f64>,
    /// Values the script reads with `load(key)`. Must be JSON-serializable. The script's own
    /// `store()` calls come back as `result.store_writes`; persisting them is up to the caller.
    ///
    /// // PORT: a JS `undefined` map value cannot be stored in [`Value`]; omit the key, as `JSON.stringify` does.
    pub store: Option<IndexMap<String, Value>>,
}

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
    pub execute: Arc<
        dyn Fn(Option<Value>, CodemodeToolContext) -> pi_js::BoxFuture< pi_js::Result<Option<Value>>>
            + Send
            + Sync,
    >,
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
pub enum CodemodeErrorKind {
    /// The script threw or failed to parse. `name` and `stack` come from the script's error.
    #[serde(rename = "script")]
    Script,
    /// The overall deadline expired. The worker was terminated.
    #[serde(rename = "timeout")]
    Timeout,
    /// The caller's signal fired or the sandbox was closed. The worker was terminated.
    #[serde(rename = "aborted")]
    Aborted,
    /// The worker or VM failed outside the script's control (for example a wasm trap or a missing worker file).
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

/// `output` is kept for failed executions too, up to the failure. `exit()` completes with `value: undefined`.
///
/// `value: None` is JS `undefined` (the key is omitted). `Some(Value::Null)` is JSON `null`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodemodeResult {
    Success {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
        output: Vec<CodemodeOutputItem>,
        calls: Vec<CodemodeCall>,
        #[serde(rename = "storeWrites")]
        store_writes: CodemodeStoreWrites,
    },
    Failure {
        ok: bool,
        error: CodemodeError,
        output: Vec<CodemodeOutputItem>,
        calls: Vec<CodemodeCall>,
    },
}

/// `CodemodeWasmModule | Promise<CodemodeWasmModule>`.
///
/// // PORT: a JS `Promise` is shared and polled once. [`CodemodeWasmInput::Pending`] holds that future
/// behind an `Arc` so the options value can be cloned the way a JS object holding a Promise can.
#[derive(Clone)]
pub enum CodemodeWasmInput {
    Module(CodemodeWasmModule),
    Pending(Arc<std::sync::Mutex<Option<pi_js::BoxFuture< pi_js::Result<CodemodeWasmModule>>>>>),
}

impl std::fmt::Debug for CodemodeWasmInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Module(_) => f.write_str("CodemodeWasmInput::Module"),
            Self::Pending(_) => f.write_str("CodemodeWasmInput::Pending"),
        }
    }
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

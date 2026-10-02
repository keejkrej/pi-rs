//! Port of packages/codemode/src/runtime/protocol.ts

#![allow(dead_code, unused_variables)]

use std::sync::Arc;
use std::sync::atomic::AtomicI32;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::CodemodeOutputItem;
use crate::wasm::CodemodeWasmModule;

/// `jsName` is the identifier the script uses; `description` is listed in `ALL_TOOLS`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerToolSpec {
    pub name: String,
    pub js_name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerGlobalSpec {
    pub name: String,
    pub spread: bool,
}

/// Messages between the host and the worker thread. Tool arguments, results, and values cross as
/// JSON strings: the worker passes them into and out of the QuickJS VM as strings and never builds
/// structured values itself.
///
/// // PORT: `interrupt` is an `Arc<AtomicI32>` instead of a `SharedArrayBuffer` of one `Int32`.
/// The host stores a non-zero value before the worker thread is stopped. `memory_limit_bytes` is
/// `f64` because the TS field is a JS `number`.
pub struct WorkerData {
    pub code: String,
    pub tools: Vec<WorkerToolSpec>,
    pub globals: Vec<WorkerGlobalSpec>,
    /// Compiled `quickjs-wasi` module.
    pub wasm: CodemodeWasmModule,
    pub memory_limit_bytes: Option<f64>,
    /// Snapshot for `load()`: key to JSON text.
    pub store: IndexMap<String, String>,
    pub interrupt: Arc<AtomicI32>,
}

/// JSON-encoded `{ name?, message, stack? }` of an error thrown by the script.
pub type ScriptErrorJson = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerCallTarget {
    #[serde(rename = "tool")]
    Tool,
    #[serde(rename = "global")]
    Global,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WorkerToHostMessage {
    #[serde(rename = "call")]
    Call {
        id: i64,
        target: WorkerCallTarget,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<String>,
    },
    #[serde(rename = "output")]
    Output { item: CodemodeOutputItem },
    /// `writes` is a JSON array of `[key, json]` for `store()` and `[key]` for deletions.
    /// `error` is set when `ok` is false. Absent `value` is JS `undefined`.
    #[serde(rename = "done")]
    Done {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        writes: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ScriptErrorJson>,
    },
    /// The VM failed outside the script's control, for example a wasm trap.
    #[serde(rename = "crash")]
    Crash { message: String },
}

/// `payload` is the JSON result when `ok`, otherwise the error message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HostToWorkerMessage {
    #[serde(rename = "result")]
    Result {
        id: i64,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<String>,
    },
}

pub fn is_worker_to_host_message(value: &Value) -> bool {
    todo!("port: is_worker_to_host_message")
}

pub fn is_host_to_worker_message(value: &Value) -> bool {
    todo!("port: is_host_to_worker_message")
}

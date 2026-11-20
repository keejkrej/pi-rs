//! Port of packages/codemode/src/runtime/protocol.ts

use std::sync::Arc;
use std::sync::atomic::AtomicI32;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{CodemodeCallTarget, CodemodeDuring, CodemodeOutputItem};
use crate::wasm::CodemodeWasmModule;

pub type WorkerCallTarget = CodemodeCallTarget;

/// Deterministic execution parameters (`CodemodeSandboxBase::start()`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeterministicConfig {
    pub started_at: f64,
    pub seed: u32,
}

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

/// Everything a VM needs to run one script, independent of where it runs. Plain data, so it can
/// cross a channel or remote RPC boundary unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmStartData {
    pub code: String,
    pub tools: Vec<WorkerToolSpec>,
    pub globals: Vec<WorkerGlobalSpec>,
    pub memory_limit_bytes: Option<f64>,
    /// Snapshot for `load()`: key to JSON text.
    pub store: IndexMap<String, String>,
    /// Run deterministically: UTC, virtual clock starting at `startedAt` that moves only with
    /// delivered results, randomness from `seed`, no `WeakRef`, results applied in `seq` order,
    /// and `started`/`idle` messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deterministic: Option<DeterministicConfig>,
}

/// Messages between the host and the worker thread. Tool arguments, results, and values cross as
/// JSON strings: the worker passes them into and out of the QuickJS VM as strings and never builds
/// structured values itself.
///
/// // PORT: `interrupt` is an `Arc<AtomicI32>` instead of a `SharedArrayBuffer` of one `Int32`.
/// The host stores a non-zero value before the worker thread is stopped. `memory_limit_bytes` is
/// `f64` because the TS field is a JS `number`. `tz` carries any thread-local `TZ` override from
/// the spawning thread so host-mode `Date` matches the caller's environment.
#[derive(Clone)]
pub struct WorkerData {
    pub code: String,
    pub tools: Vec<WorkerToolSpec>,
    pub globals: Vec<WorkerGlobalSpec>,
    /// Compiled `quickjs-wasi` module.
    pub wasm: CodemodeWasmModule,
    pub memory_limit_bytes: Option<f64>,
    /// Snapshot for `load()`: key to JSON text.
    pub store: IndexMap<String, String>,
    pub deterministic: Option<DeterministicConfig>,
    pub interrupt: Arc<AtomicI32>,
    pub tz: Option<String>,
}

impl From<&WorkerData> for VmStartData {
    fn from(data: &WorkerData) -> Self {
        Self {
            code: data.code.clone(),
            tools: data.tools.clone(),
            globals: data.globals.clone(),
            memory_limit_bytes: data.memory_limit_bytes,
            store: data.store.clone(),
            deterministic: data.deterministic.clone(),
        }
    }
}

/// JSON-encoded `{ name?, message, stack? }` of an error thrown by the script.
pub type ScriptErrorJson = String;

fn is_false(value: &bool) -> bool {
    !*value
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
    Output {
        item: CodemodeOutputItem,
        #[serde(default, skip_serializing_if = "is_false")]
        console: bool,
    },
    /// `writes` is a JSON array of `[key, json]` for `store()` and `[key]` for deletions.
    /// `error` is set when `ok` is false. Absent `value` is JS `undefined`.
    /// Deterministic mode: `during` is the result the VM was processing when the script finished
    /// (`0`: its first run) and the calls issued by then.
    #[serde(rename = "done")]
    Done {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        writes: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ScriptErrorJson>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        during: Option<CodemodeDuring>,
    },
    /// The VM failed outside the script's control, for example a wasm trap.
    #[serde(rename = "crash")]
    Crash { message: String },
    /// Deterministic mode, before the script runs: what the VM is, so a replay can refuse a different one.
    #[serde(rename = "started")]
    Started { fingerprint: String },
    /// Deterministic mode, after the script waits again: having processed result `seq` (`0`: the
    /// script's first run), it has issued `issued` calls in total.
    #[serde(rename = "idle")]
    Idle { seq: i64, issued: i64 },
}

/// `payload` is the JSON result when `ok`, otherwise the error message. Deterministic mode: `seq`
/// numbers the results from `1` in the order the VM applies them, and the VM's clock reads `now`
/// while it processes this one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HostToWorkerMessage {
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
}

pub fn is_worker_to_host_message(value: &Value) -> bool {
    matches!(
        value
            .as_object()
            .and_then(|obj| obj.get("type"))
            .and_then(Value::as_str),
        Some("call" | "output" | "done" | "crash" | "started" | "idle")
    )
}

pub fn is_host_to_worker_message(value: &Value) -> bool {
    matches!(
        value
            .as_object()
            .and_then(|obj| obj.get("type"))
            .and_then(Value::as_str),
        Some("result")
    )
}

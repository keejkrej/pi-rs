//! Port of packages/codemode/src/wasm.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use indexmap::IndexMap;

/// Embedded `quickjs-wasi/quickjs.wasm` (PORTING.md §2.9).
pub(crate) const QUICKJS_WASM: &[u8] = include_bytes!("../assets/quickjs.wasm");

/// A compiled `quickjs-wasi/quickjs.wasm`.
///
/// // PORT: opaque stand-in for `WebAssembly.Module`. The bytes compile to [`wasmtime::Module`]
/// once per path (the default module once per process).
#[derive(Clone)]
pub struct CodemodeWasmModule {
    pub(crate) module: Arc<wasmtime::Module>,
}

/// Default module, compiled once per process.
static QUICKJS_MODULE: OnceLock<CodemodeWasmModule> = OnceLock::new();

/// Compiled modules keyed by path. A failed load is not inserted, so the next call retries.
static MODULES: LazyLock<Mutex<IndexMap<String, CodemodeWasmModule>>> = LazyLock::new(|| Mutex::new(IndexMap::new()));

/// Read and compile the QuickJS wasm once per path. `path` defaults to the embedded
/// `quickjs-wasi` bytes; pass it when the file lives elsewhere. A failed load is retried on the next call.
///
/// // PORT: TS `async` stays `async fn`. Compilation itself is synchronous wasmtime.
pub async fn load_quick_js_wasm(path: Option<&str>) -> pi_js::Result<CodemodeWasmModule> {
    todo!("port: load_quick_js_wasm")
}

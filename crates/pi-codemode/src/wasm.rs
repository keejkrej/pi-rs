//! Port of packages/codemode/src/wasm.ts

use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use indexmap::IndexMap;

/// Embedded `quickjs-wasi/quickjs.wasm` (PORTING.md §2.9).
pub(crate) const QUICKJS_WASM: &[u8] = include_bytes!("../assets/quickjs.wasm");

/// Shared wasmtime engine for QuickJS modules.
pub(crate) static WASM_ENGINE: LazyLock<wasmtime::Engine> = LazyLock::new(|| {
    let mut config = wasmtime::Config::new();
    // QuickJS enforces `MAX_STACK_SIZE` (512 KiB) against WASM linear-memory stack frames, whose
    // Cranelift native frames can exceed wasmtime's default 512 KiB host-stack limit before
    // QuickJS throws `RangeError`. Keep native headroom well above `MAX_STACK_SIZE`.
    config.max_wasm_stack(2 * 1024 * 1024);
    wasmtime::Engine::new(&config).expect("valid wasmtime config")
});

/// A compiled `quickjs-wasi/quickjs.wasm`.
///
/// // PORT: opaque stand-in for `WebAssembly.Module`. The bytes compile to [`wasmtime::Module`]
/// once per path (the default module once per process).
#[derive(Clone)]
pub struct CodemodeWasmModule {
    pub(crate) module: Arc<wasmtime::Module>,
}

impl std::fmt::Debug for CodemodeWasmModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodemodeWasmModule").finish_non_exhaustive()
    }
}

impl CodemodeWasmModule {
    /// Compile raw QuickJS WASM bytes using the shared engine.
    pub fn from_bytes(bytes: &[u8]) -> pi_js::Result<Self> {
        let module = wasmtime::Module::from_binary(&WASM_ENGINE, bytes)
            .map_err(|err| pi_js::Error::msg(format!("Failed to compile QuickJS wasm: {err}")))?;
        Ok(Self {
            module: Arc::new(module),
        })
    }
}

/// Default module, compiled once per process.
static QUICKJS_MODULE: OnceLock<CodemodeWasmModule> = OnceLock::new();
static QUICKJS_INIT_LOCK: Mutex<()> = Mutex::new(());

/// Compiled modules keyed by path. A failed load is not inserted, so the next call retries.
static MODULES: LazyLock<Mutex<IndexMap<String, CodemodeWasmModule>>> = LazyLock::new(|| Mutex::new(IndexMap::new()));

/// Read and compile the QuickJS wasm once per path. `path` defaults to the embedded
/// `quickjs-wasi` bytes; pass it when the file lives elsewhere. A failed load is retried on the next call.
///
/// // PORT: TS `async` stays `async fn`. Compilation itself is synchronous wasmtime.
pub async fn load_quick_js_wasm(path: Option<&str>) -> pi_js::Result<CodemodeWasmModule> {
    let Some(path) = path else {
        if let Some(cached) = QUICKJS_MODULE.get() {
            return Ok(cached.clone());
        }
        let _guard = QUICKJS_INIT_LOCK.lock().unwrap();
        if let Some(cached) = QUICKJS_MODULE.get() {
            return Ok(cached.clone());
        }
        let compiled = CodemodeWasmModule::from_bytes(QUICKJS_WASM)?;
        let _ = QUICKJS_MODULE.set(compiled.clone());
        return Ok(compiled);
    };

    if let Some(cached) = MODULES.lock().unwrap().get(path).cloned() {
        return Ok(cached);
    }
    let bytes = pi_js::fs::promises::read_file(path).await?;
    let compiled = CodemodeWasmModule::from_bytes(&bytes)?;
    MODULES.lock().unwrap().insert(path.to_string(), compiled.clone());
    Ok(compiled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_imports_and_exports_match_quickjs_wasi() {
        let module = wasmtime::Module::from_binary(&WASM_ENGINE, QUICKJS_WASM).unwrap();
        let imports: Vec<String> = module
            .imports()
            .map(|imp| format!("{}::{}", imp.module(), imp.name()))
            .collect();
        assert!(imports.contains(&"env::host_call".to_string()));
        assert!(imports.contains(&"env::host_interrupt".to_string()));
        assert!(imports.contains(&"wasi_snapshot_preview1::fd_write".to_string()));
    }
}

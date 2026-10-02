//! Port of npm `quickjs-wasi` 3.6.2 (node_modules/quickjs-wasi); vendored per PORTING.md §2.6.

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use crate::wasm::CodemodeWasmModule;

/// Largest supported QuickJS native stack limit for the shipped WASM binary.
///
/// The binary has a 1 MiB linker-defined stack; reserving half of it leaves
/// headroom for native frames and stack-overflow exception handling.
pub const MAX_STACK_SIZE: i64 = 512 * 1024;

/// Live wasm linear memory seen by a WASI override. `discard_output` reads and writes `u32`s in it.
///
/// // PORT: the WASI shim itself is not ported. This is only the memory view `packages/codemode` touches.
#[derive(Clone, Debug)]
pub struct WasiMemory {
    inner: Arc<WasiMemoryInner>,
}

#[derive(Debug)]
struct WasiMemoryInner {}

impl WasiMemory {
    pub fn read_u32_le(&self, ptr: i64) -> i64 {
        todo!("port: WasiMemory::read_u32_le")
    }

    pub fn write_u32_le(&self, ptr: i64, value: i64) {
        todo!("port: WasiMemory::write_u32_le")
    }
}

/// `fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno`.
pub type WasiFdWrite = Arc<dyn Fn(&WasiMemory, i64, i64, i64, i64) -> i64 + Send + Sync>;

/// Overrides the caller replaces on `wasi_snapshot_preview1`. Only `fd_write` is used.
#[derive(Clone, Default)]
pub struct WasiOverrides {
    pub fd_write: Option<WasiFdWrite>,
}

/// `(memory) => overrides`, matching `WasiOptions` from `quickjs-wasi`.
pub type WasiOptions = Arc<dyn Fn(&WasiMemory) -> WasiOverrides + Send + Sync>;

/// Host callback installed with [`QuickJS::new_function`].
///
/// `this` is the JS `this` handle. `args` are the handles the trampoline passed; a missing
/// argument is absent from the slice (TS `=== undefined`), and a guest `undefined` is a handle
/// whose [`JSValueHandle::is_undefined`] is true.
pub type HostFunction = Arc<dyn Fn(&JSValueHandle, &[JSValueHandle]) -> pi_js::Result<JSValueHandle> + Send + Sync>;

/// Options [`packages/codemode`](crate) passes to [`QuickJS::create`].
///
/// // PORT: `create` is synchronous. TS returns a `Promise` because `WebAssembly.instantiate` is async;
/// wasmtime instantiation is not, and the worker runs on a `std::thread`.
pub struct QuickJSOptions {
    pub wasm: CodemodeWasmModule,
    pub memory_limit: Option<f64>,
    pub max_stack_size: Option<i64>,
    pub interrupt_handler: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub wasi: Option<WasiOptions>,
}

/// A batch of handles created inside [`QuickJS::with_scope`], disposed together when the scope ends.
#[derive(Debug)]
pub struct HandleScope<'a> {
    vm: &'a QuickJS,
}

impl HandleScope<'_> {
    /// Remove a handle from the scope so that it outlives it.
    pub fn escape(&self, handle: JSValueHandle) -> JSValueHandle {
        todo!("port: HandleScope::escape")
    }
}

/// A QuickJS VM.
///
/// Methods below are the ones `packages/codemode` calls. Failures that TS throws are [`pi_js::Result`];
/// a guest exception is [`pi_js::Error::typed`]`("JSException", JSException)`.
#[derive(Clone, Debug)]
pub struct QuickJS {
    inner: Arc<QuickJSInner>,
}

#[derive(Debug)]
struct QuickJSInner;

impl QuickJS {
    pub fn create(options: QuickJSOptions) -> pi_js::Result<Self> {
        todo!("port: QuickJS::create")
    }

    /// Evaluate JavaScript code and return the result as a handle.
    /// `filename` defaults to `"<eval>"` inside the body. `flags` is a bitwise OR of `EvalFlags`.
    pub fn eval_code(&self, code: &str, filename: Option<&str>, flags: Option<i64>) -> pi_js::Result<JSValueHandle> {
        todo!("port: QuickJS::eval_code")
    }

    /// Call a QuickJS function. If the function throws, the `Err` is a `JSException`.
    pub fn call_function(
        &self,
        func: &JSValueHandle,
        this_val: &JSValueHandle,
        args: &[JSValueHandle],
    ) -> pi_js::Result<JSValueHandle> {
        todo!("port: QuickJS::call_function")
    }

    /// Execute all pending microtask jobs. Returns the number of jobs executed.
    pub fn execute_pending_jobs(&self) -> pi_js::Result<i64> {
        todo!("port: QuickJS::execute_pending_jobs")
    }

    pub fn new_string(&self, str: &str) -> pi_js::Result<JSValueHandle> {
        todo!("port: QuickJS::new_string")
    }

    pub fn new_number(&self, num: f64) -> pi_js::Result<JSValueHandle> {
        todo!("port: QuickJS::new_number")
    }

    /// Create a QuickJS function backed by a host callback. The callback stays registered for the life of the VM.
    pub fn new_function(&self, name: &str, func: HostFunction) -> pi_js::Result<JSValueHandle> {
        todo!("port: QuickJS::new_function")
    }

    /// The undefined value. Cached; do not dispose.
    pub fn undefined(&self) -> pi_js::Result<JSValueHandle> {
        todo!("port: QuickJS::undefined")
    }

    /// The true value. Cached; do not dispose.
    pub fn r#true(&self) -> pi_js::Result<JSValueHandle> {
        todo!("port: QuickJS::true")
    }

    /// The false value. Cached; do not dispose.
    pub fn r#false(&self) -> pi_js::Result<JSValueHandle> {
        todo!("port: QuickJS::false")
    }

    /// Run `func` with a handle scope. Handles created during the call are disposed when it returns,
    /// except those passed to [`HandleScope::escape`]. `func` is synchronous.
    pub fn with_scope<T>(&self, func: impl FnOnce(&HandleScope<'_>) -> T) -> T {
        todo!("port: QuickJS::with_scope")
    }
}

/// An exception thrown from QuickJS code.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct JSException {
    name: String,
    message: String,
    stack: Option<String>,
    /// A live handle to the QuickJS exception value. Must be disposed when done.
    pub handle: JSValueHandle,
}

impl JSException {
    pub fn new(handle: JSValueHandle) -> Self {
        todo!("port: JSException::new")
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn stack(&self) -> Option<&str> {
        self.stack.as_deref()
    }

    pub fn dispose(&self) {
        todo!("port: JSException::dispose")
    }
}

/// A handle to a JSValue inside the QuickJS WASM instance.
#[derive(Clone, Debug)]
pub struct JSValueHandle {
    inner: Arc<JSValueHandleInner>,
}

#[derive(Debug)]
struct JSValueHandleInner {
    vm: QuickJS,
    disposed: bool,
}

impl JSValueHandle {
    pub fn is_undefined(&self) -> bool {
        todo!("port: JSValueHandle::is_undefined")
    }

    /// JavaScript truthiness (`!!value`).
    pub fn to_boolean(&self) -> pi_js::Result<bool> {
        todo!("port: JSValueHandle::to_boolean")
    }

    pub fn to_number(&self) -> pi_js::Result<f64> {
        todo!("port: JSValueHandle::to_number")
    }

    /// JavaScript string conversion. May run guest `toString` / `valueOf`.
    pub fn to_string(&self) -> pi_js::Result<String> {
        todo!("port: JSValueHandle::to_string")
    }

    pub fn get_prop(&self, name: &str) -> pi_js::Result<Self> {
        todo!("port: JSValueHandle::get_prop")
    }

    /// Dispose this handle. Safe to call after the VM has been disposed (a no-op).
    pub fn dispose(&self) {
        todo!("port: JSValueHandle::dispose")
    }
}

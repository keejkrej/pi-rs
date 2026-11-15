//! Port of npm `quickjs-wasi` 3.6.2 (node_modules/quickjs-wasi); vendored per PORTING.md §2.6.

use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use indexmap::IndexMap;
use serde_json::Value;
use wasmtime::{Caller, Global, Linker, Memory, Store, StoreContextMut, TypedFunc};

use crate::wasm::{CodemodeWasmModule, WASM_ENGINE};

/// Largest supported QuickJS native stack limit for the shipped WASM binary.
///
/// The binary has a 1 MiB linker-defined stack; reserving half of it leaves
/// headroom for native frames and stack-overflow exception handling.
pub const MAX_STACK_SIZE: i64 = 512 * 1024;

static NEXT_VM_ID: AtomicUsize = AtomicUsize::new(1);

/// Flags for [`QuickJS::eval_code`], matching QuickJS `JS_EVAL_*` constants.
#[allow(non_snake_case)]
pub mod EvalFlags {
    /// Global script mode (default).
    pub const TYPE_GLOBAL: i64 = 0;
    /// Module mode: returns a handle to a Promise resolving to the module namespace.
    pub const TYPE_MODULE: i64 = 1 << 0;
    /// Force strict mode.
    pub const STRICT: i64 = 1 << 3;
    /// Compile only; do not execute.
    pub const COMPILE_ONLY: i64 = 1 << 5;
    /// Omit stack frames before this eval from Error backtraces.
    pub const BACKTRACE_BARRIER: i64 = 1 << 6;
    /// Allow top-level `await` in global scripts.
    pub const ASYNC: i64 = 1 << 7;
}

/// Flags for [`QuickJS::compile`], matching QuickJS bytecode stripping constants.
#[allow(non_snake_case)]
pub mod CompileFlags {
    /// Strip source code from bytecode output.
    pub const STRIP_SOURCE: i64 = 1 << 4;
    /// Strip debug metadata (line numbers) from bytecode output.
    pub const STRIP_DEBUG: i64 = 1 << 5;
}

/// Intrinsic flags for [`QuickJS::create_with_intrinsics`], matching `quickjs-wasi`'s `Intrinsics`.
#[allow(non_snake_case)]
pub mod Intrinsics {
    pub const DATE: u32 = 1 << 0;
    pub const EVAL: u32 = 1 << 1;
    pub const REGEXP: u32 = 1 << 2;
    pub const JSON: u32 = 1 << 3;
    pub const PROXY: u32 = 1 << 4;
    pub const MAP_SET: u32 = 1 << 5;
    pub const TYPED_ARRAYS: u32 = 1 << 6;
    pub const PROMISE: u32 = 1 << 7;
    pub const BIG_INT: u32 = 1 << 8;
    pub const WEAK_REF: u32 = 1 << 9;
    pub const PERFORMANCE: u32 = 1 << 10;
    pub const DOM_EXCEPTION: u32 = 1 << 11;
    pub const ATOB_BTOA: u32 = 1 << 12;
    pub const ALL: u32 = u32::MAX;
}

// -----------------------------------------------------------------------------
// Re-entrant wasmtime Store / Caller bridge
// -----------------------------------------------------------------------------

trait WasmCallSite {
    fn as_ctx_mut(&mut self) -> StoreContextMut<'_, HostState>;
}

impl WasmCallSite for Store<HostState> {
    fn as_ctx_mut(&mut self) -> StoreContextMut<'_, HostState> {
        <Store<HostState> as wasmtime::AsContextMut>::as_context_mut(self)
    }
}

impl WasmCallSite for Caller<'_, HostState> {
    fn as_ctx_mut(&mut self) -> StoreContextMut<'_, HostState> {
        <Caller<'_, HostState> as wasmtime::AsContextMut>::as_context_mut(self)
    }
}

thread_local! {
    /// Holds the active `(vm_id, &mut dyn WasmCallSite)` while a WASM import callback is
    /// executing synchronously on this thread so `QuickJS` / `JSValueHandle` / `WasiMemory`
    /// methods called inside callbacks can re-enter WASM without deadlocking `store`.
    static ACTIVE_SITE: Cell<Option<(usize, NonNull<dyn WasmCallSite>)>> = const { Cell::new(None) };
}

struct SiteResetGuard(Option<(usize, NonNull<dyn WasmCallSite>)>);

impl Drop for SiteResetGuard {
    fn drop(&mut self) {
        ACTIVE_SITE.with(|cell| cell.set(self.0));
    }
}

fn enter_caller<R>(vm_id: usize, caller: &mut Caller<'_, HostState>, f: impl FnOnce() -> R) -> R {
    let raw: *mut (dyn WasmCallSite + '_) = caller;
    // SAFETY: `raw` points to a live `Caller` stack frame above `f` on this exact thread.
    // The `SiteResetGuard` restores the previous `ACTIVE_SITE` before `caller` leaves scope,
    // and `with_site` ensures exclusive `&mut` access via `take()` / `SiteResetGuard`.
    let erased: NonNull<dyn WasmCallSite> = unsafe {
        let static_raw: *mut (dyn WasmCallSite + 'static) = std::mem::transmute(raw);
        NonNull::new_unchecked(static_raw)
    };
    let prev = ACTIVE_SITE.with(|cell| cell.replace(Some((vm_id, erased))));
    let _guard = SiteResetGuard(prev);
    f()
}

// -----------------------------------------------------------------------------
// WASI types
// -----------------------------------------------------------------------------

/// Live wasm linear memory seen by a WASI override.
#[derive(Clone, Debug)]
pub struct WasiMemory {
    inner: Arc<WasiMemoryInner>,
}

#[derive(Debug)]
struct WasiMemoryInner {
    vm: Weak<QuickJSInner>,
}

impl WasiMemory {
    pub fn read_u32_le(&self, ptr: i64) -> i64 {
        let Some(vm_inner) = self.inner.vm.upgrade() else {
            return 0;
        };
        let Ok(offset) = u32::try_from(ptr) else {
            return 0;
        };
        QuickJS { inner: vm_inner }
            .with_memory_read(offset, 4, |slice| {
                i64::from(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
            })
            .unwrap_or(0)
    }

    pub fn write_u32_le(&self, ptr: i64, value: i64) {
        let Some(vm_inner) = self.inner.vm.upgrade() else {
            return;
        };
        let Ok(offset) = u32::try_from(ptr) else {
            return;
        };
        let bytes = (value as u32).to_le_bytes();
        let _ = QuickJS { inner: vm_inner }.with_memory_write(offset, &bytes);
    }

    pub fn read_bytes(&self, ptr: u32, len: usize) -> Vec<u8> {
        let Some(vm_inner) = self.inner.vm.upgrade() else {
            return Vec::new();
        };
        QuickJS { inner: vm_inner }
            .with_memory_read(ptr, len, |slice| slice.to_vec())
            .unwrap_or_default()
    }

    pub fn write_bytes(&self, ptr: u32, bytes: &[u8]) -> bool {
        let Some(vm_inner) = self.inner.vm.upgrade() else {
            return false;
        };
        QuickJS { inner: vm_inner }.with_memory_write(ptr, bytes).is_ok()
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

/// Synchronous ES module loader hooks (`module_loader`).
#[derive(Clone)]
pub struct ModuleLoader {
    /// Optional specifier resolver `(base_name, specifier) -> normalized_name`.
    pub normalize: Option<Arc<dyn Fn(&str, &str) -> pi_js::Result<String> + Send + Sync>>,
    /// Synchronous source loader `(module_name) -> js_source`.
    pub load: Arc<dyn Fn(&str) -> pi_js::Result<String> + Send + Sync>,
}

/// Unhandled promise rejection notification `(promise, reason, is_handled)`.
pub type PromiseRejectionHandler = Arc<dyn Fn(&JSValueHandle, &JSValueHandle, bool) + Send + Sync>;

/// Property attributes for `define_prop`. All default to `false`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JSPropertyDescriptor {
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
}

/// Returned by [`JSValueHandle::get_own_property_descriptor`].
#[derive(Debug)]
pub struct JSOwnPropertyDescriptor {
    pub value: Option<JSValueHandle>,
    pub get: Option<JSValueHandle>,
    pub set: Option<JSValueHandle>,
    pub writable: Option<bool>,
    pub enumerable: bool,
    pub configurable: bool,
}

/// QuickJS promise settlement state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

/// Result of inspecting a settled QuickJS promise.
#[derive(Debug)]
pub enum PromiseResult {
    Pending,
    Fulfilled(JSValueHandle),
    Rejected(JSValueHandle),
}

/// A QuickJS promise alongside its `resolve` / `reject` capability handles.
pub struct JSDeferred {
    pub handle: JSValueHandle,
    resolve_fn: JSValueHandle,
    reject_fn: JSValueHandle,
}

impl JSDeferred {
    /// Resolve the promise with `value` and dispose the internal resolve handle.
    pub fn resolve(&self, vm: &QuickJS, value: &JSValueHandle) -> pi_js::Result<()> {
        let undef = vm.undefined()?;
        let res = vm.call_function_raw(&self.resolve_fn, &undef, std::slice::from_ref(value))?;
        res.dispose();
        self.resolve_fn.dispose();
        self.reject_fn.dispose();
        Ok(())
    }

    /// Reject the promise with `reason` and dispose the internal reject handle.
    pub fn reject(&self, vm: &QuickJS, reason: &JSValueHandle) -> pi_js::Result<()> {
        let undef = vm.undefined()?;
        let res = vm.call_function_raw(&self.reject_fn, &undef, std::slice::from_ref(reason))?;
        res.dispose();
        self.resolve_fn.dispose();
        self.reject_fn.dispose();
        Ok(())
    }

    /// Dispose the resolve/reject capability handles and the promise handle.
    pub fn dispose(&self) {
        self.resolve_fn.dispose();
        self.reject_fn.dispose();
        self.handle.dispose();
    }
}

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

// -----------------------------------------------------------------------------
// HandleScope
// -----------------------------------------------------------------------------

/// A batch of handles created inside [`QuickJS::with_scope`], disposed together when the scope ends.
#[derive(Debug)]
pub struct HandleScope<'a> {
    vm: &'a QuickJS,
}

impl HandleScope<'_> {
    /// Remove a handle from the scope so that it outlives it (or moves to an enclosing scope).
    pub fn escape(&self, handle: JSValueHandle) -> JSValueHandle {
        self.vm.escape_from_scope(&handle);
        handle
    }
}

struct ScopeExitGuard<'a> {
    vm: &'a QuickJS,
}

impl Drop for ScopeExitGuard<'_> {
    fn drop(&mut self) {
        self.vm.pop_and_dispose_scope();
    }
}

// -----------------------------------------------------------------------------
// WASM Exports + VM Internal State
// -----------------------------------------------------------------------------

struct HostState {
    vm_id: usize,
    vm: Weak<QuickJSInner>,
    wasi_memory: WasiMemory,
    fd_write_override: Option<WasiFdWrite>,
}

struct WasmExports {
    memory: Memory,
    _stack_pointer: Option<Global>,
    _initialize: TypedFunc<(), ()>,
    qjs_init: TypedFunc<(), i32>,
    qjs_init2: TypedFunc<i32, i32>,
    qjs_set_interrupt_handler: TypedFunc<i32, ()>,
    qjs_set_promise_rejection_handler: TypedFunc<i32, ()>,
    qjs_set_module_loader: TypedFunc<i32, ()>,
    qjs_set_memory_limit: TypedFunc<i32, ()>,
    qjs_set_max_stack_size: TypedFunc<i32, ()>,
    qjs_run_gc: TypedFunc<(), ()>,
    qjs_destroy: TypedFunc<(), ()>,
    qjs_eval: TypedFunc<(i32, i32, i32, i32), i32>,
    qjs_compile: TypedFunc<(i32, i32, i32, i32, i32, i32), i32>,
    qjs_eval_bytecode: TypedFunc<(i32, i32), i32>,
    qjs_new_string: TypedFunc<(i32, i32), i32>,
    qjs_new_number: TypedFunc<f64, i32>,
    qjs_new_object: TypedFunc<(), i32>,
    qjs_new_array: TypedFunc<(), i32>,
    qjs_get_undefined: TypedFunc<(), i32>,
    qjs_get_null: TypedFunc<(), i32>,
    qjs_get_true: TypedFunc<(), i32>,
    qjs_get_false: TypedFunc<(), i32>,
    qjs_new_symbol: TypedFunc<(i32, i32, i32), i32>,
    qjs_new_big_int64: TypedFunc<(i32, i32), i32>,
    qjs_get_big_int64: TypedFunc<(i32, i32, i32), i32>,
    qjs_get_float64: TypedFunc<i32, f64>,
    qjs_get_string_len: TypedFunc<(i32, i32), i32>,
    qjs_free_cstring: TypedFunc<i32, ()>,
    qjs_is_exception: TypedFunc<i32, i32>,
    qjs_is_undefined: TypedFunc<i32, i32>,
    qjs_is_null: TypedFunc<i32, i32>,
    qjs_is_bool: TypedFunc<i32, i32>,
    qjs_is_number: TypedFunc<i32, i32>,
    qjs_is_string: TypedFunc<i32, i32>,
    qjs_is_object: TypedFunc<i32, i32>,
    qjs_is_array: TypedFunc<i32, i32>,
    qjs_is_function: TypedFunc<i32, i32>,
    qjs_is_error: TypedFunc<i32, i32>,
    qjs_is_promise: TypedFunc<i32, i32>,
    qjs_is_symbol: TypedFunc<i32, i32>,
    qjs_is_big_int: TypedFunc<i32, i32>,
    qjs_is_proxy: TypedFunc<i32, i32>,
    qjs_is_array_buffer: TypedFunc<i32, i32>,
    qjs_get_bool: TypedFunc<i32, i32>,
    qjs_dup_value: TypedFunc<i32, i32>,
    qjs_free_value: TypedFunc<i32, ()>,
    qjs_get_global: TypedFunc<(), i32>,
    qjs_get_prop_string: TypedFunc<(i32, i32), i32>,
    qjs_set_prop_string: TypedFunc<(i32, i32, i32), i32>,
    qjs_define_prop_string: TypedFunc<(i32, i32, i32, i32), i32>,
    qjs_define_prop_value: TypedFunc<(i32, i32, i32, i32), i32>,
    qjs_get_prop_value: TypedFunc<(i32, i32), i32>,
    qjs_set_prop_value: TypedFunc<(i32, i32, i32), i32>,
    qjs_get_prop_uint32: TypedFunc<(i32, i32), i32>,
    qjs_set_prop_uint32: TypedFunc<(i32, i32, i32), i32>,
    qjs_call_constructor: TypedFunc<(i32, i32, i32), i32>,
    qjs_call: TypedFunc<(i32, i32, i32, i32), i32>,
    qjs_new_host_function: TypedFunc<(i32, i32, i32), i32>,
    qjs_new_promise: TypedFunc<(i32, i32), i32>,
    qjs_promise_state: TypedFunc<i32, i32>,
    qjs_promise_result: TypedFunc<i32, i32>,
    qjs_promise_then: TypedFunc<(i32, i32, i32), i32>,
    qjs_promise_mark_as_handled: TypedFunc<i32, ()>,
    qjs_is_job_pending: TypedFunc<(), i32>,
    qjs_execute_pending_job: TypedFunc<(), i32>,
    qjs_get_exception: TypedFunc<(), i32>,
    qjs_throw: TypedFunc<i32, i32>,
    qjs_new_error: TypedFunc<(), i32>,
    qjs_get_own_property_names: TypedFunc<i32, i32>,
    qjs_get_own_property_names_all: TypedFunc<i32, i32>,
    qjs_get_own_property_descriptor: TypedFunc<(i32, i32), i32>,
    qjs_has_own_property: TypedFunc<(i32, i32), i32>,
    qjs_get_prototype_of: TypedFunc<i32, i32>,
    qjs_new_array_buffer: TypedFunc<(i32, i32), i32>,
    qjs_get_array_buffer: TypedFunc<(i32, i32), i32>,
    qjs_new_uint8_array: TypedFunc<(i32, i32), i32>,
    qjs_get_typed_array_buffer: TypedFunc<(i32, i32, i32, i32), i32>,
    wasm_malloc: TypedFunc<i32, i32>,
    wasm_free: TypedFunc<i32, ()>,
}

struct QuickJSInner {
    id: usize,
    disposed: AtomicBool,
    store: Mutex<Option<Store<HostState>>>,
    exports: WasmExports,
    host_callbacks: Mutex<IndexMap<String, HostFunction>>,
    next_internal_id: AtomicUsize,
    interrupt_handler: Mutex<Option<Arc<dyn Fn() -> bool + Send + Sync>>>,
    unhandled_rejection_handler: Mutex<Option<PromiseRejectionHandler>>,
    module_loader: Mutex<Option<ModuleLoader>>,
    timezone_offset_handler: Mutex<Option<Arc<dyn Fn(i64) -> i32 + Send + Sync>>>,
    scopes: Mutex<Vec<Vec<Arc<JSValueHandleInner>>>>,
    cached_global: Mutex<Option<JSValueHandle>>,
    cached_undefined: Mutex<Option<JSValueHandle>>,
    cached_null: Mutex<Option<JSValueHandle>>,
    cached_true: Mutex<Option<JSValueHandle>>,
    cached_false: Mutex<Option<JSValueHandle>>,
    _wasi_memory: WasiMemory,
}

impl std::fmt::Debug for QuickJSInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuickJSInner")
            .field("id", &self.id)
            .field("disposed", &self.disposed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Drop for QuickJSInner {
    fn drop(&mut self) {
        self.disposed.store(true, Ordering::Release);
        *self.store.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// A QuickJS VM.
///
/// Methods below are the ones `packages/codemode` and plugins call. Failures that TS throws are
/// [`pi_js::Result`]; a guest exception is [`pi_js::Error::typed`]`("JSException", JSException)`.
#[derive(Clone, Debug)]
pub struct QuickJS {
    inner: Arc<QuickJSInner>,
}

// -----------------------------------------------------------------------------
// WASI & env Linker Wiring
// -----------------------------------------------------------------------------

const WASI_ERRNO_SUCCESS: i32 = 0;
const WASI_ERRNO_BADF: i32 = 8;
const WASI_ERRNO_INVAL: i32 = 28;
const WASI_ERRNO_IO: i32 = 29;
const WASI_ERRNO_NOSYS: i32 = 52;
const WASI_CLOCK_REALTIME: i32 = 0;
const WASI_CLOCK_MONOTONIC: i32 = 1;

fn wire_linker(linker: &mut Linker<HostState>) -> pi_js::Result<()> {
    let map_err = |err: wasmtime::Error| pi_js::Error::msg(format!("Failed to register WASM import: {err}"));

    // env::host_call(namePtr, nameLen, thisPtr, argc, argvPtr) -> ptr
    linker
        .func_wrap(
            "env",
            "host_call",
            |mut caller: Caller<'_, HostState>,
             name_ptr: i32,
             name_len: i32,
             this_ptr: i32,
             argc: i32,
             argv_ptr: i32|
             -> i32 {
                let state = caller.data();
                let vm_id = state.vm_id;
                let Some(vm_inner) = state.vm.upgrade() else {
                    return 0;
                };
                let vm = QuickJS { inner: vm_inner };
                enter_caller(vm_id, &mut caller, || {
                    vm.handle_host_call(name_ptr, name_len, this_ptr, argc, argv_ptr)
                })
            },
        )
        .map_err(map_err)?;

    // env::host_interrupt() -> i32
    linker
        .func_wrap("env", "host_interrupt", |mut caller: Caller<'_, HostState>| -> i32 {
            let state = caller.data();
            let vm_id = state.vm_id;
            let Some(vm_inner) = state.vm.upgrade() else {
                return 0;
            };
            let vm = QuickJS { inner: vm_inner };
            enter_caller(vm_id, &mut caller, || {
                let handler = vm
                    .inner
                    .interrupt_handler
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                match handler {
                    Some(cb) if cb() => 1,
                    _ => 0,
                }
            })
        })
        .map_err(map_err)?;

    // env::host_promise_rejection(promisePtr, reasonPtr, isHandled) -> ()
    linker
        .func_wrap(
            "env",
            "host_promise_rejection",
            |mut caller: Caller<'_, HostState>, promise_ptr: i32, reason_ptr: i32, is_handled: i32| {
                let state = caller.data();
                let vm_id = state.vm_id;
                let Some(vm_inner) = state.vm.upgrade() else {
                    return;
                };
                let vm = QuickJS { inner: vm_inner };
                enter_caller(vm_id, &mut caller, || {
                    vm.handle_promise_rejection(promise_ptr, reason_ptr, is_handled != 0);
                });
            },
        )
        .map_err(map_err)?;

    // env::host_module_normalize(baseNamePtr, namePtr) -> ptr
    linker
        .func_wrap(
            "env",
            "host_module_normalize",
            |mut caller: Caller<'_, HostState>, base_name_ptr: i32, name_ptr: i32| -> i32 {
                let state = caller.data();
                let vm_id = state.vm_id;
                let Some(vm_inner) = state.vm.upgrade() else {
                    return 0;
                };
                let vm = QuickJS { inner: vm_inner };
                enter_caller(vm_id, &mut caller, || {
                    vm.handle_module_normalize(base_name_ptr, name_ptr)
                })
            },
        )
        .map_err(map_err)?;

    // env::host_module_load(namePtr, outLenPtr) -> ptr
    linker
        .func_wrap(
            "env",
            "host_module_load",
            |mut caller: Caller<'_, HostState>, name_ptr: i32, out_len_ptr: i32| -> i32 {
                let state = caller.data();
                let vm_id = state.vm_id;
                let Some(vm_inner) = state.vm.upgrade() else {
                    return 0;
                };
                let vm = QuickJS { inner: vm_inner };
                enter_caller(vm_id, &mut caller, || vm.handle_module_load(name_ptr, out_len_ptr))
            },
        )
        .map_err(map_err)?;

    // env::host_get_timezone_offset(hi, lo) -> i32 (UTC offset seconds, east positive)
    linker
        .func_wrap(
            "env",
            "host_get_timezone_offset",
            |mut caller: Caller<'_, HostState>, hi: i32, lo: i32| -> i32 {
                let state = caller.data();
                let vm_id = state.vm_id;
                let Some(vm_inner) = state.vm.upgrade() else {
                    return 0;
                };
                let vm = QuickJS { inner: vm_inner };
                enter_caller(vm_id, &mut caller, || {
                    let time_secs = ((i64::from(hi)) << 32) | i64::from(lo as u32);
                    let handler = vm
                        .inner
                        .timezone_offset_handler
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    match handler {
                        Some(cb) => cb(time_secs),
                        None => 0,
                    }
                })
            },
        )
        .map_err(map_err)?;

    // wasi_snapshot_preview1::clock_time_get(clock_id, precision, result_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "clock_time_get",
            |mut caller: Caller<'_, HostState>, clock_id: i32, _precision: i64, result_ptr: i32| -> i32 {
                let Some(vm_inner) = caller.data().vm.upgrade() else {
                    return WASI_ERRNO_NOSYS;
                };
                let ns: u64 = if clock_id == WASI_CLOCK_REALTIME {
                    (pi_js::time::now_ms().max(0) as u64).saturating_mul(1_000_000)
                } else if clock_id == WASI_CLOCK_MONOTONIC {
                    let ms = pi_js::time::performance_now();
                    if ms.is_finite() && ms >= 0.0 {
                        (ms * 1_000_000.0) as u64
                    } else {
                        0
                    }
                } else {
                    return WASI_ERRNO_NOSYS;
                };
                let vm_id = caller.data().vm_id;
                let vm = QuickJS { inner: vm_inner };
                enter_caller(vm_id, &mut caller, || {
                    if let Ok(ptr) = u32::try_from(result_ptr)
                        && vm.with_memory_write(ptr, &ns.to_le_bytes()).is_ok()
                    {
                        WASI_ERRNO_SUCCESS
                    } else {
                        WASI_ERRNO_INVAL
                    }
                })
            },
        )
        .map_err(map_err)?;

    // wasi_snapshot_preview1::fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |mut caller: Caller<'_, HostState>, fd: i32, iovs_ptr: i32, iovs_len: i32, nwritten_ptr: i32| -> i32 {
                let state = caller.data();
                let vm_id = state.vm_id;
                let wasi_mem = state.wasi_memory.clone();
                let custom = state.fd_write_override.clone();
                let Some(vm_inner) = state.vm.upgrade() else {
                    return WASI_ERRNO_BADF;
                };
                let vm = QuickJS { inner: vm_inner };
                enter_caller(vm_id, &mut caller, || {
                    if let Some(override_fn) = custom {
                        return override_fn(
                            &wasi_mem,
                            i64::from(fd),
                            i64::from(iovs_ptr),
                            i64::from(iovs_len),
                            i64::from(nwritten_ptr),
                        ) as i32;
                    }
                    if fd != 1 && fd != 2 {
                        return WASI_ERRNO_BADF;
                    }
                    let Ok(base) = u32::try_from(iovs_ptr) else {
                        return WASI_ERRNO_INVAL;
                    };
                    let Ok(count) = u32::try_from(iovs_len) else {
                        return WASI_ERRNO_INVAL;
                    };
                    let Ok(nwritten) = u32::try_from(nwritten_ptr) else {
                        return WASI_ERRNO_INVAL;
                    };
                    let mut total: u32 = 0;
                    for i in 0..count {
                        let Some(entry_ptr) = base.checked_add(i.saturating_mul(8)) else {
                            return WASI_ERRNO_INVAL;
                        };
                        let buf_ptr =
                            vm.with_memory_read(entry_ptr, 4, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
                        let buf_len =
                            vm.with_memory_read(entry_ptr + 4, 4, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
                        let (Ok(buf_ptr), Ok(buf_len)) = (buf_ptr, buf_len) else {
                            return WASI_ERRNO_INVAL;
                        };
                        if buf_len > 0 {
                            let bytes = vm.with_memory_read(buf_ptr, buf_len as usize, |s| s.to_vec());
                            let Ok(bytes) = bytes else {
                                return WASI_ERRNO_INVAL;
                            };
                            let text = decode_wtf8(&bytes);
                            if fd == 1 {
                                pi_js::console::log(&text);
                            } else {
                                pi_js::console::error(&text);
                            }
                        }
                        total = total.saturating_add(buf_len);
                    }
                    if vm.with_memory_write(nwritten, &total.to_le_bytes()).is_err() {
                        return WASI_ERRNO_INVAL;
                    }
                    WASI_ERRNO_SUCCESS
                })
            },
        )
        .map_err(map_err)?;

    // wasi_snapshot_preview1::fd_close(fd) -> errno
    linker
        .func_wrap("wasi_snapshot_preview1", "fd_close", |_fd: i32| -> i32 {
            WASI_ERRNO_NOSYS
        })
        .map_err(map_err)?;

    // wasi_snapshot_preview1::fd_fdstat_get(fd, stat_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_fdstat_get",
            |mut caller: Caller<'_, HostState>, fd: i32, stat_ptr: i32| -> i32 {
                if fd != 1 && fd != 2 {
                    return WASI_ERRNO_BADF;
                }
                let state = caller.data();
                let vm_id = state.vm_id;
                let Some(vm_inner) = state.vm.upgrade() else {
                    return WASI_ERRNO_BADF;
                };
                let vm = QuickJS { inner: vm_inner };
                enter_caller(vm_id, &mut caller, || {
                    let Ok(ptr) = u32::try_from(stat_ptr) else {
                        return WASI_ERRNO_INVAL;
                    };
                    let mut stat = [0u8; 24];
                    stat[0] = 2; // CHARACTER_DEVICE
                    if vm.with_memory_write(ptr, &stat).is_ok() {
                        WASI_ERRNO_SUCCESS
                    } else {
                        WASI_ERRNO_INVAL
                    }
                })
            },
        )
        .map_err(map_err)?;

    // wasi_snapshot_preview1::fd_seek(fd, offset, whence, result_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_seek",
            |_fd: i32, _offset: i64, _whence: i32, _result_ptr: i32| -> i32 { WASI_ERRNO_NOSYS },
        )
        .map_err(map_err)?;

    // wasi_snapshot_preview1::random_get(buf_ptr, buf_len) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "random_get",
            |mut caller: Caller<'_, HostState>, buf_ptr: i32, buf_len: i32| -> i32 {
                let state = caller.data();
                let vm_id = state.vm_id;
                let Some(vm_inner) = state.vm.upgrade() else {
                    return WASI_ERRNO_IO;
                };
                let vm = QuickJS { inner: vm_inner };
                enter_caller(vm_id, &mut caller, || {
                    let Ok(ptr) = u32::try_from(buf_ptr) else {
                        return WASI_ERRNO_INVAL;
                    };
                    let Ok(len) = usize::try_from(buf_len) else {
                        return WASI_ERRNO_INVAL;
                    };
                    let mut buf = vec![0u8; len];
                    if getrandom::fill(&mut buf).is_err() {
                        return WASI_ERRNO_IO;
                    }
                    if vm.with_memory_write(ptr, &buf).is_ok() {
                        WASI_ERRNO_SUCCESS
                    } else {
                        WASI_ERRNO_INVAL
                    }
                })
            },
        )
        .map_err(map_err)?;

    // wasi_snapshot_preview1::environ_sizes_get(count_ptr, buf_size_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_sizes_get",
            |mut caller: Caller<'_, HostState>, count_ptr: i32, buf_size_ptr: i32| -> i32 {
                let state = caller.data();
                let vm_id = state.vm_id;
                let Some(vm_inner) = state.vm.upgrade() else {
                    return WASI_ERRNO_NOSYS;
                };
                let vm = QuickJS { inner: vm_inner };
                enter_caller(vm_id, &mut caller, || {
                    let (Ok(c), Ok(s)) = (u32::try_from(count_ptr), u32::try_from(buf_size_ptr)) else {
                        return WASI_ERRNO_INVAL;
                    };
                    if vm.with_memory_write(c, &0u32.to_le_bytes()).is_ok()
                        && vm.with_memory_write(s, &0u32.to_le_bytes()).is_ok()
                    {
                        WASI_ERRNO_SUCCESS
                    } else {
                        WASI_ERRNO_INVAL
                    }
                })
            },
        )
        .map_err(map_err)?;

    // wasi_snapshot_preview1::environ_get(environ, environ_buf) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_get",
            |_environ: i32, _environ_buf: i32| -> i32 { WASI_ERRNO_SUCCESS },
        )
        .map_err(map_err)?;

    // wasi_snapshot_preview1::proc_exit(code) -> trap
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "proc_exit",
            |code: i32| -> std::result::Result<(), wasmtime::Error> {
                Err(wasmtime::Error::msg(format!("proc_exit({code})")))
            },
        )
        .map_err(map_err)?;

    Ok(())
}

fn get_export<P: wasmtime::WasmParams, R: wasmtime::WasmResults>(
    instance: &wasmtime::Instance,
    store: &mut Store<HostState>,
    name: &str,
) -> pi_js::Result<TypedFunc<P, R>> {
    instance
        .get_typed_func(store, name)
        .map_err(|err| pi_js::Error::msg(format!("Missing WASM export {name}: {err}")))
}

fn load_exports(store: &mut Store<HostState>, instance: &wasmtime::Instance) -> pi_js::Result<WasmExports> {
    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| pi_js::Error::msg("Missing WASM export `memory`"))?;
    let _stack_pointer = instance.get_global(&mut *store, "__stack_pointer");

    Ok(WasmExports {
        memory,
        _stack_pointer,
        _initialize: get_export(instance, store, "_initialize")?,
        qjs_init: get_export(instance, store, "qjs_init")?,
        qjs_init2: get_export(instance, store, "qjs_init2")?,
        qjs_set_interrupt_handler: get_export(instance, store, "qjs_set_interrupt_handler")?,
        qjs_set_promise_rejection_handler: get_export(instance, store, "qjs_set_promise_rejection_handler")?,
        qjs_set_module_loader: get_export(instance, store, "qjs_set_module_loader")?,
        qjs_set_memory_limit: get_export(instance, store, "qjs_set_memory_limit")?,
        qjs_set_max_stack_size: get_export(instance, store, "qjs_set_max_stack_size")?,
        qjs_run_gc: get_export(instance, store, "qjs_run_gc")?,
        qjs_destroy: get_export(instance, store, "qjs_destroy")?,
        qjs_eval: get_export(instance, store, "qjs_eval")?,
        qjs_compile: get_export(instance, store, "qjs_compile")?,
        qjs_eval_bytecode: get_export(instance, store, "qjs_eval_bytecode")?,
        qjs_new_string: get_export(instance, store, "qjs_new_string")?,
        qjs_new_number: get_export(instance, store, "qjs_new_number")?,
        qjs_new_object: get_export(instance, store, "qjs_new_object")?,
        qjs_new_array: get_export(instance, store, "qjs_new_array")?,
        qjs_get_undefined: get_export(instance, store, "qjs_get_undefined")?,
        qjs_get_null: get_export(instance, store, "qjs_get_null")?,
        qjs_get_true: get_export(instance, store, "qjs_get_true")?,
        qjs_get_false: get_export(instance, store, "qjs_get_false")?,
        qjs_new_symbol: get_export(instance, store, "qjs_new_symbol")?,
        qjs_new_big_int64: get_export(instance, store, "qjs_new_big_int64")?,
        qjs_get_big_int64: get_export(instance, store, "qjs_get_big_int64")?,
        qjs_get_float64: get_export(instance, store, "qjs_get_float64")?,
        qjs_get_string_len: get_export(instance, store, "qjs_get_string_len")?,
        qjs_free_cstring: get_export(instance, store, "qjs_free_cstring")?,
        qjs_is_exception: get_export(instance, store, "qjs_is_exception")?,
        qjs_is_undefined: get_export(instance, store, "qjs_is_undefined")?,
        qjs_is_null: get_export(instance, store, "qjs_is_null")?,
        qjs_is_bool: get_export(instance, store, "qjs_is_bool")?,
        qjs_is_number: get_export(instance, store, "qjs_is_number")?,
        qjs_is_string: get_export(instance, store, "qjs_is_string")?,
        qjs_is_object: get_export(instance, store, "qjs_is_object")?,
        qjs_is_array: get_export(instance, store, "qjs_is_array")?,
        qjs_is_function: get_export(instance, store, "qjs_is_function")?,
        qjs_is_error: get_export(instance, store, "qjs_is_error")?,
        qjs_is_promise: get_export(instance, store, "qjs_is_promise")?,
        qjs_is_symbol: get_export(instance, store, "qjs_is_symbol")?,
        qjs_is_big_int: get_export(instance, store, "qjs_is_big_int")?,
        qjs_is_proxy: get_export(instance, store, "qjs_is_proxy")?,
        qjs_is_array_buffer: get_export(instance, store, "qjs_is_array_buffer")?,
        qjs_get_bool: get_export(instance, store, "qjs_get_bool")?,
        qjs_dup_value: get_export(instance, store, "qjs_dup_value")?,
        qjs_free_value: get_export(instance, store, "qjs_free_value")?,
        qjs_get_global: get_export(instance, store, "qjs_get_global")?,
        qjs_get_prop_string: get_export(instance, store, "qjs_get_prop_string")?,
        qjs_set_prop_string: get_export(instance, store, "qjs_set_prop_string")?,
        qjs_define_prop_string: get_export(instance, store, "qjs_define_prop_string")?,
        qjs_define_prop_value: get_export(instance, store, "qjs_define_prop_value")?,
        qjs_get_prop_value: get_export(instance, store, "qjs_get_prop_value")?,
        qjs_set_prop_value: get_export(instance, store, "qjs_set_prop_value")?,
        qjs_get_prop_uint32: get_export(instance, store, "qjs_get_prop_uint32")?,
        qjs_set_prop_uint32: get_export(instance, store, "qjs_set_prop_uint32")?,
        qjs_call_constructor: get_export(instance, store, "qjs_call_constructor")?,
        qjs_call: get_export(instance, store, "qjs_call")?,
        qjs_new_host_function: get_export(instance, store, "qjs_new_host_function")?,
        qjs_new_promise: get_export(instance, store, "qjs_new_promise")?,
        qjs_promise_state: get_export(instance, store, "qjs_promise_state")?,
        qjs_promise_result: get_export(instance, store, "qjs_promise_result")?,
        qjs_promise_then: get_export(instance, store, "qjs_promise_then")?,
        qjs_promise_mark_as_handled: get_export(instance, store, "qjs_promise_mark_as_handled")?,
        qjs_is_job_pending: get_export(instance, store, "qjs_is_job_pending")?,
        qjs_execute_pending_job: get_export(instance, store, "qjs_execute_pending_job")?,
        qjs_get_exception: get_export(instance, store, "qjs_get_exception")?,
        qjs_throw: get_export(instance, store, "qjs_throw")?,
        qjs_new_error: get_export(instance, store, "qjs_new_error")?,
        qjs_get_own_property_names: get_export(instance, store, "qjs_get_own_property_names")?,
        qjs_get_own_property_names_all: get_export(instance, store, "qjs_get_own_property_names_all")?,
        qjs_get_own_property_descriptor: get_export(instance, store, "qjs_get_own_property_descriptor")?,
        qjs_has_own_property: get_export(instance, store, "qjs_has_own_property")?,
        qjs_get_prototype_of: get_export(instance, store, "qjs_get_prototype_of")?,
        qjs_new_array_buffer: get_export(instance, store, "qjs_new_array_buffer")?,
        qjs_get_array_buffer: get_export(instance, store, "qjs_get_array_buffer")?,
        qjs_new_uint8_array: get_export(instance, store, "qjs_new_uint8_array")?,
        qjs_get_typed_array_buffer: get_export(instance, store, "qjs_get_typed_array_buffer")?,
        wasm_malloc: get_export(instance, store, "wasm_malloc")?,
        wasm_free: get_export(instance, store, "wasm_free")?,
    })
}

fn wasm_err(op: &str, err: wasmtime::Error) -> pi_js::Error {
    pi_js::Error::msg(format!("QuickJS WASM error ({op}): {err}"))
}

// -----------------------------------------------------------------------------
// WTF-8 helpers
// -----------------------------------------------------------------------------

/// Decode WTF-8 bytes (QuickJS UTF-8 with possible lone surrogate 3-byte sequences)
/// into a valid Rust UTF-8 `String`, converting unpaired surrogates to U+FFFD while
/// preserving embedded NULs and valid surrogate pairs.
fn decode_wtf8(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        if b0 < 0x80 {
            units.push(u16::from(b0));
            i += 1;
        } else if (b0 & 0xE0) == 0xC0 && i + 1 < bytes.len() && (bytes[i + 1] & 0xC0) == 0x80 {
            let cp = ((u16::from(b0 & 0x1F)) << 6) | u16::from(bytes[i + 1] & 0x3F);
            units.push(cp);
            i += 2;
        } else if (b0 & 0xF0) == 0xE0
            && i + 2 < bytes.len()
            && (bytes[i + 1] & 0xC0) == 0x80
            && (bytes[i + 2] & 0xC0) == 0x80
        {
            let cp = ((u16::from(b0 & 0x0F)) << 12)
                | ((u16::from(bytes[i + 1] & 0x3F)) << 6)
                | u16::from(bytes[i + 2] & 0x3F);
            units.push(cp);
            i += 3;
        } else if (b0 & 0xF8) == 0xF0
            && i + 3 < bytes.len()
            && (bytes[i + 1] & 0xC0) == 0x80
            && (bytes[i + 2] & 0xC0) == 0x80
            && (bytes[i + 3] & 0xC0) == 0x80
        {
            let cp = ((u32::from(b0 & 0x07)) << 18)
                | ((u32::from(bytes[i + 1] & 0x3F)) << 12)
                | ((u32::from(bytes[i + 2] & 0x3F)) << 6)
                | u32::from(bytes[i + 3] & 0x3F);
            if cp > 0xFFFF && cp <= 0x10_FFFF {
                let adj = cp - 0x10000;
                units.push((0xD800 + (adj >> 10)) as u16);
                units.push((0xDC00 + (adj & 0x3FF)) as u16);
            } else {
                units.push(0xFFFD);
            }
            i += 4;
        } else {
            units.push(0xFFFD);
            i += 1;
        }
    }
    String::from_utf16_lossy(&units)
}

// -----------------------------------------------------------------------------
// QuickJS Implementation
// -----------------------------------------------------------------------------

impl QuickJS {
    /// Create a fresh QuickJS VM instance with all intrinsics enabled.
    pub fn create(options: QuickJSOptions) -> pi_js::Result<Self> {
        Self::create_with_intrinsics(options, Intrinsics::ALL)
    }

    /// Create a fresh QuickJS VM instance with a specific `Intrinsics` bitmask.
    pub fn create_with_intrinsics(options: QuickJSOptions, intrinsics: u32) -> pi_js::Result<Self> {
        if let Some(stack) = options.max_stack_size
            && (stack < 0 || stack > MAX_STACK_SIZE)
        {
            return Err(pi_js::Error::js(
                "RangeError",
                format!("maxStackSize must be an integer between 0 and {MAX_STACK_SIZE}"),
            ));
        }

        let vm_id = NEXT_VM_ID.fetch_add(1, Ordering::Relaxed);
        let mut init_err: Option<pi_js::Error> = None;

        let inner = Arc::new_cyclic(|weak| {
            let wasi_memory = WasiMemory {
                inner: Arc::new(WasiMemoryInner { vm: weak.clone() }),
            };
            let fd_write_override = options.wasi.as_ref().and_then(|factory| factory(&wasi_memory).fd_write);

            let mut linker = Linker::new(&WASM_ENGINE);
            if let Err(err) = wire_linker(&mut linker) {
                init_err = Some(err);
            }

            let mut store = Store::new(
                &WASM_ENGINE,
                HostState {
                    vm_id,
                    vm: weak.clone(),
                    wasi_memory: wasi_memory.clone(),
                    fd_write_override,
                },
            );

            let instance = if init_err.is_none() {
                match linker.instantiate(&mut store, &options.wasm.module) {
                    Ok(inst) => Some(inst),
                    Err(err) => {
                        init_err = Some(pi_js::Error::msg(format!("Failed to instantiate QuickJS wasm: {err}")));
                        None
                    }
                }
            } else {
                None
            };

            let exports = instance.and_then(|inst| match load_exports(&mut store, &inst) {
                Ok(exp) => Some(exp),
                Err(err) => {
                    init_err = Some(err);
                    None
                }
            });

            // Only reachable when instantiation + export binding failed (we check `init_err` below).
            let exports = exports.unwrap_or_else(|| panic!("QuickJS WASM export binding checked via init_err"));

            QuickJSInner {
                id: vm_id,
                disposed: AtomicBool::new(false),
                store: Mutex::new(Some(store)),
                exports,
                host_callbacks: Mutex::new(IndexMap::new()),
                next_internal_id: AtomicUsize::new(1),
                interrupt_handler: Mutex::new(options.interrupt_handler),
                unhandled_rejection_handler: Mutex::new(None),
                module_loader: Mutex::new(None),
                timezone_offset_handler: Mutex::new(None),
                scopes: Mutex::new(Vec::new()),
                cached_global: Mutex::new(None),
                cached_undefined: Mutex::new(None),
                cached_null: Mutex::new(None),
                cached_true: Mutex::new(None),
                cached_false: Mutex::new(None),
                _wasi_memory: wasi_memory,
            }
        });

        if let Some(err) = init_err {
            return Err(err);
        }

        let vm = Self { inner };

        // Initialize WASI reactor and QuickJS context now that `inner` Arc is live.
        vm.with_site(|exports, mut ctx| {
            exports
                ._initialize
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("_initialize", e))?;
            let rc = if intrinsics != Intrinsics::ALL {
                exports
                    .qjs_init2
                    .call(&mut ctx, intrinsics as i32)
                    .map_err(|e| wasm_err("qjs_init2", e))?
            } else {
                exports
                    .qjs_init
                    .call(&mut ctx, ())
                    .map_err(|e| wasm_err("qjs_init", e))?
            };
            if rc != 0 {
                return Err(pi_js::Error::msg("Failed to initialize QuickJS runtime"));
            }
            if let Some(limit) = options.memory_limit {
                let bytes = clamp_memory_limit(limit);
                exports
                    .qjs_set_memory_limit
                    .call(&mut ctx, bytes)
                    .map_err(|e| wasm_err("qjs_set_memory_limit", e))?;
            }
            if let Some(stack) = options.max_stack_size {
                exports
                    .qjs_set_max_stack_size
                    .call(&mut ctx, stack as i32)
                    .map_err(|e| wasm_err("qjs_set_max_stack_size", e))?;
            }
            if vm
                .inner
                .interrupt_handler
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
            {
                exports
                    .qjs_set_interrupt_handler
                    .call(&mut ctx, 1)
                    .map_err(|e| wasm_err("qjs_set_interrupt_handler", e))?;
            }
            Ok(())
        })?;

        Ok(vm)
    }

    pub fn is_disposed(&self) -> bool {
        self.inner.disposed.load(Ordering::Acquire)
    }

    /// Dispose the VM, releasing cached handles, callbacks, and WASM memory.
    pub fn dispose(&self) {
        if self.inner.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        *self.inner.cached_global.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.inner.cached_undefined.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.inner.cached_null.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.inner.cached_true.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.inner.cached_false.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.inner.scopes.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.inner
            .host_callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *self.inner.module_loader.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self
            .inner
            .unhandled_rejection_handler
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        *self.inner.interrupt_handler.lock().unwrap_or_else(|e| e.into_inner()) = None;

        if let Ok(mut guard) = self.inner.store.lock()
            && let Some(mut store) = guard.take()
        {
            let _ = self.inner.exports.qjs_destroy.call(&mut store, ());
        }
    }

    fn assert_not_disposed(&self) -> pi_js::Result<()> {
        if self.is_disposed() {
            Err(pi_js::Error::msg("QuickJS VM has been disposed"))
        } else {
            Ok(())
        }
    }

    fn with_site<R>(
        &self,
        f: impl FnOnce(&WasmExports, StoreContextMut<'_, HostState>) -> pi_js::Result<R>,
    ) -> pi_js::Result<R> {
        self.assert_not_disposed()?;
        let active = ACTIVE_SITE.with(|cell| match cell.get() {
            Some((id, ptr)) if id == self.inner.id => {
                cell.set(None);
                Some((id, ptr))
            }
            _ => None,
        });
        if let Some((id, mut ptr)) = active {
            let _restore = SiteResetGuard(Some((id, ptr)));
            // SAFETY: `ptr` was installed for a live `Caller` stack frame on this thread above us.
            // Taking it out of `ACTIVE_SITE` guarantees unique `&mut` access during `f`.
            let site = unsafe { ptr.as_mut() };
            return f(&self.inner.exports, site.as_ctx_mut());
        }

        let mut guard = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        let Some(store) = guard.as_mut() else {
            return Err(pi_js::Error::msg("QuickJS VM has been disposed"));
        };
        f(&self.inner.exports, store.as_ctx_mut())
    }

    fn with_memory_read<R>(&self, ptr: u32, len: usize, f: impl FnOnce(&[u8]) -> R) -> pi_js::Result<R> {
        self.with_site(|exports, ctx| {
            let data = exports.memory.data(&ctx);
            let start = ptr as usize;
            let end = start
                .checked_add(len)
                .ok_or_else(|| pi_js::Error::msg("WASM memory read overflow"))?;
            let slice = data
                .get(start..end)
                .ok_or_else(|| pi_js::Error::msg("WASM memory read out of bounds"))?;
            Ok(f(slice))
        })
    }

    fn with_memory_write(&self, ptr: u32, bytes: &[u8]) -> pi_js::Result<()> {
        self.with_site(|exports, mut ctx| {
            let data = exports.memory.data_mut(&mut ctx);
            let start = ptr as usize;
            let end = start
                .checked_add(bytes.len())
                .ok_or_else(|| pi_js::Error::msg("WASM memory write overflow"))?;
            let slice = data
                .get_mut(start..end)
                .ok_or_else(|| pi_js::Error::msg("WASM memory write out of bounds"))?;
            slice.copy_from_slice(bytes);
            Ok(())
        })
    }

    fn write_string(&self, s: &str) -> pi_js::Result<(i32, i32)> {
        let bytes = s.as_bytes();
        let alloc_len = i32::try_from(bytes.len() + 1)
            .map_err(|_| pi_js::Error::msg("String too large for wasm32 linear memory"))?;
        self.with_site(|exports, mut ctx| {
            let ptr = exports
                .wasm_malloc
                .call(&mut ctx, alloc_len)
                .map_err(|e| wasm_err("wasm_malloc", e))?;
            if ptr == 0 {
                return Err(pi_js::Error::msg("wasm_malloc failed"));
            }
            let data = exports.memory.data_mut(&mut ctx);
            let start = (ptr as u32) as usize;
            let end = start + bytes.len();
            let Some(slice) = data.get_mut(start..=end) else {
                let _ = exports.wasm_free.call(&mut ctx, ptr);
                return Err(pi_js::Error::msg("wasm_malloc returned out-of-bounds pointer"));
            };
            slice[..bytes.len()].copy_from_slice(bytes);
            slice[bytes.len()] = 0;
            Ok((ptr, bytes.len() as i32))
        })
    }

    fn free_ptr(&self, ptr: i32) {
        if ptr != 0 {
            let _ = self.with_site(|exports, mut ctx| {
                let _ = exports.wasm_free.call(&mut ctx, ptr);
                Ok(())
            });
        }
    }

    fn read_c_string(&self, ptr: i32) -> pi_js::Result<String> {
        if ptr == 0 {
            return Ok(String::new());
        }
        self.with_site(|exports, ctx| {
            let data = exports.memory.data(&ctx);
            let start = (ptr as u32) as usize;
            let tail = data
                .get(start..)
                .ok_or_else(|| pi_js::Error::msg("C string pointer out of bounds"))?;
            let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
            Ok(decode_wtf8(&tail[..end]))
        })
    }

    fn make_handle(&self, ptr: i32, singleton: bool, borrowed: bool) -> JSValueHandle {
        let inner = Arc::new(JSValueHandleInner {
            vm: self.clone(),
            ptr,
            disposed: AtomicBool::new(false),
            singleton,
            borrowed,
            on_dispose: Mutex::new(None),
        });
        if !singleton && !borrowed {
            let mut scopes = self.inner.scopes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(top) = scopes.last_mut() {
                top.push(inner.clone());
            }
        }
        JSValueHandle { inner }
    }

    fn escape_from_scope(&self, handle: &JSValueHandle) {
        let mut scopes = self.inner.scopes.lock().unwrap_or_else(|e| e.into_inner());
        let len = scopes.len();
        if len == 0 {
            return;
        }
        if let Some(pos) = scopes[len - 1]
            .iter()
            .position(|entry| Arc::ptr_eq(entry, &handle.inner))
        {
            let removed = scopes[len - 1].swap_remove(pos);
            if len >= 2 {
                scopes[len - 2].push(removed);
            }
        }
    }

    fn pop_and_dispose_scope(&self) {
        let popped = {
            let mut scopes = self.inner.scopes.lock().unwrap_or_else(|e| e.into_inner());
            scopes.pop()
        };
        if let Some(handles) = popped {
            for handle_inner in handles {
                handle_inner.dispose_inner();
            }
        }
    }

    fn throw_if_exception(&self, result: JSValueHandle) -> pi_js::Result<JSValueHandle> {
        let is_exc = self.with_site(|exports, mut ctx| {
            exports
                .qjs_is_exception
                .call(&mut ctx, result.ptr())
                .map_err(|e| wasm_err("qjs_is_exception", e))
        })?;
        if is_exc != 0 {
            let exc_handle = self.get_exception()?;
            result.dispose();
            return Err(pi_js::Error::typed("JSException", JSException::new(exc_handle)));
        }
        Ok(result)
    }

    /// Extract the pending QuickJS exception value handle.
    pub fn get_exception(&self) -> pi_js::Result<JSValueHandle> {
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_get_exception
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("qjs_get_exception", e))
        })?;
        Ok(self.make_handle(ptr, false, false))
    }

    /// Evaluate JavaScript code and return the result as a handle.
    /// `filename` defaults to `"<eval>"` inside the body. `flags` is a bitwise OR of `EvalFlags`.
    pub fn eval_code(&self, code: &str, filename: Option<&str>, flags: Option<i64>) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        let (code_ptr, code_len) = self.write_string(code)?;
        let fn_name = filename.unwrap_or("<eval>");
        let fn_res = self.write_string(fn_name);
        let (fn_ptr, _) = match fn_res {
            Ok(v) => v,
            Err(e) => {
                self.free_ptr(code_ptr);
                return Err(e);
            }
        };
        let eval_res = self.with_site(|exports, mut ctx| {
            exports
                .qjs_eval
                .call(&mut ctx, (code_ptr, code_len, fn_ptr, flags.unwrap_or(0) as i32))
                .map_err(|e| wasm_err("qjs_eval", e))
        });
        self.free_ptr(code_ptr);
        self.free_ptr(fn_ptr);
        let result_ptr = eval_res?;
        self.throw_if_exception(self.make_handle(result_ptr, false, false))
    }

    /// Compile JavaScript source code to QuickJS bytecode without executing it.
    pub fn compile(
        &self,
        code: &str,
        filename: Option<&str>,
        eval_flags: i64,
        compile_flags: i64,
    ) -> pi_js::Result<Vec<u8>> {
        self.assert_not_disposed()?;
        let (code_ptr, code_len) = self.write_string(code)?;
        let (fn_ptr, _) = match self.write_string(filename.unwrap_or("<compile>")) {
            Ok(v) => v,
            Err(e) => {
                self.free_ptr(code_ptr);
                return Err(e);
            }
        };
        let out_len_ptr = match self.with_site(|exports, mut ctx| {
            exports
                .wasm_malloc
                .call(&mut ctx, 4)
                .map_err(|e| wasm_err("wasm_malloc", e))
        }) {
            Ok(p) if p != 0 => p,
            Ok(_) => {
                self.free_ptr(code_ptr);
                self.free_ptr(fn_ptr);
                return Err(pi_js::Error::msg("wasm_malloc failed"));
            }
            Err(e) => {
                self.free_ptr(code_ptr);
                self.free_ptr(fn_ptr);
                return Err(e);
            }
        };

        let buf_ptr_res = self.with_site(|exports, mut ctx| {
            exports
                .qjs_compile
                .call(
                    &mut ctx,
                    (
                        code_ptr,
                        code_len,
                        fn_ptr,
                        eval_flags as i32,
                        compile_flags as i32,
                        out_len_ptr,
                    ),
                )
                .map_err(|e| wasm_err("qjs_compile", e))
        });
        self.free_ptr(code_ptr);
        self.free_ptr(fn_ptr);

        let buf_ptr = match buf_ptr_res {
            Ok(p) => p,
            Err(e) => {
                self.free_ptr(out_len_ptr);
                return Err(e);
            }
        };

        if buf_ptr == 0 {
            self.free_ptr(out_len_ptr);
            let exc_handle = self.get_exception()?;
            let exc = JSException::new(exc_handle);
            let msg = format!("Compilation error: {}", exc.message());
            exc.dispose();
            return Err(pi_js::Error::msg(msg));
        }

        let len_bytes = self.with_memory_read(out_len_ptr as u32, 4, |s| [s[0], s[1], s[2], s[3]]);
        self.free_ptr(out_len_ptr);
        let out_len = u32::from_le_bytes(len_bytes?) as usize;
        let bytecode = self.with_memory_read(buf_ptr as u32, out_len, |s| s.to_vec());
        self.free_ptr(buf_ptr);
        bytecode
    }

    /// Execute previously compiled QuickJS bytecode from [`QuickJS::compile`].
    pub fn eval_bytecode(&self, bytecode: &[u8]) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        let alloc_len = i32::try_from(bytecode.len())
            .map_err(|_| pi_js::Error::msg("Bytecode too large for wasm32 linear memory"))?;
        let buf_ptr = if alloc_len == 0 {
            0
        } else {
            let ptr = self.with_site(|exports, mut ctx| {
                exports
                    .wasm_malloc
                    .call(&mut ctx, alloc_len)
                    .map_err(|e| wasm_err("wasm_malloc", e))
            })?;
            if ptr == 0 {
                return Err(pi_js::Error::msg("wasm_malloc failed"));
            }
            if let Err(e) = self.with_memory_write(ptr as u32, bytecode) {
                self.free_ptr(ptr);
                return Err(e);
            }
            ptr
        };
        let res = self.with_site(|exports, mut ctx| {
            exports
                .qjs_eval_bytecode
                .call(&mut ctx, (buf_ptr, alloc_len))
                .map_err(|e| wasm_err("qjs_eval_bytecode", e))
        });
        self.free_ptr(buf_ptr);
        self.throw_if_exception(self.make_handle(res?, false, false))
    }

    /// Call a QuickJS function without converting a guest exception into `Err(JSException)`.
    pub fn call_function_raw(
        &self,
        func: &JSValueHandle,
        this_val: &JSValueHandle,
        args: &[JSValueHandle],
    ) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        let argc = i32::try_from(args.len()).map_err(|_| pi_js::Error::msg("Too many arguments"))?;
        let argv_ptr = if argc > 0 {
            let ptr = self.with_site(|exports, mut ctx| {
                exports
                    .wasm_malloc
                    .call(&mut ctx, argc * 4)
                    .map_err(|e| wasm_err("wasm_malloc", e))
            })?;
            if ptr == 0 {
                return Err(pi_js::Error::msg("wasm_malloc failed"));
            }
            let mut argv_bytes = Vec::with_capacity(args.len() * 4);
            for arg in args {
                argv_bytes.extend_from_slice(&(arg.ptr() as u32).to_le_bytes());
            }
            if let Err(e) = self.with_memory_write(ptr as u32, &argv_bytes) {
                self.free_ptr(ptr);
                return Err(e);
            }
            ptr
        } else {
            0
        };
        let res = self.with_site(|exports, mut ctx| {
            exports
                .qjs_call
                .call(&mut ctx, (func.ptr(), this_val.ptr(), argc, argv_ptr))
                .map_err(|e| wasm_err("qjs_call", e))
        });
        self.free_ptr(argv_ptr);
        Ok(self.make_handle(res?, false, false))
    }

    /// Call a QuickJS function. If the function throws, the `Err` is a `JSException`.
    pub fn call_function(
        &self,
        func: &JSValueHandle,
        this_val: &JSValueHandle,
        args: &[JSValueHandle],
    ) -> pi_js::Result<JSValueHandle> {
        let raw = self.call_function_raw(func, this_val, args)?;
        self.throw_if_exception(raw)
    }

    /// Invoke a QuickJS constructor (`new ctor(...args)`).
    pub fn construct(&self, ctor: &JSValueHandle, args: &[JSValueHandle]) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        let argc = i32::try_from(args.len()).map_err(|_| pi_js::Error::msg("Too many arguments"))?;
        let argv_ptr = if argc > 0 {
            let ptr = self.with_site(|exports, mut ctx| {
                exports
                    .wasm_malloc
                    .call(&mut ctx, argc * 4)
                    .map_err(|e| wasm_err("wasm_malloc", e))
            })?;
            if ptr == 0 {
                return Err(pi_js::Error::msg("wasm_malloc failed"));
            }
            let mut argv_bytes = Vec::with_capacity(args.len() * 4);
            for arg in args {
                argv_bytes.extend_from_slice(&(arg.ptr() as u32).to_le_bytes());
            }
            if let Err(e) = self.with_memory_write(ptr as u32, &argv_bytes) {
                self.free_ptr(ptr);
                return Err(e);
            }
            ptr
        } else {
            0
        };
        let res = self.with_site(|exports, mut ctx| {
            exports
                .qjs_call_constructor
                .call(&mut ctx, (ctor.ptr(), argc, argv_ptr))
                .map_err(|e| wasm_err("qjs_call_constructor", e))
        });
        self.free_ptr(argv_ptr);
        self.throw_if_exception(self.make_handle(res?, false, false))
    }

    /// Whether any microtask jobs are currently pending.
    pub fn is_job_pending(&self) -> pi_js::Result<bool> {
        self.with_site(|exports, mut ctx| {
            let pending = exports
                .qjs_is_job_pending
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("qjs_is_job_pending", e))?;
            Ok(pending != 0)
        })
    }

    /// Execute all pending microtask jobs. Returns the number of jobs executed.
    pub fn execute_pending_jobs(&self) -> pi_js::Result<i64> {
        self.assert_not_disposed()?;
        let mut count: i64 = 0;
        loop {
            let pending = self.with_site(|exports, mut ctx| {
                exports
                    .qjs_is_job_pending
                    .call(&mut ctx, ())
                    .map_err(|e| wasm_err("qjs_is_job_pending", e))
            })?;
            if pending == 0 {
                break;
            }
            let result = self.with_site(|exports, mut ctx| {
                exports
                    .qjs_execute_pending_job
                    .call(&mut ctx, ())
                    .map_err(|e| wasm_err("qjs_execute_pending_job", e))
            })?;
            if result < 0 {
                let exc_handle = self.get_exception()?;
                let exc = JSException::new(exc_handle);
                let msg = format!("Job execution error: {}", exc.message());
                exc.dispose();
                return Err(pi_js::Error::msg(msg));
            }
            count += 1;
        }
        Ok(count)
    }

    /// Explicitly run QuickJS garbage collection.
    pub fn run_gc(&self) -> pi_js::Result<()> {
        self.with_site(|exports, mut ctx| {
            exports
                .qjs_run_gc
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("qjs_run_gc", e))
        })
    }

    /// Update the memory limit in bytes.
    pub fn set_memory_limit(&self, limit: f64) -> pi_js::Result<()> {
        let bytes = clamp_memory_limit(limit);
        self.with_site(|exports, mut ctx| {
            exports
                .qjs_set_memory_limit
                .call(&mut ctx, bytes)
                .map_err(|e| wasm_err("qjs_set_memory_limit", e))
        })
    }

    /// Update the maximum native stack size in bytes.
    pub fn set_max_stack_size(&self, limit: i64) -> pi_js::Result<()> {
        if limit < 0 || limit > MAX_STACK_SIZE {
            return Err(pi_js::Error::js(
                "RangeError",
                format!("maxStackSize must be an integer between 0 and {MAX_STACK_SIZE}"),
            ));
        }
        self.with_site(|exports, mut ctx| {
            exports
                .qjs_set_max_stack_size
                .call(&mut ctx, limit as i32)
                .map_err(|e| wasm_err("qjs_set_max_stack_size", e))
        })
    }

    /// Install or clear the interrupt handler callback.
    pub fn set_interrupt_handler(&self, handler: Option<Arc<dyn Fn() -> bool + Send + Sync>>) -> pi_js::Result<()> {
        let enabled = i32::from(handler.is_some());
        *self.inner.interrupt_handler.lock().unwrap_or_else(|e| e.into_inner()) = handler;
        self.with_site(|exports, mut ctx| {
            exports
                .qjs_set_interrupt_handler
                .call(&mut ctx, enabled)
                .map_err(|e| wasm_err("qjs_set_interrupt_handler", e))
        })
    }

    /// Install or clear the synchronous ES module loader.
    pub fn set_module_loader(&self, loader: Option<ModuleLoader>) -> pi_js::Result<()> {
        let enabled = i32::from(loader.is_some());
        *self.inner.module_loader.lock().unwrap_or_else(|e| e.into_inner()) = loader;
        self.with_site(|exports, mut ctx| {
            exports
                .qjs_set_module_loader
                .call(&mut ctx, enabled)
                .map_err(|e| wasm_err("qjs_set_module_loader", e))
        })
    }

    /// Install or clear the unhandled promise rejection handler.
    pub fn set_unhandled_rejection_handler(&self, handler: Option<PromiseRejectionHandler>) -> pi_js::Result<()> {
        let enabled = i32::from(handler.is_some());
        *self
            .inner
            .unhandled_rejection_handler
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = handler;
        self.with_site(|exports, mut ctx| {
            exports
                .qjs_set_promise_rejection_handler
                .call(&mut ctx, enabled)
                .map_err(|e| wasm_err("qjs_set_promise_rejection_handler", e))
        })
    }

    pub fn new_string(&self, s: &str) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        let (ptr, len) = self.write_string(s)?;
        let res = self.with_site(|exports, mut ctx| {
            exports
                .qjs_new_string
                .call(&mut ctx, (ptr, len))
                .map_err(|e| wasm_err("qjs_new_string", e))
        });
        self.free_ptr(ptr);
        self.throw_if_exception(self.make_handle(res?, false, false))
    }

    pub fn new_number(&self, num: f64) -> pi_js::Result<JSValueHandle> {
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_new_number
                .call(&mut ctx, num)
                .map_err(|e| wasm_err("qjs_new_number", e))
        })?;
        Ok(self.make_handle(ptr, false, false))
    }

    pub fn new_boolean(&self, value: bool) -> pi_js::Result<JSValueHandle> {
        if value { self.r#true() } else { self.r#false() }
    }

    pub fn new_object(&self) -> pi_js::Result<JSValueHandle> {
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_new_object
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("qjs_new_object", e))
        })?;
        self.throw_if_exception(self.make_handle(ptr, false, false))
    }

    pub fn new_array(&self) -> pi_js::Result<JSValueHandle> {
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_new_array
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("qjs_new_array", e))
        })?;
        self.throw_if_exception(self.make_handle(ptr, false, false))
    }

    pub fn new_bigint(&self, value: i64) -> pi_js::Result<JSValueHandle> {
        let lo = (value as u64 & 0xFFFF_FFFF) as i32;
        let hi = (value >> 32) as i32;
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_new_big_int64
                .call(&mut ctx, (lo, hi))
                .map_err(|e| wasm_err("qjs_new_big_int64", e))
        })?;
        self.throw_if_exception(self.make_handle(ptr, false, false))
    }

    pub fn new_symbol_for(&self, description: &str) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        let (ptr, len) = self.write_string(description)?;
        let res = self.with_site(|exports, mut ctx| {
            exports
                .qjs_new_symbol
                .call(&mut ctx, (ptr, len, 1))
                .map_err(|e| wasm_err("qjs_new_symbol", e))
        });
        self.free_ptr(ptr);
        self.throw_if_exception(self.make_handle(res?, false, false))
    }

    pub fn new_array_buffer(&self, bytes: &[u8]) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        let alloc_len = i32::try_from(bytes.len()).map_err(|_| pi_js::Error::msg("Buffer too large"))?;
        let ptr = if alloc_len == 0 {
            0
        } else {
            let p = self.with_site(|exports, mut ctx| {
                exports
                    .wasm_malloc
                    .call(&mut ctx, alloc_len)
                    .map_err(|e| wasm_err("wasm_malloc", e))
            })?;
            if p == 0 {
                return Err(pi_js::Error::msg("wasm_malloc failed"));
            }
            if let Err(e) = self.with_memory_write(p as u32, bytes) {
                self.free_ptr(p);
                return Err(e);
            }
            p
        };
        let res = self.with_site(|exports, mut ctx| {
            exports
                .qjs_new_array_buffer
                .call(&mut ctx, (ptr, alloc_len))
                .map_err(|e| wasm_err("qjs_new_array_buffer", e))
        });
        self.free_ptr(ptr);
        self.throw_if_exception(self.make_handle(res?, false, false))
    }

    pub fn new_uint8_array(&self, bytes: &[u8]) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        let alloc_len = i32::try_from(bytes.len()).map_err(|_| pi_js::Error::msg("Buffer too large"))?;
        let ptr = if alloc_len == 0 {
            0
        } else {
            let p = self.with_site(|exports, mut ctx| {
                exports
                    .wasm_malloc
                    .call(&mut ctx, alloc_len)
                    .map_err(|e| wasm_err("wasm_malloc", e))
            })?;
            if p == 0 {
                return Err(pi_js::Error::msg("wasm_malloc failed"));
            }
            if let Err(e) = self.with_memory_write(p as u32, bytes) {
                self.free_ptr(p);
                return Err(e);
            }
            p
        };
        let res = self.with_site(|exports, mut ctx| {
            exports
                .qjs_new_uint8_array
                .call(&mut ctx, (ptr, alloc_len))
                .map_err(|e| wasm_err("qjs_new_uint8_array", e))
        });
        self.free_ptr(ptr);
        self.throw_if_exception(self.make_handle(res?, false, false))
    }

    /// Create a QuickJS `Error` object with a message.
    pub fn new_error(&self, message: &str) -> pi_js::Result<JSValueHandle> {
        self.new_error_with("Error", message, None)
    }

    /// Create a QuickJS `Error` object with explicit `name`, `message`, and optional `stack`.
    pub fn new_error_with(&self, name: &str, message: &str, stack: Option<&str>) -> pi_js::Result<JSValueHandle> {
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_new_error
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("qjs_new_error", e))
        })?;
        let err_handle = self.make_handle(ptr, false, false);
        let msg_h = self.new_string(message)?;
        err_handle.set_prop("message", &msg_h)?;
        msg_h.dispose();
        if !name.is_empty() && name != "Error" {
            let name_h = self.new_string(name)?;
            err_handle.set_prop("name", &name_h)?;
            name_h.dispose();
        }
        if let Some(stack) = stack {
            let stack_h = self.new_string(stack)?;
            err_handle.set_prop("stack", &stack_h)?;
            stack_h.dispose();
        }
        Ok(err_handle)
    }

    /// Create a QuickJS function backed by a host callback. The callback stays registered for the life of the VM.
    pub fn new_function(&self, name: &str, func: HostFunction) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        {
            let mut callbacks = self.inner.host_callbacks.lock().unwrap_or_else(|e| e.into_inner());
            if callbacks.contains_key(name) {
                return Err(pi_js::Error::msg(format!(
                    "Host callback with name \"{name}\" is already registered"
                )));
            }
            callbacks.insert(name.to_string(), func);
        }
        let (name_ptr, name_len) = match self.write_string(name) {
            Ok(v) => v,
            Err(e) => {
                self.inner
                    .host_callbacks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .shift_remove(name);
                return Err(e);
            }
        };
        let res = self.with_site(|exports, mut ctx| {
            exports
                .qjs_new_host_function
                .call(&mut ctx, (name_ptr, name_len, 0))
                .map_err(|e| wasm_err("qjs_new_host_function", e))
        });
        self.free_ptr(name_ptr);
        match res {
            Ok(ptr) => Ok(self.make_handle(ptr, false, false)),
            Err(e) => {
                self.inner
                    .host_callbacks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .shift_remove(name);
                Err(e)
            }
        }
    }

    /// Create an ephemeral host function whose callback is unregistered when its handle is disposed.
    pub fn new_ephemeral_function(&self, func: HostFunction) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        let id = self.inner.next_internal_id.fetch_add(1, Ordering::Relaxed);
        let name = format!("__ephemeral:{id}");
        let handle = self.new_function(&name, func)?;
        let inner_weak = Arc::downgrade(&self.inner);
        let name_owned = name;
        *handle.inner.on_dispose.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(move || {
            if let Some(vm_inner) = inner_weak.upgrade() {
                vm_inner
                    .host_callbacks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .shift_remove(&name_owned);
            }
        }));
        Ok(handle)
    }

    /// Unregister a previously registered host callback by name.
    pub fn unregister_host_callback(&self, name: &str) -> bool {
        self.inner
            .host_callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shift_remove(name)
            .is_some()
    }

    /// Create a new QuickJS promise and its `resolve` / `reject` capability wrapper.
    pub fn new_promise(&self) -> pi_js::Result<JSDeferred> {
        self.assert_not_disposed()?;
        let resolve_out_ptr = self.with_site(|exports, mut ctx| {
            exports
                .wasm_malloc
                .call(&mut ctx, 4)
                .map_err(|e| wasm_err("wasm_malloc", e))
        })?;
        if resolve_out_ptr == 0 {
            return Err(pi_js::Error::msg("wasm_malloc failed"));
        }
        let reject_out_ptr = match self.with_site(|exports, mut ctx| {
            exports
                .wasm_malloc
                .call(&mut ctx, 4)
                .map_err(|e| wasm_err("wasm_malloc", e))
        }) {
            Ok(p) if p != 0 => p,
            Ok(_) => {
                self.free_ptr(resolve_out_ptr);
                return Err(pi_js::Error::msg("wasm_malloc failed"));
            }
            Err(e) => {
                self.free_ptr(resolve_out_ptr);
                return Err(e);
            }
        };
        let promise_res = self.with_site(|exports, mut ctx| {
            exports
                .qjs_new_promise
                .call(&mut ctx, (resolve_out_ptr, reject_out_ptr))
                .map_err(|e| wasm_err("qjs_new_promise", e))
        });
        let promise_ptr = match promise_res {
            Ok(p) => p,
            Err(e) => {
                self.free_ptr(resolve_out_ptr);
                self.free_ptr(reject_out_ptr);
                return Err(e);
            }
        };
        let res_bytes = self.with_memory_read(resolve_out_ptr as u32, 4, |s| [s[0], s[1], s[2], s[3]]);
        let rej_bytes = self.with_memory_read(reject_out_ptr as u32, 4, |s| [s[0], s[1], s[2], s[3]]);
        self.free_ptr(resolve_out_ptr);
        self.free_ptr(reject_out_ptr);
        let resolve_ptr = i32::from_le_bytes(res_bytes?);
        let reject_ptr = i32::from_le_bytes(rej_bytes?);

        Ok(JSDeferred {
            handle: self.make_handle(promise_ptr, false, false),
            resolve_fn: self.make_handle(resolve_ptr, false, false),
            reject_fn: self.make_handle(reject_ptr, false, false),
        })
    }

    /// Inspect a promise's current settlement state.
    pub fn promise_state(&self, promise: &JSValueHandle) -> pi_js::Result<PromiseState> {
        let state = self.with_site(|exports, mut ctx| {
            exports
                .qjs_promise_state
                .call(&mut ctx, promise.ptr())
                .map_err(|e| wasm_err("qjs_promise_state", e))
        })?;
        Ok(match state {
            1 => PromiseState::Fulfilled,
            2 => PromiseState::Rejected,
            _ => PromiseState::Pending,
        })
    }

    /// Inspect a promise's state and result value (if settled).
    pub fn promise_result(&self, promise: &JSValueHandle) -> pi_js::Result<PromiseResult> {
        if !promise.is_promise() {
            return Ok(PromiseResult::Fulfilled(promise.dup()?));
        }
        match self.promise_state(promise)? {
            PromiseState::Pending => Ok(PromiseResult::Pending),
            PromiseState::Fulfilled => {
                let ptr = self.with_site(|exports, mut ctx| {
                    exports
                        .qjs_promise_result
                        .call(&mut ctx, promise.ptr())
                        .map_err(|e| wasm_err("qjs_promise_result", e))
                })?;
                Ok(PromiseResult::Fulfilled(self.make_handle(ptr, false, false)))
            }
            PromiseState::Rejected => {
                let ptr = self.with_site(|exports, mut ctx| {
                    exports
                        .qjs_promise_result
                        .call(&mut ctx, promise.ptr())
                        .map_err(|e| wasm_err("qjs_promise_result", e))
                })?;
                Ok(PromiseResult::Rejected(self.make_handle(ptr, false, false)))
            }
        }
    }

    /// Attach reaction handlers via `JS_PromiseThen` without consulting `Promise.prototype.then`.
    pub fn promise_then(
        &self,
        promise: &JSValueHandle,
        on_fulfilled: &JSValueHandle,
        on_rejected: &JSValueHandle,
    ) -> pi_js::Result<JSValueHandle> {
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_promise_then
                .call(&mut ctx, (promise.ptr(), on_fulfilled.ptr(), on_rejected.ptr()))
                .map_err(|e| wasm_err("qjs_promise_then", e))
        })?;
        Ok(self.make_handle(ptr, false, false))
    }

    /// Mark a promise as handled so unhandled rejection callbacks ignore it.
    pub fn mark_promise_handled(&self, promise: &JSValueHandle) -> pi_js::Result<()> {
        self.with_site(|exports, mut ctx| {
            exports
                .qjs_promise_mark_as_handled
                .call(&mut ctx, promise.ptr())
                .map_err(|e| wasm_err("qjs_promise_mark_as_handled", e))
        })
    }

    /// The global object. Cached; do not dispose.
    pub fn global(&self) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        if let Some(cached) = self
            .inner
            .cached_global
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned()
        {
            return Ok(cached);
        }
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_get_global
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("qjs_get_global", e))
        })?;
        let handle = self.make_handle(ptr, true, false);
        let mut slot = self.inner.cached_global.lock().unwrap_or_else(|e| e.into_inner());
        Ok(slot.get_or_insert(handle).clone())
    }

    /// The undefined value. Cached; do not dispose.
    pub fn undefined(&self) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        if let Some(cached) = self
            .inner
            .cached_undefined
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned()
        {
            return Ok(cached);
        }
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_get_undefined
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("qjs_get_undefined", e))
        })?;
        let handle = self.make_handle(ptr, true, false);
        let mut slot = self.inner.cached_undefined.lock().unwrap_or_else(|e| e.into_inner());
        Ok(slot.get_or_insert(handle).clone())
    }

    /// The null value. Cached; do not dispose.
    pub fn null(&self) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        if let Some(cached) = self
            .inner
            .cached_null
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned()
        {
            return Ok(cached);
        }
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_get_null
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("qjs_get_null", e))
        })?;
        let handle = self.make_handle(ptr, true, false);
        let mut slot = self.inner.cached_null.lock().unwrap_or_else(|e| e.into_inner());
        Ok(slot.get_or_insert(handle).clone())
    }

    /// The true value. Cached; do not dispose.
    pub fn r#true(&self) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        if let Some(cached) = self
            .inner
            .cached_true
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned()
        {
            return Ok(cached);
        }
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_get_true
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("qjs_get_true", e))
        })?;
        let handle = self.make_handle(ptr, true, false);
        let mut slot = self.inner.cached_true.lock().unwrap_or_else(|e| e.into_inner());
        Ok(slot.get_or_insert(handle).clone())
    }

    /// The false value. Cached; do not dispose.
    pub fn r#false(&self) -> pi_js::Result<JSValueHandle> {
        self.assert_not_disposed()?;
        if let Some(cached) = self
            .inner
            .cached_false
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned()
        {
            return Ok(cached);
        }
        let ptr = self.with_site(|exports, mut ctx| {
            exports
                .qjs_get_false
                .call(&mut ctx, ())
                .map_err(|e| wasm_err("qjs_get_false", e))
        })?;
        let handle = self.make_handle(ptr, true, false);
        let mut slot = self.inner.cached_false.lock().unwrap_or_else(|e| e.into_inner());
        Ok(slot.get_or_insert(handle).clone())
    }

    /// Run `func` with a handle scope. Handles created during the call are disposed when it returns,
    /// except those passed to [`HandleScope::escape`]. `func` is synchronous.
    pub fn with_scope<T>(&self, func: impl FnOnce(&HandleScope<'_>) -> T) -> T {
        {
            let mut scopes = self.inner.scopes.lock().unwrap_or_else(|e| e.into_inner());
            scopes.push(Vec::new());
        }
        let _exit = ScopeExitGuard { vm: self };
        let scope = HandleScope { vm: self };
        func(&scope)
    }

    /// Convert a `serde_json::Value` into a fresh QuickJS value handle without invoking guest `JSON.parse`.
    pub fn from_json_value(&self, value: &Value) -> pi_js::Result<JSValueHandle> {
        match value {
            Value::Null => self.null(),
            Value::Bool(b) => self.new_boolean(*b),
            Value::Number(n) => self.new_number(n.as_f64().unwrap_or(f64::NAN)),
            Value::String(s) => self.new_string(s),
            Value::Array(items) => {
                let arr = self.new_array()?;
                for (idx, item) in items.iter().enumerate() {
                    let elem = self.from_json_value(item)?;
                    let set_res = arr.set_prop_uint32(idx as u32, &elem);
                    elem.dispose();
                    set_res?;
                }
                Ok(arr)
            }
            Value::Object(map) => {
                let obj = self.new_object()?;
                for (key, val) in map {
                    let child = self.from_json_value(val)?;
                    let set_res = obj.set_prop(key, &child);
                    child.dispose();
                    set_res?;
                }
                Ok(obj)
            }
        }
    }

    /// Convert a QuickJS value handle into a `serde_json::Value` cleanly (JS JSON-compatible projection).
    pub fn to_json_value(&self, handle: &JSValueHandle) -> pi_js::Result<Value> {
        if handle.is_null() || handle.is_undefined() {
            return Ok(Value::Null);
        }
        if handle.is_bool() {
            return Ok(Value::Bool(handle.to_boolean()?));
        }
        if handle.is_number() {
            return Ok(pi_js::json::number_value(handle.to_number()?));
        }
        if handle.is_string() {
            return Ok(Value::String(handle.to_string()?));
        }
        if handle.is_array() {
            let len = handle.length()?;
            let mut items = Vec::with_capacity(len);
            for i in 0..len {
                let elem = handle.get_prop_uint32(i as u32)?;
                let val = self.to_json_value(&elem);
                elem.dispose();
                items.push(val?);
            }
            return Ok(Value::Array(items));
        }
        if handle.is_object() {
            let mut map = serde_json::Map::new();
            for key in handle.keys()? {
                let prop = handle.get_prop(&key)?;
                if prop.is_undefined() || prop.is_function() || prop.is_symbol() {
                    prop.dispose();
                    continue;
                }
                let val = self.to_json_value(&prop);
                prop.dispose();
                map.insert(key, val?);
            }
            return Ok(Value::Object(map));
        }
        Ok(Value::Null)
    }

    // ---- Internal WASM import handlers ----

    fn throw_host_error_into_guest(&self, err_name: &str, err_msg: &str, err_stack: Option<&str>) {
        if let Ok(err_handle) = self.new_error_with(err_name, err_msg, err_stack) {
            let ptr = err_handle.ptr();
            let _ = self.with_site(|exports, mut ctx| {
                let _ = exports.qjs_throw.call(&mut ctx, ptr);
                Ok(())
            });
            err_handle.dispose();
        }
    }

    fn handle_host_call(&self, name_ptr: i32, name_len: i32, this_ptr: i32, argc: i32, argv_ptr: i32) -> i32 {
        let Ok(name_u32) = u32::try_from(name_ptr) else {
            return 0;
        };
        let Ok(name_len_usize) = usize::try_from(name_len) else {
            return 0;
        };
        let Ok(name_bytes) = self.with_memory_read(name_u32, name_len_usize, |s| s.to_vec()) else {
            return 0;
        };
        let name = decode_wtf8(&name_bytes);

        let callback = self
            .inner
            .host_callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&name)
            .cloned();

        let Some(callback) = callback else {
            let msg = format!(
                "Host callback \"{name}\" is not registered: it was unregistered, its ephemeral function handle was disposed, or it was never re-registered after a snapshot restore."
            );
            self.throw_host_error_into_guest("Error", &msg, None);
            return 0;
        };

        let this_handle = self.make_handle(this_ptr, false, true);
        let mut args = Vec::new();
        if argc > 0
            && argv_ptr != 0
            && let Ok(argv_u32) = u32::try_from(argv_ptr)
            && let Ok(raw) = self.with_memory_read(argv_u32, (argc as usize) * 4, |s| s.to_vec())
        {
            args.reserve(argc as usize);
            for chunk in raw.chunks_exact(4) {
                let arg_ptr = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                args.push(self.make_handle(arg_ptr, false, true));
            }
        }

        match callback(&this_handle, &args) {
            Ok(result_handle) => self
                .with_site(|exports, mut ctx| {
                    exports
                        .qjs_dup_value
                        .call(&mut ctx, result_handle.ptr())
                        .map_err(|e| wasm_err("qjs_dup_value", e))
                })
                .unwrap_or(0),
            Err(err) => {
                if let Some(js_exc) = err.downcast_ref::<JSException>()
                    && !js_exc.handle.inner.disposed.load(Ordering::Acquire)
                {
                    let ptr = js_exc.handle.ptr();
                    let _ = self.with_site(|exports, mut ctx| {
                        let _ = exports.qjs_throw.call(&mut ctx, ptr);
                        Ok(())
                    });
                } else {
                    let msg = err.message();
                    self.throw_host_error_into_guest(err.name(), &msg, None);
                }
                0
            }
        }
    }

    fn handle_promise_rejection(&self, promise_ptr: i32, reason_ptr: i32, is_handled: bool) {
        let handler = self
            .inner
            .unhandled_rejection_handler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(handler) = handler else {
            let _ = self.with_site(|exports, mut ctx| {
                let _ = exports.qjs_free_value.call(&mut ctx, promise_ptr);
                let _ = exports.qjs_free_value.call(&mut ctx, reason_ptr);
                Ok(())
            });
            return;
        };
        let promise = self.make_handle(promise_ptr, false, false);
        let reason = self.make_handle(reason_ptr, false, false);
        handler(&promise, &reason, is_handled);
        promise.dispose();
        reason.dispose();
    }

    fn handle_module_normalize(&self, base_name_ptr: i32, name_ptr: i32) -> i32 {
        let Ok(specifier) = self.read_c_string(name_ptr) else {
            return 0;
        };
        let normalize = self
            .inner
            .module_loader
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|m| m.normalize.clone());
        let Some(normalize) = normalize else {
            return self.write_string(&specifier).map(|(p, _)| p).unwrap_or(0);
        };
        let Ok(base_name) = self.read_c_string(base_name_ptr) else {
            return 0;
        };
        match normalize(&base_name, &specifier) {
            Ok(normalized) => self.write_string(&normalized).map(|(p, _)| p).unwrap_or(0),
            Err(err) => {
                let msg = err.message();
                self.throw_host_error_into_guest(err.name(), &msg, None);
                0
            }
        }
    }

    fn handle_module_load(&self, name_ptr: i32, out_len_ptr: i32) -> i32 {
        let load = self
            .inner
            .module_loader
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|m| m.load.clone());
        let Some(load) = load else {
            return 0;
        };
        let Ok(name) = self.read_c_string(name_ptr) else {
            return 0;
        };
        match load(&name) {
            Ok(source) => match self.write_string(&source) {
                Ok((ptr, len)) => {
                    if let Ok(out_u32) = u32::try_from(out_len_ptr)
                        && self.with_memory_write(out_u32, &(len as u32).to_le_bytes()).is_ok()
                    {
                        ptr
                    } else {
                        self.free_ptr(ptr);
                        0
                    }
                }
                Err(err) => {
                    let msg = err.message();
                    self.throw_host_error_into_guest(err.name(), &msg, None);
                    0
                }
            },
            Err(err) => {
                let msg = err.message();
                self.throw_host_error_into_guest(err.name(), &msg, None);
                0
            }
        }
    }
}

fn clamp_memory_limit(limit: f64) -> i32 {
    if !limit.is_finite() || limit < 0.0 {
        return -1;
    }
    let clamped = (limit as u64).min(u64::from(u32::MAX)) as u32;
    clamped as i32
}

// -----------------------------------------------------------------------------
// JSException
// -----------------------------------------------------------------------------

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
        let name = handle
            .get_prop("name")
            .ok()
            .map(|h| {
                h.consume(|prop| {
                    if prop.is_undefined() {
                        "Error".to_string()
                    } else {
                        prop.to_string().unwrap_or_else(|_| "Error".to_string())
                    }
                })
            })
            .unwrap_or_else(|| "Error".to_string());

        let message = match handle.get_prop("message") {
            Ok(msg_handle) => msg_handle.consume(|prop| {
                if prop.is_undefined() {
                    handle.to_string().unwrap_or_else(|_| "Error".to_string())
                } else {
                    prop.to_string().unwrap_or_else(|_| "Error".to_string())
                }
            }),
            Err(_) => handle.to_string().unwrap_or_else(|_| "Error".to_string()),
        };

        let stack = handle.get_prop("stack").ok().and_then(|h| {
            h.consume(|prop| {
                if prop.is_undefined() {
                    None
                } else {
                    prop.to_string().ok()
                }
            })
        });

        Self {
            name,
            message,
            stack,
            handle,
        }
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
        self.handle.dispose();
    }
}

// -----------------------------------------------------------------------------
// JSValueHandle
// -----------------------------------------------------------------------------

/// A handle to a JSValue inside the QuickJS WASM instance.
#[derive(Clone, Debug)]
pub struct JSValueHandle {
    inner: Arc<JSValueHandleInner>,
}

struct JSValueHandleInner {
    vm: QuickJS,
    ptr: i32,
    disposed: AtomicBool,
    singleton: bool,
    borrowed: bool,
    on_dispose: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl std::fmt::Debug for JSValueHandleInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JSValueHandleInner")
            .field("ptr", &self.ptr)
            .field("disposed", &self.disposed.load(Ordering::Relaxed))
            .field("singleton", &self.singleton)
            .field("borrowed", &self.borrowed)
            .finish_non_exhaustive()
    }
}

impl JSValueHandleInner {
    fn dispose_inner(&self) {
        if self.singleton || self.borrowed {
            return;
        }
        if self.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(cb) = self.on_dispose.lock().unwrap_or_else(|e| e.into_inner()).take() {
            cb();
        }
        if !self.vm.is_disposed() {
            let ptr = self.ptr;
            let _ = self.vm.with_site(|exports, mut ctx| {
                let _ = exports.qjs_free_value.call(&mut ctx, ptr);
                Ok(())
            });
        }
    }
}

impl Drop for JSValueHandleInner {
    fn drop(&mut self) {
        self.dispose_inner();
    }
}

struct ConsumeGuard<'a>(&'a JSValueHandle);

impl Drop for ConsumeGuard<'_> {
    fn drop(&mut self) {
        self.0.dispose();
    }
}

impl JSValueHandle {
    pub fn ptr(&self) -> i32 {
        self.inner.ptr
    }

    fn flag_check(&self, f: fn(&WasmExports) -> &TypedFunc<i32, i32>) -> bool {
        if self.inner.vm.is_disposed() || self.inner.disposed.load(Ordering::Acquire) {
            return false;
        }
        let ptr = self.inner.ptr;
        self.inner
            .vm
            .with_site(|exports, mut ctx| Ok(f(exports).call(&mut ctx, ptr).unwrap_or(0) != 0))
            .unwrap_or(false)
    }

    pub fn is_undefined(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_undefined)
    }

    pub fn is_null(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_null)
    }

    pub fn is_bool(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_bool)
    }

    pub fn is_number(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_number)
    }

    pub fn is_string(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_string)
    }

    pub fn is_object(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_object)
    }

    pub fn is_array(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_array)
    }

    pub fn is_function(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_function)
    }

    pub fn is_error(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_error)
    }

    pub fn is_promise(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_promise)
    }

    pub fn is_symbol(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_symbol)
    }

    pub fn is_bigint(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_big_int)
    }

    pub fn is_proxy(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_proxy)
    }

    pub fn is_array_buffer(&self) -> bool {
        self.flag_check(|e| &e.qjs_is_array_buffer)
    }

    pub fn typeof_str(&self) -> &'static str {
        if self.is_undefined() {
            "undefined"
        } else if self.is_null() || self.is_object() {
            if self.is_function() { "function" } else { "object" }
        } else if self.is_bool() {
            "boolean"
        } else if self.is_number() {
            "number"
        } else if self.is_bigint() {
            "bigint"
        } else if self.is_string() {
            "string"
        } else if self.is_symbol() {
            "symbol"
        } else {
            "unknown"
        }
    }

    /// JavaScript truthiness (`!!value`).
    pub fn to_boolean(&self) -> pi_js::Result<bool> {
        let ptr = self.inner.ptr;
        let b = self.inner.vm.with_site(|exports, mut ctx| {
            exports
                .qjs_get_bool
                .call(&mut ctx, ptr)
                .map_err(|e| wasm_err("qjs_get_bool", e))
        })?;
        Ok(b != 0)
    }

    pub fn to_number(&self) -> pi_js::Result<f64> {
        let ptr = self.inner.ptr;
        self.inner.vm.with_site(|exports, mut ctx| {
            exports
                .qjs_get_float64
                .call(&mut ctx, ptr)
                .map_err(|e| wasm_err("qjs_get_float64", e))
        })
    }

    pub fn to_bigint(&self) -> pi_js::Result<i64> {
        let lo_ptr = self.inner.vm.with_site(|exports, mut ctx| {
            exports
                .wasm_malloc
                .call(&mut ctx, 4)
                .map_err(|e| wasm_err("wasm_malloc", e))
        })?;
        if lo_ptr == 0 {
            return Err(pi_js::Error::msg("wasm_malloc failed"));
        }
        let hi_ptr = match self.inner.vm.with_site(|exports, mut ctx| {
            exports
                .wasm_malloc
                .call(&mut ctx, 4)
                .map_err(|e| wasm_err("wasm_malloc", e))
        }) {
            Ok(p) if p != 0 => p,
            Ok(_) => {
                self.inner.vm.free_ptr(lo_ptr);
                return Err(pi_js::Error::msg("wasm_malloc failed"));
            }
            Err(e) => {
                self.inner.vm.free_ptr(lo_ptr);
                return Err(e);
            }
        };
        let ret = self.inner.vm.with_site(|exports, mut ctx| {
            exports
                .qjs_get_big_int64
                .call(&mut ctx, (self.inner.ptr, lo_ptr, hi_ptr))
                .map_err(|e| wasm_err("qjs_get_big_int64", e))
        });
        let ret = match ret {
            Ok(r) => r,
            Err(e) => {
                self.inner.vm.free_ptr(lo_ptr);
                self.inner.vm.free_ptr(hi_ptr);
                return Err(e);
            }
        };
        if ret != 0 {
            self.inner.vm.free_ptr(lo_ptr);
            self.inner.vm.free_ptr(hi_ptr);
            return Err(pi_js::Error::msg("Failed to convert value to BigInt"));
        }
        let lo_bytes = self
            .inner
            .vm
            .with_memory_read(lo_ptr as u32, 4, |s| [s[0], s[1], s[2], s[3]]);
        let hi_bytes = self
            .inner
            .vm
            .with_memory_read(hi_ptr as u32, 4, |s| [s[0], s[1], s[2], s[3]]);
        self.inner.vm.free_ptr(lo_ptr);
        self.inner.vm.free_ptr(hi_ptr);
        let lo = u64::from(u32::from_le_bytes(lo_bytes?));
        let hi = i64::from(i32::from_le_bytes(hi_bytes?));
        Ok((hi << 32) | (lo as i64))
    }

    /// JavaScript string conversion. May run guest `toString` / `valueOf`.
    pub fn to_string(&self) -> pi_js::Result<String> {
        let vm = &self.inner.vm;
        let len_ptr = vm.with_site(|exports, mut ctx| {
            exports
                .wasm_malloc
                .call(&mut ctx, 4)
                .map_err(|e| wasm_err("wasm_malloc", e))
        })?;
        if len_ptr == 0 {
            return Err(pi_js::Error::msg("wasm_malloc failed"));
        }
        let cstr_res = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_get_string_len
                .call(&mut ctx, (self.inner.ptr, len_ptr))
                .map_err(|e| wasm_err("qjs_get_string_len", e))
        });
        let cstr_ptr = match cstr_res {
            Ok(p) => p,
            Err(e) => {
                vm.free_ptr(len_ptr);
                return Err(e);
            }
        };
        if cstr_ptr == 0 {
            vm.free_ptr(len_ptr);
            return Ok("<null>".to_string());
        }
        let len_bytes = vm.with_memory_read(len_ptr as u32, 4, |s| [s[0], s[1], s[2], s[3]]);
        vm.free_ptr(len_ptr);
        let len = u32::from_le_bytes(len_bytes?) as usize;
        let str_res = vm.with_memory_read(cstr_ptr as u32, len, decode_wtf8);
        let _ = vm.with_site(|exports, mut ctx| {
            let _ = exports.qjs_free_cstring.call(&mut ctx, cstr_ptr);
            Ok(())
        });
        str_res
    }

    /// Extract bytes from an `ArrayBuffer`, `Uint8Array`, or other TypedArray view.
    pub fn to_bytes(&self) -> pi_js::Result<Vec<u8>> {
        let vm = &self.inner.vm;
        if self.is_array_buffer() {
            let len_out = vm.with_site(|exports, mut ctx| {
                exports
                    .wasm_malloc
                    .call(&mut ctx, 4)
                    .map_err(|e| wasm_err("wasm_malloc", e))
            })?;
            if len_out == 0 {
                return Err(pi_js::Error::msg("wasm_malloc failed"));
            }
            let data_ptr = vm.with_site(|exports, mut ctx| {
                exports
                    .qjs_get_array_buffer
                    .call(&mut ctx, (self.inner.ptr, len_out))
                    .map_err(|e| wasm_err("qjs_get_array_buffer", e))
            });
            let data_ptr = match data_ptr {
                Ok(p) if p != 0 => p,
                Ok(_) => {
                    vm.free_ptr(len_out);
                    return Err(pi_js::Error::msg("Failed to get ArrayBuffer data"));
                }
                Err(e) => {
                    vm.free_ptr(len_out);
                    return Err(e);
                }
            };
            let len_bytes = vm.with_memory_read(len_out as u32, 4, |s| [s[0], s[1], s[2], s[3]]);
            vm.free_ptr(len_out);
            let len = u32::from_le_bytes(len_bytes?) as usize;
            return vm.with_memory_read(data_ptr as u32, len, |s| s.to_vec());
        }

        let bo_ptr =
            vm.with_site(|e, mut c| e.wasm_malloc.call(&mut c, 4).map_err(|er| wasm_err("wasm_malloc", er)))?;
        let bl_ptr =
            vm.with_site(|e, mut c| e.wasm_malloc.call(&mut c, 4).map_err(|er| wasm_err("wasm_malloc", er)))?;
        let bpe_ptr =
            vm.with_site(|e, mut c| e.wasm_malloc.call(&mut c, 4).map_err(|er| wasm_err("wasm_malloc", er)))?;
        if bo_ptr == 0 || bl_ptr == 0 || bpe_ptr == 0 {
            vm.free_ptr(bo_ptr);
            vm.free_ptr(bl_ptr);
            vm.free_ptr(bpe_ptr);
            return Err(pi_js::Error::msg("wasm_malloc failed"));
        }
        let ab_ptr = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_get_typed_array_buffer
                .call(&mut ctx, (self.inner.ptr, bo_ptr, bl_ptr, bpe_ptr))
                .map_err(|e| wasm_err("qjs_get_typed_array_buffer", e))
        });
        let ab_handle = match ab_ptr {
            Ok(p) => vm.make_handle(p, false, false),
            Err(e) => {
                vm.free_ptr(bo_ptr);
                vm.free_ptr(bl_ptr);
                vm.free_ptr(bpe_ptr);
                return Err(e);
            }
        };
        let is_exc = vm.with_site(|e, mut c| {
            e.qjs_is_exception
                .call(&mut c, ab_handle.ptr())
                .map_err(|er| wasm_err("qjs_is_exception", er))
        })?;
        if is_exc != 0 {
            ab_handle.dispose();
            vm.free_ptr(bo_ptr);
            vm.free_ptr(bl_ptr);
            vm.free_ptr(bpe_ptr);
            return Err(pi_js::Error::msg("Value is not an ArrayBuffer or typed array"));
        }
        let byte_offset = u32::from_le_bytes(vm.with_memory_read(bo_ptr as u32, 4, |s| [s[0], s[1], s[2], s[3]])?);
        let byte_len = u32::from_le_bytes(vm.with_memory_read(bl_ptr as u32, 4, |s| [s[0], s[1], s[2], s[3]])?);
        vm.free_ptr(bo_ptr);
        vm.free_ptr(bl_ptr);
        vm.free_ptr(bpe_ptr);

        let ab_len_ptr =
            vm.with_site(|e, mut c| e.wasm_malloc.call(&mut c, 4).map_err(|er| wasm_err("wasm_malloc", er)))?;
        let ab_data_ptr = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_get_array_buffer
                .call(&mut ctx, (ab_handle.ptr(), ab_len_ptr))
                .map_err(|e| wasm_err("qjs_get_array_buffer", e))
        });
        vm.free_ptr(ab_len_ptr);
        ab_handle.dispose();
        let ab_data_ptr = ab_data_ptr?;
        if ab_data_ptr == 0 {
            return Err(pi_js::Error::msg("Failed to get ArrayBuffer data from typed array"));
        }
        vm.with_memory_read((ab_data_ptr as u32) + byte_offset, byte_len as usize, |s| s.to_vec())
    }

    pub fn to_json_value(&self) -> pi_js::Result<Value> {
        self.inner.vm.to_json_value(self)
    }

    pub fn get_prop(&self, name: &str) -> pi_js::Result<Self> {
        let vm = &self.inner.vm;
        if name.contains('\0') {
            let key_handle = vm.new_string(name)?;
            let res = self.get_prop_value(&key_handle);
            key_handle.dispose();
            return res;
        }
        let (name_ptr, _) = vm.write_string(name)?;
        let res = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_get_prop_string
                .call(&mut ctx, (self.inner.ptr, name_ptr))
                .map_err(|e| wasm_err("qjs_get_prop_string", e))
        });
        vm.free_ptr(name_ptr);
        Ok(vm.make_handle(res?, false, false))
    }

    pub fn get_prop_value(&self, key: &JSValueHandle) -> pi_js::Result<Self> {
        let vm = &self.inner.vm;
        let ptr = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_get_prop_value
                .call(&mut ctx, (self.inner.ptr, key.ptr()))
                .map_err(|e| wasm_err("qjs_get_prop_value", e))
        })?;
        Ok(vm.make_handle(ptr, false, false))
    }

    pub fn get_prop_uint32(&self, index: u32) -> pi_js::Result<Self> {
        let vm = &self.inner.vm;
        let ptr = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_get_prop_uint32
                .call(&mut ctx, (self.inner.ptr, index as i32))
                .map_err(|e| wasm_err("qjs_get_prop_uint32", e))
        })?;
        Ok(vm.make_handle(ptr, false, false))
    }

    pub fn set_prop(&self, name: &str, value: &JSValueHandle) -> pi_js::Result<()> {
        let vm = &self.inner.vm;
        if name.contains('\0') {
            let key_handle = vm.new_string(name)?;
            let res = self.set_prop_value(&key_handle, value);
            key_handle.dispose();
            return res;
        }
        let (name_ptr, _) = vm.write_string(name)?;
        let res = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_set_prop_string
                .call(&mut ctx, (self.inner.ptr, name_ptr, value.ptr()))
                .map_err(|e| wasm_err("qjs_set_prop_string", e))
        });
        vm.free_ptr(name_ptr);
        res?;
        Ok(())
    }

    pub fn set_prop_value(&self, key: &JSValueHandle, value: &JSValueHandle) -> pi_js::Result<()> {
        self.inner.vm.with_site(|exports, mut ctx| {
            exports
                .qjs_set_prop_value
                .call(&mut ctx, (self.inner.ptr, key.ptr(), value.ptr()))
                .map_err(|e| wasm_err("qjs_set_prop_value", e))?;
            Ok(())
        })
    }

    pub fn set_prop_uint32(&self, index: u32, value: &JSValueHandle) -> pi_js::Result<()> {
        self.inner.vm.with_site(|exports, mut ctx| {
            exports
                .qjs_set_prop_uint32
                .call(&mut ctx, (self.inner.ptr, index as i32, value.ptr()))
                .map_err(|e| wasm_err("qjs_set_prop_uint32", e))?;
            Ok(())
        })
    }

    pub fn define_prop(
        &self,
        name: &str,
        value: &JSValueHandle,
        descriptor: JSPropertyDescriptor,
    ) -> pi_js::Result<()> {
        let mut flags: i32 = 0;
        if descriptor.configurable {
            flags |= 1;
        }
        if descriptor.writable {
            flags |= 2;
        }
        if descriptor.enumerable {
            flags |= 4;
        }
        let vm = &self.inner.vm;
        if name.contains('\0') {
            let key_handle = vm.new_string(name)?;
            let res = self.define_prop_value(&key_handle, value, descriptor);
            key_handle.dispose();
            return res;
        }
        let (name_ptr, _) = vm.write_string(name)?;
        let res = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_define_prop_string
                .call(&mut ctx, (self.inner.ptr, name_ptr, value.ptr(), flags))
                .map_err(|e| wasm_err("qjs_define_prop_string", e))
        });
        vm.free_ptr(name_ptr);
        res?;
        Ok(())
    }

    pub fn define_prop_value(
        &self,
        key: &JSValueHandle,
        value: &JSValueHandle,
        descriptor: JSPropertyDescriptor,
    ) -> pi_js::Result<()> {
        let mut flags: i32 = 0;
        if descriptor.configurable {
            flags |= 1;
        }
        if descriptor.writable {
            flags |= 2;
        }
        if descriptor.enumerable {
            flags |= 4;
        }
        self.inner.vm.with_site(|exports, mut ctx| {
            exports
                .qjs_define_prop_value
                .call(&mut ctx, (self.inner.ptr, key.ptr(), value.ptr(), flags))
                .map_err(|e| wasm_err("qjs_define_prop_value", e))?;
            Ok(())
        })
    }

    pub fn has_own_property(&self, name: &str) -> pi_js::Result<bool> {
        let vm = &self.inner.vm;
        let (name_ptr, _) = vm.write_string(name)?;
        let res = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_has_own_property
                .call(&mut ctx, (self.inner.ptr, name_ptr))
                .map_err(|e| wasm_err("qjs_has_own_property", e))
        });
        vm.free_ptr(name_ptr);
        Ok(res? != 0)
    }

    pub fn keys(&self) -> pi_js::Result<Vec<String>> {
        self.collect_string_keys(|exports| &exports.qjs_get_own_property_names)
    }

    pub fn get_own_property_names(&self) -> pi_js::Result<Vec<String>> {
        self.collect_string_keys(|exports| &exports.qjs_get_own_property_names_all)
    }

    fn collect_string_keys(&self, func: fn(&WasmExports) -> &TypedFunc<i32, i32>) -> pi_js::Result<Vec<String>> {
        let vm = &self.inner.vm;
        let keys_ptr = vm.with_site(|exports, mut ctx| {
            func(exports)
                .call(&mut ctx, self.inner.ptr)
                .map_err(|e| wasm_err("qjs_get_own_property_names", e))
        })?;
        let keys_handle = vm.make_handle(keys_ptr, false, false);
        let is_exc = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_is_exception
                .call(&mut ctx, keys_handle.ptr())
                .map_err(|e| wasm_err("qjs_is_exception", e))
        })?;
        if is_exc != 0 {
            keys_handle.dispose();
            return Ok(Vec::new());
        }
        let len = keys_handle.length()?;
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            let key_handle = keys_handle.get_prop_uint32(i as u32)?;
            let s = key_handle.to_string();
            key_handle.dispose();
            out.push(s?);
        }
        keys_handle.dispose();
        Ok(out)
    }

    pub fn get_own_property_descriptor(&self, key: &str) -> pi_js::Result<Option<JSOwnPropertyDescriptor>> {
        let vm = &self.inner.vm;
        let key_handle = vm.new_string(key)?;
        let desc_ptr = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_get_own_property_descriptor
                .call(&mut ctx, (self.inner.ptr, key_handle.ptr()))
                .map_err(|e| wasm_err("qjs_get_own_property_descriptor", e))
        });
        key_handle.dispose();
        let desc_ptr = desc_ptr?;
        if desc_ptr == 0 {
            return Ok(None);
        }
        let desc_handle = vm.throw_if_exception(vm.make_handle(desc_ptr, false, false))?;
        let result = desc_handle.consume(|d| -> pi_js::Result<JSOwnPropertyDescriptor> {
            let enumerable = d.get_prop("enumerable")?.consume(|h| h.to_boolean())?;
            let configurable = d.get_prop("configurable")?.consume(|h| h.to_boolean())?;
            if d.has_own_property("value")? {
                let value = d.get_prop("value")?;
                let writable = d.get_prop("writable")?.consume(|h| h.to_boolean())?;
                Ok(JSOwnPropertyDescriptor {
                    value: Some(value),
                    get: None,
                    set: None,
                    writable: Some(writable),
                    enumerable,
                    configurable,
                })
            } else {
                let get = d.get_prop("get")?;
                let set = d.get_prop("set")?;
                Ok(JSOwnPropertyDescriptor {
                    value: None,
                    get: Some(get),
                    set: Some(set),
                    writable: None,
                    enumerable,
                    configurable,
                })
            }
        })?;
        Ok(Some(result))
    }

    pub fn get_prototype_of(&self) -> pi_js::Result<Self> {
        let vm = &self.inner.vm;
        let ptr = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_get_prototype_of
                .call(&mut ctx, self.inner.ptr)
                .map_err(|e| wasm_err("qjs_get_prototype_of", e))
        })?;
        vm.throw_if_exception(vm.make_handle(ptr, false, false))
    }

    pub fn length(&self) -> pi_js::Result<usize> {
        let len_h = self.get_prop("length")?;
        let n = len_h.consume(|h| h.to_number())?;
        if !n.is_finite() || n <= 0.0 {
            Ok(0)
        } else {
            Ok(n as usize)
        }
    }

    /// Run `func` with `&self`, then dispose this handle before returning (even on panic).
    pub fn consume<T>(&self, func: impl FnOnce(&Self) -> T) -> T {
        let _dispose = ConsumeGuard(self);
        func(self)
    }

    /// Duplicate this handle (increment QuickJS reference count).
    pub fn dup(&self) -> pi_js::Result<Self> {
        let vm = &self.inner.vm;
        let ptr = vm.with_site(|exports, mut ctx| {
            exports
                .qjs_dup_value
                .call(&mut ctx, self.inner.ptr)
                .map_err(|e| wasm_err("qjs_dup_value", e))
        })?;
        Ok(vm.make_handle(ptr, false, false))
    }

    /// Dispose this handle. Safe to call after the VM has been disposed (a no-op).
    pub fn dispose(&self) {
        self.inner.dispose_inner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wasm::load_quick_js_wasm;
    use serde_json::json;

    fn test_vm() -> QuickJS {
        let wasm = futures::executor::block_on(load_quick_js_wasm(None)).unwrap();
        QuickJS::create(QuickJSOptions {
            wasm,
            memory_limit: None,
            max_stack_size: Some(MAX_STACK_SIZE),
            interrupt_handler: None,
            wasi: None,
        })
        .unwrap()
    }

    #[test]
    fn eval_and_host_function_and_reentrancy() {
        let vm = test_vm();
        let vm_clone = vm.clone();
        let host_add = vm
            .new_function(
                "add",
                Arc::new(move |_this, args| {
                    let a = args.first().map(|h| h.to_number()).transpose()?.unwrap_or(0.0);
                    let b = args.get(1).map(|h| h.to_number()).transpose()?.unwrap_or(0.0);
                    vm_clone.new_number(a + b)
                }),
            )
            .unwrap();
        vm.global().unwrap().set_prop("hostAdd", &host_add).unwrap();
        host_add.dispose();

        let res = vm.eval_code("hostAdd(19, 23)", None, None).unwrap();
        assert_eq!(res.to_number().unwrap(), 42.0);
        res.dispose();
        vm.dispose();
    }

    #[test]
    fn eval_module_with_loader_and_promises() {
        let wasm = futures::executor::block_on(load_quick_js_wasm(None)).unwrap();
        let vm = QuickJS::create(QuickJSOptions {
            wasm,
            memory_limit: None,
            max_stack_size: Some(MAX_STACK_SIZE),
            interrupt_handler: None,
            wasi: None,
        })
        .unwrap();
        vm.set_module_loader(Some(ModuleLoader {
            normalize: Some(Arc::new(|_base, spec| Ok(spec.to_string()))),
            load: Arc::new(|name| match name {
                "./dep.js" => Ok("export const answer = 41;".to_string()),
                other => Err(pi_js::Error::msg(format!("Module not found: {other}"))),
            }),
        }))
        .unwrap();

        let promise = vm
            .eval_code(
                "import { answer } from './dep.js'; globalThis.modOut = answer + 1;",
                Some("entry.js"),
                Some(EvalFlags::TYPE_MODULE),
            )
            .unwrap();
        vm.execute_pending_jobs().unwrap();
        assert_eq!(vm.promise_state(&promise).unwrap(), PromiseState::Fulfilled);
        promise.dispose();

        let out = vm.global().unwrap().get_prop("modOut").unwrap();
        assert_eq!(out.to_number().unwrap(), 42.0);
        out.dispose();
        vm.dispose();
    }

    #[test]
    fn scope_interrupt_stack_memory_and_json_roundtrip() {
        let hit_interrupt = Arc::new(AtomicBool::new(false));
        let hit_clone = hit_interrupt.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();

        let wasm = futures::executor::block_on(load_quick_js_wasm(None)).unwrap();
        let vm = QuickJS::create(QuickJSOptions {
            wasm,
            memory_limit: Some(8.0 * 1024.0 * 1024.0),
            max_stack_size: Some(MAX_STACK_SIZE),
            interrupt_handler: Some(Arc::new(move || {
                if count_clone.fetch_add(1, Ordering::Relaxed) > 200 {
                    hit_clone.store(true, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            })),
            wasi: None,
        })
        .unwrap();

        let err = vm.eval_code("while (true) {}", None, None).unwrap_err();
        assert!(hit_interrupt.load(Ordering::Relaxed));
        assert_eq!(err.name(), "JSException");

        // HandleScope + escape
        let escaped = vm.with_scope(|scope| {
            let obj = vm.new_object().unwrap();
            let v = vm.new_string("kept").unwrap();
            obj.set_prop("k", &v).unwrap();
            let _temp = vm.new_number(99.0).unwrap();
            scope.escape(obj)
        });
        assert_eq!(escaped.to_json_value().unwrap(), json!({ "k": "kept" }));
        escaped.dispose();

        // Clear interrupt before testing stack overflow
        vm.set_interrupt_handler(None).unwrap();

        // Deep recursion hits catchable RangeError (no trap)
        let rec_err = vm
            .eval_code("function f() { return f(); } f();", None, None)
            .unwrap_err();
        let Some(js_exc) = rec_err.downcast_ref::<JSException>() else {
            panic!("expected JSException for stack overflow, got: {rec_err:?}");
        };
        assert_eq!(
            js_exc.name(),
            "RangeError",
            "msg={}, stack={:?}",
            js_exc.message(),
            js_exc.stack()
        );

        let j = vm.from_json_value(&json!({"a": [1, true, "x", null]})).unwrap();
        assert_eq!(j.to_json_value().unwrap(), json!({"a": [1, true, "x", null]}));
        j.dispose();
        vm.dispose();
    }
}

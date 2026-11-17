//! Port of packages/coding-agent/src/core/extensions/loader.ts
//! Loads native Rust and QuickJS WASM (`.ts`/`.js`/`.mjs`/`.cjs`) extensions.

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use indexmap::{IndexMap, IndexSet};
use pi_agent_core::types::{AgentTool, AgentToolResult, ToolExecutionMode};
use pi_ai::types::Model;
use pi_codemode::{
    EvalFlags, JSDeferred, JSException, JSValueHandle, MAX_STACK_SIZE, ModuleLoader, PromiseResult, PromiseState,
    QuickJS, QuickJSOptions, load_quick_js_wasm,
};
use pi_js::abort::AbortSignal;
use pi_js::vendor::typebox::{Schema, t};
use pi_js::{BoxFuture, Unsubscribe};
use pi_tui::autocomplete::AutocompleteItem;
use pi_tui::components::text::Text as TuiText;
use pi_tui::tui::Component;
use serde_json::Value;

use super::jiti_loader::{transpile_ts_to_js, wrap_commonjs_if_needed};
use super::types::mcp_wire::{
    CONFIG_DIR_NAME_WIRE, EventBus, ExecOptions, McpServerRegistry, create_event_bus, create_synthetic_source_info,
    get_agent_dir_wire, get_synthetic_path_source, is_synthetic_path, read_pi_manifest_wire, resolve_path_wire,
};
use super::types::{
    Extension, ExtensionApi, ExtensionApiInner, ExtensionCommandContext, ExtensionContext, ExtensionFactory,
    ExtensionLoadState, ExtensionRuntime, ExtensionRuntimeInner, ExtensionToolContext, FlagOptions, FlagType,
    FlagValue, LoadExtensionErrorEntry, LoadExtensionWarningEntry, LoadExtensionsResult, LoadedExtension,
    ProviderConfig, RegisteredCommandOptions, RenderShell, ShortcutOptions, ToolAnnotations, ToolDefinition,
    ToolExposure, ToolNamespace,
};
use super::virtual_modules::{
    HostJsResult, PendingHostTask, VmHostBridge, install_host_globals, is_virtual_module, resolve_virtual_module,
};

thread_local! {
    /// Tracks which QuickJS extension VM is currently executing synchronously on this thread
    /// so synchronous `pi.events.emit(...)` -> subscriber callbacks can re-enter cleanly.
    static ACTIVE_EXT_VM_ID: Cell<Option<u64>> = const { Cell::new(None) };
}

struct ActiveVmCellGuard(Option<u64>);

impl Drop for ActiveVmCellGuard {
    fn drop(&mut self) {
        ACTIVE_EXT_VM_ID.with(|c| c.set(self.0));
    }
}

fn enter_vm_turn<R>(vm_id: u64, f: impl FnOnce() -> R) -> R {
    let prev = ACTIVE_EXT_VM_ID.with(|c| c.replace(Some(vm_id)));
    let _guard = ActiveVmCellGuard(prev);
    f()
}

// ---------------------------------------------------------------------------
// Extension module factory cache (matches TS extensionCache per cwd+gen)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq)]
struct ExtensionCacheToken {
    cwd: String,
    generation: u64,
}

struct CachedEntry {
    vm: Arc<QuickJsExtensionVm>,
    factory_fn_id: u64,
}

struct ExtensionCacheState {
    cwd: Option<String>,
    generation: u64,
    entries: IndexMap<String, CachedEntry>,
}

static EXTENSION_CACHE: LazyLock<Mutex<ExtensionCacheState>> = LazyLock::new(|| {
    Mutex::new(ExtensionCacheState {
        cwd: None,
        generation: 0,
        entries: IndexMap::new(),
    })
});

pub fn clear_extension_cache() {
    let mut cache = EXTENSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.entries.clear();
    cache.cwd = None;
    cache.generation += 1;
}

fn use_extension_cache_cwd(cwd: &str) -> ExtensionCacheToken {
    // WIRE(@mcp-w3): switch resolve_path_wire -> crate::utils::paths::resolve_path & canonicalize/paths_equal
    let resolved_cwd = resolve_path_wire(cwd, &pi_js::env::cwd());
    let mut cache = EXTENSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.cwd.as_deref().is_some_and(|prev| prev != resolved_cwd) {
        cache.entries.clear();
        cache.cwd = None;
        cache.generation += 1;
    }
    cache.cwd = Some(resolved_cwd.clone());
    ExtensionCacheToken {
        cwd: resolved_cwd,
        generation: cache.generation,
    }
}

fn is_current_cache_token(cache: &ExtensionCacheState, token: Option<&ExtensionCacheToken>) -> bool {
    let Some(t) = token else {
        return false;
    };
    cache.cwd.as_deref() == Some(t.cwd.as_str()) && cache.generation == t.generation
}

// ---------------------------------------------------------------------------
// Runtime construction (matches TS createExtensionRuntime)
// ---------------------------------------------------------------------------

pub fn create_extension_runtime() -> ExtensionRuntime {
    let not_initialized_err =
        "Extension runtime not initialized. Action methods cannot be called during extension loading.";
    let noop_actions = super::types::ExtensionActions {
        send_message: Arc::new(|_, _| {}),
        send_user_message: Arc::new(|_, _| {}),
        append_entry: Arc::new(|_, _| {}),
        set_session_name: Arc::new(|_| {}),
        get_session_name: Arc::new(|| None),
        set_label: Arc::new(|_, _| {}),
        get_active_tools: Arc::new(Vec::new),
        get_all_tools: Arc::new(Vec::new),
        get_settings: Arc::new(Default::default),
        set_active_tools: Arc::new(|_| {}),
        refresh_tools: Arc::new(|| {}),
        get_commands: Arc::new(Vec::new),
        set_model: Arc::new(move |_| Box::pin(async move { Err(pi_js::Error::msg(not_initialized_err)) })),
        get_thinking_level: Arc::new(|| pi_agent_core::types::ThinkingLevel::Off),
        set_thinking_level: Arc::new(|_| {}),
    };

    let inner = Arc::new(ExtensionRuntimeInner {
        flag_values: Mutex::new(IndexMap::new()),
        pending_provider_registrations: Mutex::new(Vec::new()),
        pending_native_provider_registrations: Mutex::new(Vec::new()),
        pending_virtual_model_registrations: Mutex::new(Vec::new()),
        mcp_servers: McpServerRegistry::new(),
        actions: Mutex::new(noop_actions),
        stale_message: Mutex::new(None),
        generation: AtomicU64::new(0),
        event_bus_unsubscribers: Mutex::new(Vec::new()),
        create_context_fn: Mutex::new(Arc::new(move || Err(pi_js::Error::msg(not_initialized_err)))),
        register_provider_fn: Mutex::new(Arc::new(|_, _, _| {})),
        register_native_provider_fn: Mutex::new(Arc::new(|_, _| {})),
        unregister_provider_fn: Mutex::new(Arc::new(|_, _| {})),
        register_virtual_model_fn: Mutex::new(Arc::new(|_, _| {})),
        unregister_virtual_model_fn: Mutex::new(Arc::new(|_, _| {})),
    });

    // Bind default pre-bind queuing handlers onto `inner` itself.
    let weak_inner = Arc::downgrade(&inner);
    *inner.register_provider_fn.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(move |name, config, ext_path| {
        if let Some(strong) = weak_inner.upgrade() {
            strong
                .pending_provider_registrations
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(super::types::PendingProviderRegistration {
                    name: name.to_string(),
                    config,
                    extension_path: ext_path.to_string(),
                });
        }
    });

    let weak_inner = Arc::downgrade(&inner);
    *inner
        .register_native_provider_fn
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Arc::new(move |provider, ext_path| {
        if let Some(strong) = weak_inner.upgrade() {
            strong
                .pending_native_provider_registrations
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(super::types::PendingNativeProviderRegistration {
                    provider,
                    extension_path: ext_path.to_string(),
                });
        }
    });

    let weak_inner = Arc::downgrade(&inner);
    *inner.unregister_provider_fn.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(move |name, _ext_path| {
        if let Some(strong) = weak_inner.upgrade() {
            strong
                .pending_provider_registrations
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|r| r.name != name);
            strong
                .pending_native_provider_registrations
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|r| r.provider.id() != name);
        }
    });

    let weak_inner = Arc::downgrade(&inner);
    *inner
        .register_virtual_model_fn
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Arc::new(move |definition, ext_path| {
        if let Some(strong) = weak_inner.upgrade() {
            strong
                .pending_virtual_model_registrations
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(super::types::PendingVirtualModelRegistration {
                    definition,
                    extension_path: ext_path.to_string(),
                });
        }
    });

    let weak_inner = Arc::downgrade(&inner);
    *inner
        .unregister_virtual_model_fn
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Arc::new(move |provider, id| {
        if let Some(strong) = weak_inner.upgrade() {
            strong
                .pending_virtual_model_registrations
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|r| r.definition.provider != provider || r.definition.id != id);
        }
    });

    ExtensionRuntime { inner }
}

pub fn create_extension_api(
    extension: LoadedExtension,
    runtime: ExtensionRuntime,
    cwd: String,
    event_bus: Arc<dyn EventBus>,
) -> ExtensionApi {
    ExtensionApi {
        inner: Arc::new(ExtensionApiInner {
            extension,
            runtime,
            cwd,
            event_bus,
            load_state: Mutex::new(ExtensionLoadState::Loading),
            next_handler_id: AtomicU64::new(1),
            pending_flag_values: Mutex::new(IndexMap::new()),
            pending_runtime_changes: Mutex::new(Vec::new()),
            loading_unsubscribers: Mutex::new(Vec::new()),
        }),
    }
}

fn create_extension(extension_path: &str, resolved_path: &str) -> LoadedExtension {
    let source = get_synthetic_path_source(extension_path).unwrap_or_else(|| "local".to_string());
    let base_dir = if is_synthetic_path(extension_path) {
        None
    } else {
        std::path::Path::new(resolved_path)
            .parent()
            .and_then(|p| p.to_str())
            .map(|s| s.to_string())
    };
    let source_info = create_synthetic_source_info(extension_path, source, None, None, base_dir);
    LoadedExtension::new(extension_path, resolved_path, source_info)
}

async fn initialize_extension(
    factory: &ExtensionFactory,
    extension_path: &str,
    resolved_path: &str,
    cwd: &str,
    event_bus: Arc<dyn EventBus>,
    runtime: ExtensionRuntime,
) -> pi_js::Result<LoadedExtension> {
    let extension = create_extension(extension_path, resolved_path);
    let mut api_ext = extension.clone();
    api_ext.host_keepalive = Arc::new(Mutex::new(None));
    let api = create_extension_api(api_ext, runtime, cwd.to_string(), event_bus);
    match factory.register(api.clone()).await {
        Ok(()) => {
            api.commit_loading()?;
            *extension.host_keepalive.lock().unwrap_or_else(|e| e.into_inner()) = factory.host_keepalive();
            Ok(extension)
        }
        Err(err) => {
            api.discard_loading();
            Err(err)
        }
    }
}

pub async fn load_extension_from_factory(
    factory: ExtensionFactory,
    cwd: &str,
    event_bus: Arc<dyn EventBus>,
    runtime: ExtensionRuntime,
    extension_path: Option<&str>,
) -> pi_js::Result<LoadedExtension> {
    let ext_path = extension_path.unwrap_or("<inline>");
    let resolved_cwd = resolve_path_wire(cwd, &pi_js::env::cwd());
    initialize_extension(&factory, ext_path, ext_path, &resolved_cwd, event_bus, runtime).await
}

// ---------------------------------------------------------------------------
// QuickJS Extension Host VM + Async Promise Pump + Module Resolution
// ---------------------------------------------------------------------------

static NEXT_VM_HOST_ID: AtomicU64 = AtomicU64::new(1);

enum JsContextKind {
    Extension(ExtensionContext),
    Command(ExtensionCommandContext),
    Tool(ExtensionToolContext),
}

struct StoredContextEntry {
    kind: JsContextKind,
}

#[derive(Default)]
struct VmRegistries {
    next_id: u64,
    fn_handles: IndexMap<u64, JSValueHandle>,
    apis: IndexMap<u64, ExtensionApi>,
    contexts: IndexMap<u64, StoredContextEntry>,
    signals: IndexMap<u64, AbortSignal>,
    unsubscribers: IndexMap<u64, Unsubscribe>,
    update_callbacks: IndexMap<u64, pi_agent_core::types::AgentToolUpdateCallback>,
}

struct QuickJsExtensionVm {
    id: u64,
    vm: QuickJS,
    vm_lock: tokio::sync::Mutex<()>,
    bridge: VmHostBridge,
    registries: Mutex<VmRegistries>,
}

impl Drop for QuickJsExtensionVm {
    fn drop(&mut self) {
        let handles = {
            let mut reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
            reg.apis.clear();
            reg.contexts.clear();
            reg.signals.clear();
            reg.update_callbacks.clear();
            let unsubs: Vec<Unsubscribe> = reg.unsubscribers.drain(..).map(|(_, u)| u).collect();
            for u in unsubs {
                u.call();
            }
            let handles: Vec<JSValueHandle> = reg.fn_handles.drain(..).map(|(_, h)| h).collect();
            handles
        };
        for h in handles {
            h.dispose();
        }
        self.vm.dispose();
    }
}

fn format_js_error(err: pi_js::Error) -> pi_js::Error {
    if let Some(jse) = err.downcast_ref::<JSException>() {
        let msg = jse.message().to_string();
        return pi_js::Error::msg(msg);
    }
    err
}

fn resolve_module_specifier(base_name: &str, specifier: &str) -> pi_js::Result<String> {
    if specifier == "<pi-entry>" || specifier.starts_with("<pi-entry:") {
        return Ok(specifier.to_string());
    }
    if specifier.starts_with("node:") && is_virtual_module(specifier) {
        return Ok(specifier.to_string());
    }
    if specifier.starts_with("file://") {
        let parsed = url::Url::parse(specifier).map_err(|e| pi_js::Error::msg(e.to_string()))?;
        let p = parsed
            .to_file_path()
            .map_err(|()| pi_js::Error::msg(format!("Invalid file URL: {specifier}")))?;
        return resolve_disk_module_path(&p.to_string_lossy());
    }
    if specifier.starts_with("./") || specifier.starts_with("../") || pi_js::path::is_absolute(specifier) {
        let base_dir = if base_name.is_empty() || base_name == "<eval>" || base_name.starts_with("<pi-entry") {
            pi_js::env::cwd()
        } else {
            pi_js::path::dirname(base_name)
        };
        let joined = if pi_js::path::is_absolute(specifier) {
            pi_js::path::normalize(specifier)
        } else {
            pi_js::path::resolve(&[&base_dir, specifier])
        };
        return resolve_disk_module_path(&joined);
    }

    // Bare package specifier: prefer local physical `node_modules` resolution walking ancestors (#9863),
    // then fall back to built-in virtual packages (`@earendil-works/pi-*`, `typebox`, Node built-ins).
    let mut cur = if base_name.is_empty() || base_name.starts_with('<') {
        pi_js::env::cwd()
    } else {
        pi_js::path::dirname(base_name)
    };
    loop {
        let nm = pi_js::path::join(&[&cur, "node_modules", specifier]);
        if let Ok(resolved) = resolve_disk_module_path(&nm) {
            return Ok(resolved);
        }
        let parent = pi_js::path::dirname(&cur);
        if parent == cur || parent.is_empty() {
            break;
        }
        cur = parent;
    }

    if is_virtual_module(specifier) {
        return Ok(specifier.to_string());
    }

    Err(pi_js::Error::msg(format!(
        "Cannot find module '{specifier}' from '{base_name}'"
    )))
}

fn resolve_disk_module_path(candidate: &str) -> pi_js::Result<String> {
    let try_exts = ["", ".ts", ".mts", ".cts", ".js", ".mjs", ".cjs"];
    for ext in &try_exts {
        let full = format!("{candidate}{ext}");
        if pi_js::fs::exists(&full) {
            if let Ok(st) = pi_js::fs::stat(&full) {
                if st.is_file() {
                    return Ok(pi_js::fs::realpath(&full).unwrap_or(full));
                }
            }
        }
    }

    // If `candidate` is a directory, inspect package.json or index files.
    if pi_js::fs::exists(candidate) {
        if let Ok(st) = pi_js::fs::stat(candidate) {
            if st.is_directory() {
                let pkg_json = pi_js::path::join(&[candidate, "package.json"]);
                if pi_js::fs::exists(&pkg_json) {
                    if let Ok(raw) = pi_js::fs::read_to_string(&pkg_json) {
                        if let Ok(val) = pi_js::json::parse(raw.strip_prefix('\u{feff}').unwrap_or(&raw)) {
                            let target = val
                                .get("exports")
                                .and_then(|ex| {
                                    ex.as_str()
                                        .or_else(|| ex.get(".").and_then(|d| d.as_str()))
                                        .or_else(|| {
                                            ex.get(".")
                                                .and_then(|d| d.get("import").or_else(|| d.get("default")))
                                                .and_then(|s| s.as_str())
                                        })
                                })
                                .or_else(|| val.get("module").and_then(|m| m.as_str()))
                                .or_else(|| val.get("main").and_then(|m| m.as_str()));
                            if let Some(rel) = target {
                                let entry = pi_js::path::resolve(&[candidate, rel]);
                                if let Ok(r) = resolve_disk_module_path(&entry) {
                                    return Ok(r);
                                }
                            }
                        }
                    }
                }
                for idx_name in &[
                    "index.ts",
                    "index.mts",
                    "index.cts",
                    "index.js",
                    "index.mjs",
                    "index.cjs",
                ] {
                    let idx_path = pi_js::path::join(&[candidate, idx_name]);
                    if pi_js::fs::exists(&idx_path) {
                        return Ok(pi_js::fs::realpath(&idx_path).unwrap_or(idx_path));
                    }
                }
            }
        }
    }

    Err(pi_js::Error::msg(format!("Module not found: {candidate}")))
}

fn load_and_transpile_module(module_name: &str) -> pi_js::Result<String> {
    if let Some(virtual_src) = resolve_virtual_module(module_name) {
        return Ok(virtual_src.to_string());
    }
    let raw = pi_js::fs::read_to_string(module_name)?;
    let no_bom = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let no_shebang = if no_bom.starts_with("#!") {
        if let Some(pos) = no_bom.find('\n') {
            &no_bom[pos..]
        } else {
            ""
        }
    } else {
        no_bom
    };
    let js = transpile_ts_to_js(no_shebang, module_name);
    Ok(wrap_commonjs_if_needed(&js, module_name))
}

impl QuickJsExtensionVm {
    async fn create() -> pi_js::Result<Arc<Self>> {
        let wasm = load_quick_js_wasm(None).await?;
        let vm = QuickJS::create(QuickJSOptions {
            wasm,
            memory_limit: None,
            max_stack_size: Some(MAX_STACK_SIZE),
            interrupt_handler: None,
            wasi: None,
        })?;
        let bridge = VmHostBridge::default();
        let id = NEXT_VM_HOST_ID.fetch_add(1, Ordering::Relaxed);

        vm.set_module_loader(Some(ModuleLoader {
            normalize: Some(Arc::new(|base, spec| resolve_module_specifier(base, spec))),
            load: Arc::new(|name| load_and_transpile_module(name)),
        }))?;

        let require_resolver: Arc<dyn Fn(&str, &str) -> pi_js::Result<String> + Send + Sync> =
            Arc::new(|from_dir, spec| {
                let fake_base = pi_js::path::join(&[from_dir, "__cjs__.js"]);
                let resolved = resolve_module_specifier(&fake_base, spec)?;
                load_and_transpile_module(&resolved)
            });

        install_host_globals(&vm, &bridge, require_resolver)?;

        let vm_arc = Arc::new(Self {
            id,
            vm,
            vm_lock: tokio::sync::Mutex::new(()),
            bridge,
            registries: Mutex::new(VmRegistries::default()),
        });

        vm_arc.install_extension_host_bridge()?;
        Ok(vm_arc)
    }

    fn alloc_id(&self) -> u64 {
        let mut reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
        reg.next_id += 1;
        reg.next_id
    }

    fn store_fn_handle(&self, handle: JSValueHandle) -> pi_js::Result<u64> {
        let duped = handle.dup()?;
        let mut reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
        reg.next_id += 1;
        let id = reg.next_id;
        reg.fn_handles.insert(id, duped);
        Ok(id)
    }

    fn install_extension_host_bridge(self: &Arc<Self>) -> pi_js::Result<()> {
        let self_weak = Arc::downgrade(self);
        self.vm.with_scope(|_scope| {
            let ext_host = self.vm.new_object()?;

            // __pi_ext.apiCall(apiId, method, argsArray)
            let w = self_weak.clone();
            let api_call = self.vm.new_function(
                "__pi_api_call",
                Arc::new(move |_this, args| {
                    let Some(this_vm) = w.upgrade() else {
                        return Err(pi_js::Error::msg("QuickJS extension VM disposed"));
                    };
                    let api_id = args.first().map(|h| h.to_number()).transpose()?.unwrap_or(0.0) as u64;
                    let method = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                    let arg0 = args.get(2);
                    let arg1 = args.get(3);
                    this_vm.handle_api_call(api_id, &method, arg0, arg1)
                }),
            )?;
            ext_host.set_prop("apiCall", &api_call)?;
            api_call.dispose();

            // __pi_ext.ctxCall(ctxId, method, arg0, arg1, arg2)
            let w = self_weak.clone();
            let ctx_call = self.vm.new_function(
                "__pi_ctx_call",
                Arc::new(move |_this, args| {
                    let Some(this_vm) = w.upgrade() else {
                        return Err(pi_js::Error::msg("QuickJS extension VM disposed"));
                    };
                    let ctx_id = args.first().map(|h| h.to_number()).transpose()?.unwrap_or(0.0) as u64;
                    let method = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                    let arg0 = args.get(2);
                    let arg1 = args.get(3);
                    let arg2 = args.get(4);
                    this_vm.handle_ctx_call(ctx_id, &method, arg0, arg1, arg2)
                }),
            )?;
            ext_host.set_prop("ctxCall", &ctx_call)?;
            ctx_call.dispose();

            // __pi_ext.onUpdate(updateId, partialJson)
            let w = self_weak.clone();
            let on_update = self.vm.new_function(
                "__pi_on_update",
                Arc::new(move |_this, args| {
                    let Some(this_vm) = w.upgrade() else {
                        return Err(pi_js::Error::msg("VM disposed"));
                    };
                    let update_id = args.first().map(|h| h.to_number()).transpose()?.unwrap_or(0.0) as u64;
                    let partial_val = args
                        .get(1)
                        .map(|h| this_vm.vm.to_json_value(h))
                        .transpose()?
                        .unwrap_or(Value::Null);
                    let cb = {
                        let reg = this_vm.registries.lock().unwrap_or_else(|e| e.into_inner());
                        reg.update_callbacks.get(&update_id).cloned()
                    };
                    if let Some(cb) = cb {
                        if let Ok(res) = parse_js_tool_result(&partial_val) {
                            cb(res);
                        }
                    }
                    this_vm.vm.undefined()
                }),
            )?;
            ext_host.set_prop("onUpdate", &on_update)?;
            on_update.dispose();

            // __pi_ext.storeFn(fn) -> id
            let w = self_weak.clone();
            let store_fn = self.vm.new_function(
                "__pi_store_fn",
                Arc::new(move |_this, args| {
                    let Some(this_vm) = w.upgrade() else {
                        return Err(pi_js::Error::msg("VM disposed"));
                    };
                    let Some(h) = args.first() else {
                        return this_vm.vm.null();
                    };
                    let id = this_vm.store_fn_handle(h.clone())?;
                    this_vm.vm.new_number(id as f64)
                }),
            )?;
            ext_host.set_prop("storeFn", &store_fn)?;
            store_fn.dispose();

            // __pi_ext.isSignalAborted(sigId) -> bool
            let w = self_weak.clone();
            let sig_aborted = self.vm.new_function(
                "__pi_sig_aborted",
                Arc::new(move |_this, args| {
                    let Some(this_vm) = w.upgrade() else {
                        return Err(pi_js::Error::msg("VM disposed"));
                    };
                    let sig_id = args.first().map(|h| h.to_number()).transpose()?.unwrap_or(0.0) as u64;
                    let aborted = {
                        let reg = this_vm.registries.lock().unwrap_or_else(|e| e.into_inner());
                        reg.signals.get(&sig_id).is_some_and(|s| s.aborted())
                    };
                    this_vm.vm.new_boolean(aborted)
                }),
            )?;
            ext_host.set_prop("isSignalAborted", &sig_aborted)?;
            sig_aborted.dispose();

            // __pi_ext.unsubscribe(id)
            let w = self_weak.clone();
            let unsub_fn = self.vm.new_function(
                "__pi_unsub",
                Arc::new(move |_this, args| {
                    let Some(this_vm) = w.upgrade() else {
                        return Err(pi_js::Error::msg("VM disposed"));
                    };
                    let id = args.first().map(|h| h.to_number()).transpose()?.unwrap_or(0.0) as u64;
                    let unsub = {
                        let mut reg = this_vm.registries.lock().unwrap_or_else(|e| e.into_inner());
                        reg.unsubscribers.shift_remove(&id)
                    };
                    if let Some(u) = unsub {
                        u.call();
                    }
                    this_vm.vm.undefined()
                }),
            )?;
            ext_host.set_prop("unsubscribe", &unsub_fn)?;
            unsub_fn.dispose();

            let global = self.vm.global()?;
            global.set_prop("__pi_ext", &ext_host)?;
            ext_host.dispose();
            global.dispose();

            let eval_res = self.vm.eval_code(EXTENSION_BRIDGE_JS, Some("<pi-ext-bridge>"), None)?;
            eval_res.dispose();
            Ok(())
        })
    }

    fn resolve_host_result_handle(&self, res: pi_js::Result<HostJsResult>, deferred: &JSDeferred) -> pi_js::Result<()> {
        match res {
            Ok(HostJsResult::Undefined) => {
                let u = self.vm.undefined()?;
                let r = deferred.resolve(&self.vm, &u);
                u.dispose();
                r
            }
            Ok(HostJsResult::Json(val)) => {
                let h = self.vm.from_json_value(&val)?;
                let r = deferred.resolve(&self.vm, &h);
                h.dispose();
                r
            }
            Ok(HostJsResult::Bytes(bytes)) => {
                let h = self.vm.new_uint8_array(&bytes)?;
                let r = deferred.resolve(&self.vm, &h);
                h.dispose();
                r
            }
            Ok(HostJsResult::ExecResult(exec_res)) => {
                let val = serde_json::json!({
                    "stdout": exec_res.stdout,
                    "stderr": exec_res.stderr,
                    "code": exec_res.code,
                    "killed": exec_res.killed,
                });
                let h = self.vm.from_json_value(&val)?;
                let r = deferred.resolve(&self.vm, &h);
                h.dispose();
                r
            }
            Ok(HostJsResult::FetchResponse {
                status,
                status_text,
                ok,
                url,
                headers,
                body,
            }) => {
                let hdr_pairs: Vec<Value> = headers
                    .into_iter()
                    .map(|(k, v)| Value::Array(vec![Value::String(k), Value::String(v)]))
                    .collect();
                let obj = self.vm.new_object()?;
                let meta = self.vm.from_json_value(&serde_json::json!({
                    "status": status,
                    "statusText": status_text,
                    "ok": ok,
                    "url": url,
                    "headers": hdr_pairs,
                }))?;
                let body_h = self.vm.new_uint8_array(&body)?;
                obj.set_prop("status", &meta.get_prop("status")?)?;
                obj.set_prop("statusText", &meta.get_prop("statusText")?)?;
                obj.set_prop("ok", &meta.get_prop("ok")?)?;
                obj.set_prop("url", &meta.get_prop("url")?)?;
                obj.set_prop("headers", &meta.get_prop("headers")?)?;
                obj.set_prop("body", &body_h)?;
                meta.dispose();
                body_h.dispose();
                let r = deferred.resolve(&self.vm, &obj);
                obj.dispose();
                r
            }
            Ok(HostJsResult::AgentToolCallOutcome(outcome)) => {
                let val = serde_json::to_value(&outcome).unwrap_or(Value::Null);
                let h = self.vm.from_json_value(&val)?;
                let r = deferred.resolve(&self.vm, &h);
                h.dispose();
                r
            }
            Err(err) => {
                let err_h = self.vm.new_error_with(err.name(), &err.message(), None)?;
                if let Some(code) = err.code() {
                    let code_h = self.vm.new_string(code)?;
                    let _ = err_h.set_prop("code", &code_h);
                    code_h.dispose();
                }
                let r = deferred.reject(&self.vm, &err_h);
                err_h.dispose();
                r
            }
        }
    }

    /// Execute a synchronous JS builder closure under `vm_lock` and pump all resulting
    /// Promises and async host futures (`ctx.ui.*`, `pi.exec`, `fetch`, `setTimeout`, `fs.promises`)
    /// cleanly without holding `vm_lock` across `.await`.
    async fn run_js_promise(
        &self,
        start_fn: impl FnOnce(&QuickJS) -> pi_js::Result<JSValueHandle>,
    ) -> pi_js::Result<Value> {
        let _guard = self.vm_lock.lock().await;
        let (outer_handle, mut pending) = enter_vm_turn(self.id, || -> pi_js::Result<_> {
            let h = start_fn(&self.vm).map_err(format_js_error)?;
            self.vm.execute_pending_jobs().map_err(format_js_error)?;
            let pending = self.bridge.drain_pending();
            Ok((h, pending))
        })?;
        drop(_guard);

        struct TaggedTaskDone {
            deferred: JSDeferred,
            res: pi_js::Result<HostJsResult>,
        }

        type TaskFut = Pin<Box<dyn Future<Output = TaggedTaskDone> + Send>>;
        let mut inflight: FuturesUnordered<TaskFut> = FuturesUnordered::new();

        let enqueue = |tasks: Vec<PendingHostTask>, inflight: &mut FuturesUnordered<TaskFut>| {
            for t in tasks {
                let deferred = t.deferred;
                let fut = t.fut;
                inflight.push(Box::pin(async move {
                    let res = fut.await;
                    TaggedTaskDone { deferred, res }
                }));
            }
        };
        enqueue(pending, &mut inflight);

        loop {
            let state = {
                let _guard = self.vm_lock.lock().await;
                enter_vm_turn(self.id, || -> pi_js::Result<_> {
                    self.vm.execute_pending_jobs().map_err(format_js_error)?;
                    let more = self.bridge.drain_pending();
                    if !outer_handle.is_promise() {
                        let json = self.vm.to_json_value(&outer_handle)?;
                        return Ok((Some(Ok(json)), more));
                    }
                    match self.vm.promise_state(&outer_handle)? {
                        PromiseState::Pending => Ok((None, more)),
                        PromiseState::Fulfilled => {
                            let res = match self.vm.promise_result(&outer_handle)? {
                                PromiseResult::Fulfilled(v) => {
                                    let val = if v.is_undefined() {
                                        Value::Null
                                    } else {
                                        self.vm.to_json_value(&v)?
                                    };
                                    v.dispose();
                                    Ok(val)
                                }
                                PromiseResult::Rejected(r) => {
                                    let exc = JSException::new(r);
                                    let err = pi_js::Error::msg(exc.message().to_string());
                                    exc.dispose();
                                    Err(err)
                                }
                                PromiseResult::Pending => Ok(Value::Null),
                            };
                            Ok((Some(res), more))
                        }
                        PromiseState::Rejected => {
                            let res = match self.vm.promise_result(&outer_handle)? {
                                PromiseResult::Rejected(r) => {
                                    let exc = JSException::new(r);
                                    let err = pi_js::Error::msg(exc.message().to_string());
                                    exc.dispose();
                                    Err(err)
                                }
                                PromiseResult::Fulfilled(v) => {
                                    v.dispose();
                                    Err(pi_js::Error::msg("Promise rejected"))
                                }
                                PromiseResult::Pending => Err(pi_js::Error::msg("Promise rejected")),
                            };
                            Ok((Some(res), more))
                        }
                    }
                })?
            };

            pending = state.1;
            enqueue(pending, &mut inflight);

            if let Some(done) = state.0 {
                let _guard = self.vm_lock.lock().await;
                outer_handle.dispose();
                // Dispose any orphaned deferreds still inside inflight.
                drop(inflight);
                return done;
            }

            let Some(completed) = inflight.next().await else {
                // No more host futures, pump jobs once more and fail if still pending.
                let _guard = self.vm_lock.lock().await;
                let final_res = enter_vm_turn(self.id, || -> pi_js::Result<_> {
                    self.vm.execute_pending_jobs().map_err(format_js_error)?;
                    match self.vm.promise_result(&outer_handle)? {
                        PromiseResult::Fulfilled(v) => {
                            let val = if v.is_undefined() {
                                Value::Null
                            } else {
                                self.vm.to_json_value(&v)?
                            };
                            v.dispose();
                            Ok(val)
                        }
                        PromiseResult::Rejected(r) => {
                            let exc = JSException::new(r);
                            let err = pi_js::Error::msg(exc.message().to_string());
                            exc.dispose();
                            Err(err)
                        }
                        PromiseResult::Pending => Err(pi_js::Error::msg(
                            "QuickJS promise remained unresolved with no pending host operations",
                        )),
                    }
                });
                outer_handle.dispose();
                return final_res;
            };

            {
                let _guard = self.vm_lock.lock().await;
                let more = enter_vm_turn(self.id, || -> pi_js::Result<_> {
                    self.resolve_host_result_handle(completed.res, &completed.deferred)?;
                    completed.deferred.dispose();
                    self.vm.execute_pending_jobs().map_err(format_js_error)?;
                    Ok(self.bridge.drain_pending())
                })?;
                enqueue(more, &mut inflight);
            }
        }
    }

    async fn load_module_factory(&self, resolved_path: &str) -> pi_js::Result<Option<u64>> {
        let entry_wrapper = format!(
            "import * as __mod from {}; globalThis.__pi_loaded_module = __mod;",
            pi_js::json::stringify(resolved_path)
        );
        let wrapper_name = format!("<pi-entry:{}>", resolved_path);
        let is_func_val = self
            .run_js_promise(move |vm| {
                let p = vm.eval_code(&entry_wrapper, Some(&wrapper_name), Some(EvalFlags::TYPE_MODULE))?;
                Ok(p)
            })
            .await?;
        let _ = is_func_val;

        let _guard = self.vm_lock.lock().await;
        enter_vm_turn(self.id, || {
            let global = self.vm.global()?;
            let mod_h = global.get_prop("__pi_loaded_module")?;
            let def_h = mod_h.get_prop("default")?;
            let res = if def_h.is_function() {
                let fn_id = self.store_fn_handle(def_h.clone())?;
                Some(fn_id)
            } else {
                None
            };
            def_h.dispose();
            mod_h.dispose();
            global.dispose();
            Ok(res)
        })
    }

    async fn invoke_factory(self: &Arc<Self>, factory_fn_id: u64, api: ExtensionApi) -> pi_js::Result<()> {
        let api_id = self.alloc_id();
        self.registries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .apis
            .insert(api_id, api);

        let result = self
            .run_js_promise(move |vm| {
                let global = vm.global()?;
                let builder = global.get_prop("__pi_build_api")?;
                let api_id_h = vm.new_number(api_id as f64)?;
                let undef = vm.undefined()?;
                let api_obj = vm.call_function(&builder, &undef, &[api_id_h.clone()])?;
                api_id_h.dispose();
                builder.dispose();

                let fn_h = {
                    let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
                    reg.fn_handles
                        .get(&factory_fn_id)
                        .ok_or_else(|| pi_js::Error::msg("Missing QuickJS factory handle"))?
                        .dup()?
                };
                let call_res = vm.call_function(&fn_h, &undef, &[api_obj.clone()]);
                fn_h.dispose();
                api_obj.dispose();
                undef.dispose();
                global.dispose();
                call_res
            })
            .await;

        let _ = result?;
        Ok(())
    }

    fn handle_api_call(
        self: &Arc<Self>,
        api_id: u64,
        method: &str,
        arg0: Option<&JSValueHandle>,
        arg1: Option<&JSValueHandle>,
    ) -> pi_js::Result<JSValueHandle> {
        let api = {
            let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
            reg.apis
                .get(&api_id)
                .cloned()
                .ok_or_else(|| pi_js::Error::msg("Extension API handle not found"))?
        };
        api.assert_active()?;

        match method {
            "on" => {
                let event_name = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let Some(fn_h) = arg1 else {
                    return Err(pi_js::Error::msg("Handler function is required"));
                };
                if !fn_h.is_function() {
                    return Err(pi_js::Error::msg("Handler must be a function"));
                }
                let fn_id = self.store_fn_handle(fn_h.clone())?;
                let vm_weak = Arc::downgrade(self);
                let unsub = if event_name == "project_trust" {
                    api.on_project_trust(Arc::new(move |ev, ctx| {
                        let vm_weak = vm_weak.clone();
                        Box::pin(async move {
                            let Some(vm_arc) = vm_weak.upgrade() else {
                                return Ok(super::types::ProjectTrustEventResult {
                                    trusted: super::types::ProjectTrustEventDecision::Undecided,
                                    remember: None,
                                });
                            };
                            let ev_json = serde_json::to_value(&ev).unwrap_or(Value::Null);
                            let trust_ctx = ExtensionContext::from_backend(Arc::new(TrustContextAdapter(ctx)));
                            let out = vm_arc.invoke_event_handler(fn_id, ev_json, trust_ctx).await?;
                            let result_val = out.get("result").cloned().unwrap_or(Value::Null);
                            if result_val.is_null() {
                                return Ok(super::types::ProjectTrustEventResult {
                                    trusted: super::types::ProjectTrustEventDecision::Undecided,
                                    remember: None,
                                });
                            }
                            Ok(
                                serde_json::from_value(result_val).unwrap_or(super::types::ProjectTrustEventResult {
                                    trusted: super::types::ProjectTrustEventDecision::Undecided,
                                    remember: None,
                                }),
                            )
                        })
                    }))
                } else if event_name == "user_bash" {
                    let vm_arc_for_ops = vm_weak.clone();
                    api.on::<super::types::UserBashEvent>(
                        move |ev: super::types::UserBashEvent, ctx: ExtensionContext| -> BoxFuture<pi_js::Result<Option<super::types::UserBashEventResult>>> {
                            let vm_weak = vm_arc_for_ops.clone();
                            Box::pin(async move {
                                let Some(vm_arc) = vm_weak.upgrade() else {
                                    return Ok(None);
                                };
                                let ev_json = serde_json::json!({
                                    "type": "user_bash",
                                    "command": ev.command,
                                    "excludeFromContext": ev.exclude_from_context,
                                    "cwd": ev.cwd,
                                });
                                let out = vm_arc.invoke_event_handler(fn_id, ev_json, ctx).await?;
                                if let Some(threw) = out.get("threw").and_then(|v| v.as_str()) {
                                    return Err(pi_js::Error::msg(threw.to_string()));
                                }
                                if out.get("hasResult").and_then(|v| v.as_bool()) != Some(true) {
                                    return Ok(None);
                                }
                                let result_val = out.get("result").cloned().unwrap_or(Value::Null);
                                if let Some(exec_id) = result_val.get("__piUserBashExecId").and_then(|v| v.as_u64()) {
                                    let ops: Arc<dyn crate::core::tools::bash::BashOperations> =
                                        Arc::new(QuickJsBashOperations {
                                            vm: Arc::clone(&vm_arc),
                                            exec_fn_id: exec_id,
                                        });
                                    return Ok(Some(super::types::UserBashEventResult::Operations { operations: ops }));
                                }
                                let validated = crate::core::extensions::runner::validate_user_bash_json_result(&result_val)
                                    .map_err(pi_js::Error::msg)?;
                                Ok(Some(validated))
                            })
                        },
                    )
                } else {
                    api.on_raw(
                        &event_name,
                        Arc::new(move |ev_json, ctx| {
                            let vm_weak = vm_weak.clone();
                            Box::pin(async move {
                                let Some(vm_arc) = vm_weak.upgrade() else {
                                    return Ok(None);
                                };
                                let envelope = vm_arc.invoke_event_handler(fn_id, ev_json, ctx).await?;
                                Ok(Some(envelope))
                            })
                        }),
                    )?
                };
                let unsub_id = self.alloc_id();
                self.registries
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .unsubscribers
                    .insert(unsub_id, unsub);
                self.vm.new_number(unsub_id as f64)
            }
            "registerTool" => {
                let Some(obj) = arg0 else {
                    return Err(pi_js::Error::msg("Tool definition is required"));
                };
                let def_val = self.vm.to_json_value(obj)?;
                let name = def_val.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let label = def_val
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&name)
                    .to_string();
                let description = def_val
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let params_h = obj.get_prop("parameters")?;
                let params_valid =
                    !params_h.is_undefined() && !params_h.is_null() && params_h.is_object() && !params_h.is_array();
                params_h.dispose();
                if !params_valid {
                    return Err(pi_js::Error::msg(format!(
                        "Tool \"{name}\" registered by extension \"{}\" must define an object parameter schema.",
                        api.inner.extension.path
                    )));
                }
                let params_schema: Schema = def_val
                    .get("parameters")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_else(|| t::object([("", t::any())]));

                let exec_h = obj.get_prop("execute")?;
                if !exec_h.is_function() {
                    exec_h.dispose();
                    return Err(pi_js::Error::msg(format!("Tool \"{name}\" must define execute()")));
                }
                let exec_fn_id = self.store_fn_handle(exec_h.clone())?;
                exec_h.dispose();

                let render_call_h = obj.get_prop("renderCall")?;
                let render_call_id = if render_call_h.is_function() {
                    Some(self.store_fn_handle(render_call_h.clone())?)
                } else {
                    None
                };
                render_call_h.dispose();

                let render_result_h = obj.get_prop("renderResult")?;
                let render_result_id = if render_result_h.is_function() {
                    Some(self.store_fn_handle(render_result_h.clone())?)
                } else {
                    None
                };
                render_result_h.dispose();

                let prompt_snippet = def_val
                    .get("promptSnippet")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let prompt_guidelines = def_val
                    .get("promptGuidelines")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect());
                let output_schema: Option<Schema> = def_val
                    .get("outputSchema")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok());
                let exposure: Option<ToolExposure> = def_val
                    .get("exposure")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok());
                let namespace: Option<ToolNamespace> = def_val
                    .get("namespace")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok());
                let annotations: Option<ToolAnnotations> = def_val
                    .get("annotations")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok());
                let default_active = def_val.get("defaultActive").and_then(|v| v.as_bool());
                let execution_mode: Option<ToolExecutionMode> = def_val
                    .get("executionMode")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok());
                let render_shell: Option<RenderShell> = def_val
                    .get("renderShell")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok());

                let vm_weak = Arc::downgrade(self);
                let execute: super::types::ToolExecuteFn =
                    Arc::new(move |tool_call_id, params, signal, on_update, ctx| {
                        let vm_weak = vm_weak.clone();
                        Box::pin(async move {
                            let Some(vm_arc) = vm_weak.upgrade() else {
                                return Err(pi_js::Error::msg("QuickJS extension VM disposed"));
                            };
                            vm_arc
                                .invoke_tool_execute(exec_fn_id, tool_call_id, params, signal, on_update, ctx)
                                .await
                        })
                    });

                let render_call = render_call_id.map(|rc_id| -> super::types::RenderCallFn {
                    let vm_weak = Arc::downgrade(self);
                    Arc::new(move |args, _theme, _ctx| {
                        let Some(vm_arc) = vm_weak.upgrade() else {
                            return Arc::new(TuiText::new(Some(""), None, None, None));
                        };
                        vm_arc.invoke_render_sync(rc_id, vec![args])
                    })
                });
                let render_result = render_result_id.map(|rr_id| -> super::types::RenderResultFn {
                    let vm_weak = Arc::downgrade(self);
                    Arc::new(move |result, opts, _theme, _ctx| {
                        let Some(vm_arc) = vm_weak.upgrade() else {
                            return Arc::new(TuiText::new(Some(""), None, None, None));
                        };
                        let res_val = serde_json::to_value(&result).unwrap_or(Value::Null);
                        let opts_val = serde_json::to_value(&opts).unwrap_or(Value::Null);
                        vm_arc.invoke_render_sync(rr_id, vec![res_val, opts_val])
                    })
                });

                api.register_tool(ToolDefinition {
                    name,
                    label,
                    description,
                    prompt_snippet,
                    prompt_guidelines,
                    parameters: params_schema,
                    constrained_sampling: None,
                    render_shell,
                    prepare_arguments: None,
                    output_schema,
                    exposure,
                    namespace,
                    annotations,
                    default_active,
                    prepare_loadout: None,
                    execution_mode,
                    execute,
                    render_call,
                    render_result,
                })?;
                self.vm.undefined()
            }
            "registerCommand" => {
                let name = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                if name.is_empty() {
                    return Err(pi_js::Error::msg(format!(
                        "Command registered by extension \"{}\" must have a non-empty string name. Use pi.registerCommand(\"name\", {{ description, handler }}).",
                        api.inner.extension.path
                    )));
                }
                let Some(opts_h) = arg1 else {
                    return Err(pi_js::Error::msg(format!(
                        "Command \"/{name}\" registered by extension \"{}\" must define handler().",
                        api.inner.extension.path
                    )));
                };
                if opts_h.is_undefined() || opts_h.is_null() {
                    return Err(pi_js::Error::msg(format!(
                        "Command \"/{name}\" registered by extension \"{}\" must define handler().",
                        api.inner.extension.path
                    )));
                }
                let handler_h = opts_h.get_prop("handler")?;
                if !handler_h.is_function() {
                    handler_h.dispose();
                    return Err(pi_js::Error::msg(format!(
                        "Command \"/{name}\" registered by extension \"{}\" must define handler().",
                        api.inner.extension.path
                    )));
                }
                let handler_fn_id = self.store_fn_handle(handler_h.clone())?;
                handler_h.dispose();

                let comp_h = opts_h.get_prop("getArgumentCompletions")?;
                let comp_fn_id = if comp_h.is_function() {
                    Some(self.store_fn_handle(comp_h.clone())?)
                } else {
                    None
                };
                comp_h.dispose();

                let opts_val = self.vm.to_json_value(opts_h)?;
                let description = opts_val
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let vm_weak = Arc::downgrade(self);
                let handler: super::types::CommandHandlerFn = Arc::new(move |args_str, cmd_ctx| {
                    let vm_weak = vm_weak.clone();
                    Box::pin(async move {
                        let Some(vm_arc) = vm_weak.upgrade() else {
                            return Err(pi_js::Error::msg("QuickJS extension VM disposed"));
                        };
                        vm_arc.invoke_command_handler(handler_fn_id, args_str, cmd_ctx).await
                    })
                });
                let get_argument_completions = comp_fn_id.map(|c_id| -> super::types::CommandCompletionsFn {
                    let vm_weak = Arc::downgrade(self);
                    Arc::new(move |prefix| {
                        let prefix = prefix.to_string();
                        let vm_weak = vm_weak.clone();
                        Box::pin(async move {
                            let vm_arc = vm_weak.upgrade()?;
                            vm_arc.invoke_completions(c_id, &prefix).await.ok().flatten()
                        })
                    })
                });

                api.register_command(
                    &name,
                    RegisteredCommandOptions {
                        description,
                        get_argument_completions,
                        handler,
                    },
                )?;
                self.vm.undefined()
            }
            "registerShortcut" => {
                let shortcut = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let Some(opts_h) = arg1 else {
                    return Err(pi_js::Error::msg("Shortcut options required"));
                };
                let handler_h = opts_h.get_prop("handler")?;
                if !handler_h.is_function() {
                    handler_h.dispose();
                    return Err(pi_js::Error::msg("Shortcut handler must be a function"));
                }
                let fn_id = self.store_fn_handle(handler_h.clone())?;
                handler_h.dispose();
                let opts_val = self.vm.to_json_value(opts_h)?;
                let description = opts_val
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let vm_weak = Arc::downgrade(self);
                api.register_shortcut(
                    shortcut,
                    ShortcutOptions {
                        description,
                        handler: Arc::new(move |ctx| {
                            let vm_weak = vm_weak.clone();
                            Box::pin(async move {
                                if let Some(vm_arc) = vm_weak.upgrade() {
                                    vm_arc.invoke_shortcut_handler(fn_id, ctx).await?;
                                }
                                Ok(())
                            })
                        }),
                    },
                )?;
                self.vm.undefined()
            }
            "registerFlag" => {
                let name = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let val = arg1
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let description = val.get("description").and_then(|v| v.as_str()).map(|s| s.to_string());
                let flag_type = match val.get("type").and_then(|v| v.as_str()) {
                    Some("string") => Some(FlagType::String),
                    Some("boolean") | None => Some(FlagType::Boolean),
                    Some(other) => {
                        return Err(pi_js::Error::msg(format!("Invalid flag type: {other}")));
                    }
                };
                let default = match val.get("default") {
                    None | Some(Value::Null) => None,
                    Some(Value::Bool(b)) => Some(FlagValue::Bool(*b)),
                    Some(Value::String(s)) => Some(FlagValue::String(s.clone())),
                    Some(other) => {
                        let expected = match flag_type.unwrap_or(FlagType::Boolean) {
                            FlagType::Boolean => "boolean",
                            FlagType::String => "string",
                        };
                        let got = if other.is_number() {
                            "number"
                        } else if other.is_object() {
                            "object"
                        } else {
                            "unknown"
                        };
                        return Err(pi_js::Error::msg(format!(
                            "Invalid default for flag \"{name}\": expected {expected}, got {got}"
                        )));
                    }
                };
                // Also inspect typeof default in TS style when default type != option type
                if let (Some(ft), Some(def)) = (flag_type, &default) {
                    let ok = matches!(
                        (ft, def),
                        (FlagType::Boolean, FlagValue::Bool(_)) | (FlagType::String, FlagValue::String(_))
                    );
                    if !ok {
                        let expected = match ft {
                            FlagType::Boolean => "boolean",
                            FlagType::String => "string",
                        };
                        let got = match def {
                            FlagValue::Bool(_) => "boolean",
                            FlagValue::String(_) => "string",
                        };
                        return Err(pi_js::Error::msg(format!(
                            "Invalid default for flag \"{name}\": expected {expected}, got {got}"
                        )));
                    }
                }
                api.register_flag(
                    &name,
                    FlagOptions {
                        description,
                        r#type: flag_type,
                        default,
                    },
                )?;
                self.vm.undefined()
            }
            "getFlag" => {
                let name = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                match api.get_flag(&name)? {
                    Some(FlagValue::Bool(b)) => self.vm.new_boolean(b),
                    Some(FlagValue::String(s)) => self.vm.new_string(&s),
                    None => self.vm.undefined(),
                }
            }
            "registerMessageRenderer" => {
                let custom_type = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let Some(fn_h) = arg1 else {
                    return Err(pi_js::Error::msg("Message renderer function is required"));
                };
                if !fn_h.is_function() {
                    return Err(pi_js::Error::msg("Message renderer must be a function"));
                }
                let fn_id = self.store_fn_handle(fn_h.clone())?;
                let vm_weak = Arc::downgrade(self);
                api.register_message_renderer(
                    &custom_type,
                    Arc::new(move |msg, opts, _theme| {
                        let vm_arc = vm_weak.upgrade()?;
                        let msg_val = serde_json::to_value(msg).unwrap_or(Value::Null);
                        let opts_val = serde_json::to_value(opts).unwrap_or(Value::Null);
                        vm_arc.invoke_render_opt_sync(fn_id, vec![msg_val, opts_val])
                    }),
                )?;
                self.vm.undefined()
            }
            "registerMarkdownTransformer" => {
                let Some(fn_h) = arg0 else {
                    return Err(pi_js::Error::msg("Markdown transformer function is required"));
                };
                if !fn_h.is_function() {
                    return Err(pi_js::Error::msg("Markdown transformer must be a function"));
                }
                let fn_id = self.store_fn_handle(fn_h.clone())?;
                let vm_weak = Arc::downgrade(self);
                api.register_markdown_transformer(Arc::new(move |md, ctx| {
                    let Some(vm_arc) = vm_weak.upgrade() else {
                        return md.to_string();
                    };
                    let ctx_val = serde_json::to_value(ctx).unwrap_or(Value::Null);
                    vm_arc.invoke_markdown_transformer_sync(fn_id, md, ctx_val)
                }))?;
                self.vm.undefined()
            }
            "registerEntryRenderer" => {
                let custom_type = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let Some(fn_h) = arg1 else {
                    return Err(pi_js::Error::msg("Entry renderer function is required"));
                };
                if !fn_h.is_function() {
                    return Err(pi_js::Error::msg("Entry renderer must be a function"));
                }
                let fn_id = self.store_fn_handle(fn_h.clone())?;
                let vm_weak = Arc::downgrade(self);
                api.register_entry_renderer(
                    &custom_type,
                    Arc::new(move |entry, opts, _theme| {
                        let vm_arc = vm_weak.upgrade()?;
                        let entry_val = serde_json::to_value(entry).unwrap_or(Value::Null);
                        let opts_val = serde_json::to_value(opts).unwrap_or(Value::Null);
                        vm_arc.invoke_render_opt_sync(fn_id, vec![entry_val, opts_val])
                    }),
                )?;
                self.vm.undefined()
            }
            "sendMessage" => {
                let msg_val = arg0
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let opts_val = arg1
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let draft: super::types::mcp_wire::CustomMessageDraft = serde_json::from_value(msg_val)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid sendMessage payload: {e}")))?;
                let opts = if opts_val.is_null() {
                    None
                } else {
                    serde_json::from_value(opts_val).ok()
                };
                api.send_message(draft, opts)?;
                self.vm.undefined()
            }
            "sendUserMessage" => {
                let content_val = arg0
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let opts_val = arg1
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let content: super::types::mcp_wire::CustomMessageContent = serde_json::from_value(content_val)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid sendUserMessage content: {e}")))?;
                let opts = if opts_val.is_null() {
                    None
                } else {
                    serde_json::from_value(opts_val).ok()
                };
                api.send_user_message(content, opts)?;
                self.vm.undefined()
            }
            "appendEntry" => {
                let custom_type = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let data = arg1
                    .filter(|h| !h.is_undefined())
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?;
                api.append_entry(&custom_type, data)?;
                self.vm.undefined()
            }
            "setSessionName" => {
                let name = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                api.set_session_name(&name)?;
                self.vm.undefined()
            }
            "getSessionName" => match api.get_session_name()? {
                Some(s) => self.vm.new_string(&s),
                None => self.vm.undefined(),
            },
            "setLabel" => {
                let entry_id = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let label = arg1
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| h.to_string())
                    .transpose()?;
                api.set_label(&entry_id, label.as_deref())?;
                self.vm.undefined()
            }
            "exec" => {
                let payload = arg0
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let command = payload
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let exec_args: Vec<String> = payload
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let opts_val = payload.get("options").cloned().unwrap_or(Value::Null);
                let timeout = opts_val.get("timeout").and_then(|v| v.as_u64());
                let cwd = opts_val.get("cwd").and_then(|v| v.as_str()).map(|s| s.to_string());
                let api_c = api.clone();
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    let res = api_c
                        .exec(
                            &command,
                            &exec_args,
                            Some(ExecOptions {
                                signal: None,
                                timeout,
                                cwd,
                            }),
                        )
                        .await?;
                    Ok(HostJsResult::ExecResult(res))
                });
                self.bridge.push_async(&self.vm, fut)
            }
            "getActiveTools" => {
                let val = serde_json::to_value(api.get_active_tools()?).unwrap_or(Value::Array(vec![]));
                self.vm.from_json_value(&val)
            }
            "getAllTools" => {
                let val = serde_json::to_value(api.get_all_tools()?).unwrap_or(Value::Array(vec![]));
                self.vm.from_json_value(&val)
            }
            "getSettings" => {
                let val = serde_json::to_value(api.get_settings()?).unwrap_or(Value::Object(Default::default()));
                self.vm.from_json_value(&val)
            }
            "setActiveTools" => {
                let val = arg0
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let names: Vec<String> = val
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                api.set_active_tools(names)?;
                self.vm.undefined()
            }
            "getCommands" => {
                let val = serde_json::to_value(api.get_commands()?).unwrap_or(Value::Array(vec![]));
                self.vm.from_json_value(&val)
            }
            "setModel" => {
                let val = arg0
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let model: Model = serde_json::from_value(val)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid model passed to setModel: {e}")))?;
                let api_c = api.clone();
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    let ok = api_c.set_model(model).await?;
                    Ok(HostJsResult::Json(Value::Bool(ok)))
                });
                self.bridge.push_async(&self.vm, fut)
            }
            "getThinkingLevel" => {
                let lvl = api.get_thinking_level()?;
                let val = serde_json::to_value(lvl).unwrap_or(Value::String("off".to_string()));
                self.vm.from_json_value(&val)
            }
            "setThinkingLevel" => {
                let val = arg0
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let lvl: pi_agent_core::types::ThinkingLevel = serde_json::from_value(val)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid thinking level: {e}")))?;
                api.set_thinking_level(lvl)?;
                self.vm.undefined()
            }
            "registerProvider" => {
                let name = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let cfg_val = arg1
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let config = ProviderConfig {
                    name: cfg_val.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    base_url: cfg_val.get("baseUrl").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    api_key: cfg_val.get("apiKey").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    api: cfg_val.get("api").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    headers: cfg_val
                        .get("headers")
                        .cloned()
                        .and_then(|v| serde_json::from_value(v).ok()),
                    auth_header: cfg_val.get("authHeader").and_then(|v| v.as_bool()),
                    models: cfg_val
                        .get("models")
                        .cloned()
                        .and_then(|v| serde_json::from_value(v).ok()),
                    ..Default::default()
                };
                api.register_provider(&name, config)?;
                self.vm.undefined()
            }
            "unregisterProvider" => {
                let name = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                api.unregister_provider(&name)?;
                self.vm.undefined()
            }
            "registerMcpServer" => {
                let name = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let raw = arg1
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                api.register_mcp_server(&name, raw)?;
                self.vm.undefined()
            }
            "unregisterMcpServer" => {
                let name = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                api.unregister_mcp_server(&name)?;
                self.vm.undefined()
            }
            "getMcpServers" => {
                let list = api.get_mcp_servers()?;
                let val = serde_json::to_value(&list).unwrap_or(Value::Array(vec![]));
                self.vm.from_json_value(&val)
            }
            "eventsEmit" => {
                let channel = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let data = arg1
                    .filter(|h| !h.is_undefined())
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                api.events().emit(&channel, data);
                self.vm.undefined()
            }
            "eventsOn" => {
                let channel = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let Some(fn_h) = arg1 else {
                    return Err(pi_js::Error::msg("Event handler is required"));
                };
                let fn_id = self.store_fn_handle(fn_h.clone())?;
                let vm_weak = Arc::downgrade(self);
                let unsub = api.events().on(
                    &channel,
                    Arc::new(move |data| {
                        let Some(vm_arc) = vm_weak.upgrade() else {
                            return;
                        };
                        vm_arc.invoke_eventbus_listener_sync(fn_id, data);
                    }),
                );
                let unsub_id = self.alloc_id();
                self.registries
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .unsubscribers
                    .insert(unsub_id, unsub);
                self.vm.new_number(unsub_id as f64)
            }
            _ => self.vm.undefined(),
        }
    }

    fn handle_ctx_call(
        self: &Arc<Self>,
        ctx_id: u64,
        method: &str,
        arg0: Option<&JSValueHandle>,
        arg1: Option<&JSValueHandle>,
        arg2: Option<&JSValueHandle>,
    ) -> pi_js::Result<JSValueHandle> {
        let (base_ctx, cmd_ctx, tool_ctx) = {
            let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
            let entry = reg
                .contexts
                .get(&ctx_id)
                .ok_or_else(|| pi_js::Error::msg("Extension context is no longer available"))?;
            match &entry.kind {
                JsContextKind::Extension(c) => (c.clone(), None, None),
                JsContextKind::Command(c) => (c.as_context().clone(), Some(c.clone()), None),
                JsContextKind::Tool(c) => (c.as_context().clone(), None, Some(c.clone())),
            }
        };

        match method {
            "mode" => {
                let v = serde_json::to_value(base_ctx.mode()?).unwrap_or(Value::String("print".to_string()));
                self.vm.from_json_value(&v)
            }
            "hasUI" => self.vm.new_boolean(base_ctx.has_ui()?),
            "cwd" => self.vm.new_string(&base_ctx.cwd()?),
            "model" => match base_ctx.model()? {
                Some(m) => self
                    .vm
                    .from_json_value(&serde_json::to_value(&m).unwrap_or(Value::Null)),
                None => self.vm.undefined(),
            },
            "scopedModels" => {
                let list = base_ctx.scoped_models()?;
                self.vm
                    .from_json_value(&serde_json::to_value(&list).unwrap_or(Value::Array(vec![])))
            }
            "thinkingLevel" => match base_ctx.thinking_level()? {
                Some(t) => self
                    .vm
                    .from_json_value(&serde_json::to_value(&t).unwrap_or(Value::Null)),
                None => self.vm.undefined(),
            },
            "isIdle" => self.vm.new_boolean(base_ctx.is_idle()?),
            "isProjectTrusted" => self.vm.new_boolean(base_ctx.is_project_trusted()?),
            "signal" => match base_ctx.signal()? {
                Some(sig) => {
                    let sig_id = self.alloc_id();
                    self.registries
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .signals
                        .insert(sig_id, sig);
                    self.build_js_signal(sig_id)
                }
                None => self.vm.undefined(),
            },
            "abort" => {
                base_ctx.abort()?;
                self.vm.undefined()
            }
            "hasPendingMessages" => self.vm.new_boolean(base_ctx.has_pending_messages()?),
            "shutdown" => {
                base_ctx.shutdown()?;
                self.vm.undefined()
            }
            "getContextUsage" => match base_ctx.get_context_usage()? {
                Some(u) => self
                    .vm
                    .from_json_value(&serde_json::to_value(&u).unwrap_or(Value::Null)),
                None => self.vm.undefined(),
            },
            "compact" => {
                let val = arg0
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?;
                let custom_instructions = val
                    .as_ref()
                    .and_then(|v| v.get("customInstructions"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                base_ctx.compact(custom_instructions.map(|ci| super::types::CompactOptions {
                    custom_instructions: Some(ci),
                    on_complete: None,
                    on_error: None,
                }))?;
                self.vm.undefined()
            }
            "getSystemPrompt" => self.vm.new_string(&base_ctx.get_system_prompt()?),

            // sessionManager accessors
            "sm_getCwd" => self.vm.new_string(&base_ctx.session_manager()?.get_cwd()),
            "sm_getSessionDir" => self.vm.new_string(&base_ctx.session_manager()?.get_session_dir()),
            "sm_getSessionId" => self.vm.new_string(&base_ctx.session_manager()?.get_session_id()),
            "sm_getSessionFile" => match base_ctx.session_manager()?.get_session_file() {
                Some(p) => self.vm.new_string(&p),
                None => self.vm.undefined(),
            },
            "sm_getLeafId" => match base_ctx.session_manager()?.get_leaf_id() {
                Some(p) => self.vm.new_string(&p),
                None => self.vm.null(),
            },
            "sm_getEntry" => {
                let id = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                match base_ctx.session_manager()?.get_entry(&id) {
                    Some(e) => self
                        .vm
                        .from_json_value(&serde_json::to_value(&e).unwrap_or(Value::Null)),
                    None => self.vm.undefined(),
                }
            }
            "sm_getLabel" => {
                let id = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                match base_ctx.session_manager()?.get_label(&id) {
                    Some(l) => self.vm.new_string(&l),
                    None => self.vm.undefined(),
                }
            }
            "sm_getBranch" => {
                let from = arg0
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| h.to_string())
                    .transpose()?;
                let entries = base_ctx.session_manager()?.get_branch(from.as_deref());
                self.vm
                    .from_json_value(&serde_json::to_value(&entries).unwrap_or(Value::Array(vec![])))
            }
            "sm_getEntries" => {
                let entries = base_ctx.session_manager()?.get_entries();
                self.vm
                    .from_json_value(&serde_json::to_value(&entries).unwrap_or(Value::Array(vec![])))
            }
            "sm_getSessionName" => match base_ctx.session_manager()?.get_session_name() {
                Some(n) => self.vm.new_string(&n),
                None => self.vm.undefined(),
            },

            // modelRegistry accessors
            "mr_getAvailable" => {
                let models = base_ctx.model_registry()?.0.get_available_models();
                self.vm
                    .from_json_value(&serde_json::to_value(&models).unwrap_or(Value::Array(vec![])))
            }
            "mr_getApiKey" => {
                let model_val = arg0
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let model: Model =
                    serde_json::from_value(model_val).map_err(|e| pi_js::Error::msg(format!("Invalid model: {e}")))?;
                let mr = base_ctx.model_registry()?;
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    match mr.0.get_api_key(&model).await? {
                        Some(k) => Ok(HostJsResult::Json(Value::String(k))),
                        None => Ok(HostJsResult::Undefined),
                    }
                });
                self.bridge.push_async(&self.vm, fut)
            }

            // UI context
            "ui_notify" => {
                let msg = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let kind = arg1
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| h.to_string())
                    .transpose()?;
                let nt = match kind.as_deref() {
                    Some("warning") => Some(super::types::NotifyType::Warning),
                    Some("error") => Some(super::types::NotifyType::Error),
                    Some("info") => Some(super::types::NotifyType::Info),
                    _ => None,
                };
                base_ctx.ui()?.notify(&msg, nt);
                self.vm.undefined()
            }
            "ui_setStatus" => {
                let key = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let text = arg1
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| h.to_string())
                    .transpose()?;
                base_ctx.ui()?.set_status(&key, text.as_deref());
                self.vm.undefined()
            }
            "ui_setTitle" => {
                let title = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                base_ctx.ui()?.set_title(&title);
                self.vm.undefined()
            }
            "ui_setWorkingMessage" => {
                let msg = arg0
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| h.to_string())
                    .transpose()?;
                base_ctx.ui()?.set_working_message(msg.as_deref());
                self.vm.undefined()
            }
            "ui_setWorkingVisible" => {
                let vis = arg0.map(|h| h.to_boolean()).transpose()?.unwrap_or(true);
                base_ctx.ui()?.set_working_visible(vis);
                self.vm.undefined()
            }
            "ui_setHiddenThinkingLabel" => {
                let lbl = arg0
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| h.to_string())
                    .transpose()?;
                base_ctx.ui()?.set_hidden_thinking_label(lbl.as_deref());
                self.vm.undefined()
            }
            "ui_setWidget" => {
                let key = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let lines = if let Some(a) = arg1 {
                    if a.is_array() {
                        let val = self.vm.to_json_value(a)?;
                        Some(
                            val.as_array()
                                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                                .unwrap_or_default(),
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };
                let opts = arg2
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .and_then(|v| serde_json::from_value(v).ok());
                base_ctx.ui()?.set_widget_lines(&key, lines, opts);
                self.vm.undefined()
            }
            "ui_pasteToEditor" => {
                let text = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                base_ctx.ui()?.paste_to_editor(&text);
                self.vm.undefined()
            }
            "ui_setEditorText" => {
                let text = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                base_ctx.ui()?.set_editor_text(&text);
                self.vm.undefined()
            }
            "ui_getEditorText" => self.vm.new_string(&base_ctx.ui()?.get_editor_text()),
            "ui_getToolsExpanded" => self.vm.new_boolean(base_ctx.ui()?.get_tools_expanded()),
            "ui_setToolsExpanded" => {
                let exp = arg0.map(|h| h.to_boolean()).transpose()?.unwrap_or(false);
                base_ctx.ui()?.set_tools_expanded(exp);
                self.vm.undefined()
            }
            "ui_getAllThemes" => {
                let list = base_ctx.ui()?.get_all_themes();
                self.vm
                    .from_json_value(&serde_json::to_value(&list).unwrap_or(Value::Array(vec![])))
            }
            "ui_setTheme" => {
                let val = arg0
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let name = val.as_str().unwrap_or("");
                let res = base_ctx.ui()?.set_theme_by_name(name);
                self.vm
                    .from_json_value(&serde_json::to_value(&res).unwrap_or(Value::Null))
            }
            "ui_themeCall" => {
                let fn_name = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let p1 = arg1.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let p2 = arg2.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let theme = base_ctx.ui()?.theme();
                let out = match fn_name.as_str() {
                    "fg" => theme.fg(&p1, &p2),
                    "bg" => theme.bg(&p1, &p2),
                    "bold" => theme.bold(&p1),
                    "italic" => theme.italic(&p1),
                    "underline" => theme.underline(&p1),
                    "inverse" => theme.inverse(&p1),
                    "strikethrough" => theme.strikethrough(&p1),
                    "getFgAnsi" => theme.get_fg_ansi(&p1),
                    "getBgAnsi" => theme.get_bg_ansi(&p1),
                    _ => p1,
                };
                self.vm.new_string(&out)
            }
            "ui_select" => {
                let title = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let opts_val = arg1
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let options: Vec<String> = opts_val
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let ui = base_ctx.ui()?;
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    match ui.select(&title, options, None).await? {
                        Some(choice) => Ok(HostJsResult::Json(Value::String(choice))),
                        None => Ok(HostJsResult::Undefined),
                    }
                });
                self.bridge.push_async(&self.vm, fut)
            }
            "ui_confirm" => {
                let title = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let message = arg1.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let ui = base_ctx.ui()?;
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    let ok = ui.confirm(&title, &message, None).await?;
                    Ok(HostJsResult::Json(Value::Bool(ok)))
                });
                self.bridge.push_async(&self.vm, fut)
            }
            "ui_input" => {
                let title = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let placeholder = arg1
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| h.to_string())
                    .transpose()?;
                let ui = base_ctx.ui()?;
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    match ui.input(&title, placeholder.as_deref(), None).await? {
                        Some(text) => Ok(HostJsResult::Json(Value::String(text))),
                        None => Ok(HostJsResult::Undefined),
                    }
                });
                self.bridge.push_async(&self.vm, fut)
            }
            "ui_editor" => {
                let title = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let prefill = arg1
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| h.to_string())
                    .transpose()?;
                let ui = base_ctx.ui()?;
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    match ui.editor(&title, prefill.as_deref()).await? {
                        Some(text) => Ok(HostJsResult::Json(Value::String(text))),
                        None => Ok(HostJsResult::Undefined),
                    }
                });
                self.bridge.push_async(&self.vm, fut)
            }

            // ToolContext methods
            "tool_tools" => {
                let tools = match &tool_ctx {
                    Some(tc) => tc.tools()?,
                    None => base_ctx.backend.callable_tools(),
                };
                let summary: Vec<Value> = tools
                    .into_iter()
                    .map(|t| {
                        serde_json::json!({
                            "name": t.name,
                            "label": t.label,
                            "description": t.description,
                            "parameters": t.parameters,
                        })
                    })
                    .collect();
                self.vm.from_json_value(&Value::Array(summary))
            }
            "tool_executeTool" => {
                let name = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let args_val = arg1
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Object(Default::default()));
                let tc = tool_ctx
                    .ok_or_else(|| pi_js::Error::msg("executeTool is only available in ExtensionToolContext"))?;
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    let outcome = tc.execute_tool(&name, args_val, None).await?;
                    Ok(HostJsResult::AgentToolCallOutcome(outcome))
                });
                self.bridge.push_async(&self.vm, fut)
            }

            // CommandContext methods
            "cmd_getSystemPromptOptions" => {
                let opts = match &cmd_ctx {
                    Some(c) => c.get_system_prompt_options()?,
                    None => base_ctx.backend.get_system_prompt_options(),
                };
                self.vm
                    .from_json_value(&serde_json::to_value(&opts).unwrap_or(Value::Null))
            }
            "cmd_waitForIdle" => {
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    if let Some(c) = cmd_ctx {
                        c.wait_for_idle().await?;
                    } else {
                        base_ctx.backend.wait_for_idle().await?;
                    }
                    Ok(HostJsResult::Undefined)
                });
                self.bridge.push_async(&self.vm, fut)
            }
            "cmd_reload" => {
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    if let Some(c) = cmd_ctx {
                        c.reload().await?;
                    } else {
                        base_ctx.backend.reload().await?;
                    }
                    Ok(HostJsResult::Undefined)
                });
                self.bridge.push_async(&self.vm, fut)
            }
            "cmd_newSession" => {
                let val = arg0
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?;
                let parent_session = val
                    .as_ref()
                    .and_then(|v| v.get("parentSession"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    let opts = super::types::NewSessionOptions {
                        parent_session,
                        setup: None,
                        with_session: None,
                    };
                    let res = if let Some(c) = cmd_ctx {
                        c.new_session(Some(opts)).await?
                    } else {
                        base_ctx.backend.new_session(Some(opts)).await?
                    };
                    Ok(HostJsResult::Json(serde_json::to_value(&res).unwrap_or(Value::Null)))
                });
                self.bridge.push_async(&self.vm, fut)
            }
            "cmd_fork" => {
                let entry_id = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let val = arg1
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?;
                let position = val
                    .as_ref()
                    .and_then(|v| v.get("position"))
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok());
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    let opts = super::types::ForkOptions {
                        position,
                        with_session: None,
                    };
                    let res = if let Some(c) = cmd_ctx {
                        c.fork(&entry_id, Some(opts)).await?
                    } else {
                        base_ctx.backend.fork(&entry_id, Some(opts)).await?
                    };
                    Ok(HostJsResult::Json(serde_json::to_value(&res).unwrap_or(Value::Null)))
                });
                self.bridge.push_async(&self.vm, fut)
            }
            "cmd_navigateTree" => {
                let target_id = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let val = arg1
                    .filter(|h| !h.is_undefined() && !h.is_null())
                    .map(|h| self.vm.to_json_value(h))
                    .transpose()?;
                let opts = super::types::NavigateTreeOptions {
                    summarize: val.as_ref().and_then(|v| v.get("summarize")).and_then(|b| b.as_bool()),
                    custom_instructions: val
                        .as_ref()
                        .and_then(|v| v.get("customInstructions"))
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                    replace_instructions: val
                        .as_ref()
                        .and_then(|v| v.get("replaceInstructions"))
                        .and_then(|b| b.as_bool()),
                    label: val
                        .as_ref()
                        .and_then(|v| v.get("label"))
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                };
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    let res = if let Some(c) = cmd_ctx {
                        c.navigate_tree(&target_id, Some(opts)).await?
                    } else {
                        base_ctx.backend.navigate_tree(&target_id, Some(opts)).await?
                    };
                    Ok(HostJsResult::Json(serde_json::to_value(&res).unwrap_or(Value::Null)))
                });
                self.bridge.push_async(&self.vm, fut)
            }
            "cmd_switchSession" => {
                let path = arg0.map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    let res = if let Some(c) = cmd_ctx {
                        c.switch_session(&path, None).await?
                    } else {
                        base_ctx.backend.switch_session(&path, None).await?
                    };
                    Ok(HostJsResult::Json(serde_json::to_value(&res).unwrap_or(Value::Null)))
                });
                self.bridge.push_async(&self.vm, fut)
            }

            _ => self.vm.undefined(),
        }
    }

    fn build_js_signal(&self, sig_id: u64) -> pi_js::Result<JSValueHandle> {
        let global = self.vm.global()?;
        let builder = global.get_prop("__pi_build_signal")?;
        let id_h = self.vm.new_number(sig_id as f64)?;
        let undef = self.vm.undefined()?;
        let res = self.vm.call_function(&builder, &undef, &[id_h.clone()]);
        id_h.dispose();
        builder.dispose();
        undef.dispose();
        global.dispose();
        res
    }

    async fn invoke_event_handler(&self, fn_id: u64, ev_json: Value, ctx: ExtensionContext) -> pi_js::Result<Value> {
        let ctx_id = self.alloc_id();
        self.registries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contexts
            .insert(
                ctx_id,
                StoredContextEntry {
                    kind: JsContextKind::Extension(ctx),
                },
            );

        let result = self
            .run_js_promise(move |vm| {
                let global = vm.global()?;
                let invoke_fn = global.get_prop("__pi_invoke_event")?;
                let fn_h = {
                    let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
                    reg.fn_handles
                        .get(&fn_id)
                        .ok_or_else(|| pi_js::Error::msg("Missing JS event handler handle"))?
                        .dup()?
                };
                let ev_h = vm.from_json_value(&ev_json)?;
                let ctx_id_h = vm.new_number(ctx_id as f64)?;
                let kind_h = vm.new_string("ext")?;
                let undef = vm.undefined()?;
                let res = vm.call_function(
                    &invoke_fn,
                    &undef,
                    &[fn_h.clone(), ev_h.clone(), ctx_id_h.clone(), kind_h.clone()],
                );
                fn_h.dispose();
                ev_h.dispose();
                ctx_id_h.dispose();
                kind_h.dispose();
                undef.dispose();
                invoke_fn.dispose();
                global.dispose();
                res
            })
            .await;

        let _ = ctx_id;

        result
    }

    async fn invoke_tool_execute(
        &self,
        exec_fn_id: u64,
        tool_call_id: String,
        params: Value,
        signal: Option<AbortSignal>,
        on_update: Option<pi_agent_core::types::AgentToolUpdateCallback>,
        ctx: ExtensionToolContext,
    ) -> pi_js::Result<AgentToolResult> {
        let ctx_id = self.alloc_id();
        let sig_id = signal.map(|s| {
            let id = self.alloc_id();
            self.registries
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .signals
                .insert(id, s);
            id
        });
        let update_id = on_update.map(|cb| {
            let id = self.alloc_id();
            self.registries
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .update_callbacks
                .insert(id, cb);
            id
        });
        self.registries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contexts
            .insert(
                ctx_id,
                StoredContextEntry {
                    kind: JsContextKind::Tool(ctx),
                },
            );

        let result = self
            .run_js_promise(move |vm| {
                let global = vm.global()?;
                let invoke_fn = global.get_prop("__pi_invoke_tool")?;
                let fn_h = {
                    let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
                    reg.fn_handles
                        .get(&exec_fn_id)
                        .ok_or_else(|| pi_js::Error::msg("Missing JS tool execute handle"))?
                        .dup()?
                };
                let id_h = vm.new_string(&tool_call_id)?;
                let params_h = vm.from_json_value(&params)?;
                let sig_h = match sig_id {
                    Some(id) => vm.new_number(id as f64)?,
                    None => vm.null()?,
                };
                let upd_h = match update_id {
                    Some(id) => vm.new_number(id as f64)?,
                    None => vm.null()?,
                };
                let ctx_id_h = vm.new_number(ctx_id as f64)?;
                let undef = vm.undefined()?;
                let res = vm.call_function(
                    &invoke_fn,
                    &undef,
                    &[
                        fn_h.clone(),
                        id_h.clone(),
                        params_h.clone(),
                        sig_h.clone(),
                        upd_h.clone(),
                        ctx_id_h.clone(),
                    ],
                );
                fn_h.dispose();
                id_h.dispose();
                params_h.dispose();
                sig_h.dispose();
                upd_h.dispose();
                ctx_id_h.dispose();
                undef.dispose();
                invoke_fn.dispose();
                global.dispose();
                res
            })
            .await;

        {
            let mut reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
            let _ = ctx_id;
            if let Some(id) = sig_id {
                reg.signals.shift_remove(&id);
            }
            if let Some(id) = update_id {
                reg.update_callbacks.shift_remove(&id);
            }
        }

        let val = result?;
        parse_js_tool_result(&val)
    }

    async fn invoke_command_handler(
        &self,
        fn_id: u64,
        args: String,
        ctx: ExtensionCommandContext,
    ) -> pi_js::Result<()> {
        let ctx_id = self.alloc_id();
        self.registries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contexts
            .insert(
                ctx_id,
                StoredContextEntry {
                    kind: JsContextKind::Command(ctx),
                },
            );

        let result = self
            .run_js_promise(move |vm| {
                let global = vm.global()?;
                let invoke_fn = global.get_prop("__pi_invoke_cmd")?;
                let fn_h = {
                    let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
                    reg.fn_handles
                        .get(&fn_id)
                        .ok_or_else(|| pi_js::Error::msg("Missing JS command handle"))?
                        .dup()?
                };
                let args_h = vm.new_string(&args)?;
                let ctx_id_h = vm.new_number(ctx_id as f64)?;
                let undef = vm.undefined()?;
                let res = vm.call_function(&invoke_fn, &undef, &[fn_h.clone(), args_h.clone(), ctx_id_h.clone()]);
                fn_h.dispose();
                args_h.dispose();
                ctx_id_h.dispose();
                undef.dispose();
                invoke_fn.dispose();
                global.dispose();
                res
            })
            .await;

        let _ = ctx_id;
        let _ = result?;
        Ok(())
    }

    async fn invoke_completions(&self, fn_id: u64, prefix: &str) -> pi_js::Result<Option<Vec<AutocompleteItem>>> {
        let prefix = prefix.to_string();
        let val = self
            .run_js_promise(move |vm| {
                let fn_h = {
                    let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
                    reg.fn_handles
                        .get(&fn_id)
                        .ok_or_else(|| pi_js::Error::msg("Missing JS completions handle"))?
                        .dup()?
                };
                let p_h = vm.new_string(&prefix)?;
                let undef = vm.undefined()?;
                let res = vm.call_function(&fn_h, &undef, &[p_h.clone()]);
                fn_h.dispose();
                p_h.dispose();
                undef.dispose();
                res
            })
            .await?;
        if val.is_null() {
            return Ok(None);
        }
        let items = val.as_array().map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let obj = item.as_object()?;
                    let value = obj.get("value")?.as_str()?.to_string();
                    let label = obj.get("label").and_then(|v| v.as_str()).unwrap_or(&value).to_string();
                    let description = obj.get("description").and_then(|v| v.as_str()).map(str::to_string);
                    Some(AutocompleteItem {
                        value,
                        label,
                        description,
                    })
                })
                .collect::<Vec<_>>()
        });
        Ok(items)
    }

    async fn invoke_shortcut_handler(&self, fn_id: u64, ctx: ExtensionContext) -> pi_js::Result<()> {
        let ctx_id = self.alloc_id();
        self.registries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contexts
            .insert(
                ctx_id,
                StoredContextEntry {
                    kind: JsContextKind::Extension(ctx),
                },
            );

        let res = self
            .run_js_promise(move |vm| {
                let global = vm.global()?;
                let build_fn = global.get_prop("__pi_build_ctx")?;
                let ctx_id_h = vm.new_number(ctx_id as f64)?;
                let kind_h = vm.new_string("ext")?;
                let undef = vm.undefined()?;
                let ctx_obj = vm.call_function(&build_fn, &undef, &[ctx_id_h.clone(), kind_h.clone()])?;
                let fn_h = {
                    let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
                    reg.fn_handles
                        .get(&fn_id)
                        .ok_or_else(|| pi_js::Error::msg("Missing JS shortcut handle"))?
                        .dup()?
                };
                let r = vm.call_function(&fn_h, &undef, &[ctx_obj.clone()]);
                fn_h.dispose();
                ctx_obj.dispose();
                kind_h.dispose();
                ctx_id_h.dispose();
                build_fn.dispose();
                undef.dispose();
                global.dispose();
                r
            })
            .await;

        let _ = ctx_id;
        let _ = res?;
        Ok(())
    }

    async fn invoke_user_bash_exec(
        &self,
        exec_fn_id: u64,
        command: String,
        cwd: String,
        timeout: Option<f64>,
    ) -> pi_js::Result<crate::core::tools::bash::BashExecResult> {
        let val = self
            .run_js_promise(move |vm| {
                let fn_h = {
                    let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
                    reg.fn_handles
                        .get(&exec_fn_id)
                        .ok_or_else(|| pi_js::Error::msg("Missing JS user_bash exec handle"))?
                        .dup()?
                };
                let cmd_h = vm.new_string(&command)?;
                let cwd_h = vm.new_string(&cwd)?;
                let opts_val = match timeout {
                    Some(t) => serde_json::json!({ "timeout": t }),
                    None => serde_json::json!({}),
                };
                let opts_h = vm.from_json_value(&opts_val)?;
                let undef = vm.undefined()?;
                let res = vm.call_function(&fn_h, &undef, &[cmd_h.clone(), cwd_h.clone(), opts_h.clone()]);
                fn_h.dispose();
                cmd_h.dispose();
                cwd_h.dispose();
                opts_h.dispose();
                undef.dispose();
                res
            })
            .await?;
        let exit_code = val.get("exitCode").and_then(|v| v.as_i64());
        Ok(crate::core::tools::bash::BashExecResult { exit_code })
    }

    fn invoke_eventbus_listener_sync(&self, fn_id: u64, data: Value) {
        let already_in_turn = ACTIVE_EXT_VM_ID.with(|c| c.get() == Some(self.id));
        let invoke = || -> pi_js::Result<()> {
            let fn_h = {
                let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
                let Some(h) = reg.fn_handles.get(&fn_id) else {
                    return Ok(());
                };
                h.dup()?
            };
            let arg_h = self.vm.from_json_value(&data)?;
            let undef = self.vm.undefined()?;
            let res = self.vm.call_function(&fn_h, &undef, &[arg_h.clone()]);
            fn_h.dispose();
            arg_h.dispose();
            undef.dispose();
            if let Ok(h) = res {
                h.dispose();
            }
            let _ = self.vm.execute_pending_jobs();
            Ok(())
        };

        if already_in_turn {
            let _ = invoke();
        } else if let Ok(_guard) = self.vm_lock.try_lock() {
            let _ = enter_vm_turn(self.id, invoke);
        }
    }

    fn invoke_render_sync(&self, fn_id: u64, args_json: Vec<Value>) -> Arc<dyn Component> {
        let already_in_turn = ACTIVE_EXT_VM_ID.with(|c| c.get() == Some(self.id));
        let run = || -> pi_js::Result<String> {
            let global = self.vm.global()?;
            let helper = global.get_prop("__pi_render_text")?;
            let fn_h = {
                let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
                let Some(h) = reg.fn_handles.get(&fn_id) else {
                    global.dispose();
                    helper.dispose();
                    return Ok(String::new());
                };
                h.dup()?
            };
            let mut js_args = Vec::with_capacity(args_json.len() + 1);
            js_args.push(fn_h);
            for a in args_json {
                js_args.push(self.vm.from_json_value(&a)?);
            }
            let undef = self.vm.undefined()?;
            let out_h = self.vm.call_function(&helper, &undef, &js_args);
            for h in js_args {
                h.dispose();
            }
            undef.dispose();
            helper.dispose();
            global.dispose();
            match out_h {
                Ok(h) => {
                    let s = h.to_string().unwrap_or_default();
                    h.dispose();
                    Ok(s)
                }
                Err(_) => Ok(String::new()),
            }
        };

        let text = if already_in_turn {
            run().unwrap_or_default()
        } else if let Ok(_guard) = self.vm_lock.try_lock() {
            enter_vm_turn(self.id, run).unwrap_or_default()
        } else {
            String::new()
        };
        Arc::new(TuiText::new(Some(&text), Some(0), Some(0), None))
    }

    fn invoke_render_opt_sync(&self, fn_id: u64, args_json: Vec<Value>) -> Option<Arc<dyn Component>> {
        let already_in_turn = ACTIVE_EXT_VM_ID.with(|c| c.get() == Some(self.id));
        let run = || -> pi_js::Result<Option<String>> {
            let fn_h = {
                let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
                let Some(h) = reg.fn_handles.get(&fn_id) else {
                    return Ok(None);
                };
                h.dup()?
            };
            let mut js_args = Vec::with_capacity(args_json.len() + 1);
            for a in args_json {
                js_args.push(self.vm.from_json_value(&a)?);
            }
            js_args.push(self.vm.new_object()?);
            let undef = self.vm.undefined()?;
            let out_h = self.vm.call_function(&fn_h, &undef, &js_args);
            fn_h.dispose();
            for h in js_args {
                h.dispose();
            }
            undef.dispose();
            let Ok(h) = out_h else {
                return Ok(None);
            };
            if h.is_null() || h.is_undefined() {
                h.dispose();
                return Ok(None);
            }
            let text = if h.is_object() {
                let render_h = h.get_prop("render")?;
                let rendered = if render_h.is_function() {
                    let w_h = self.vm.new_number(80.0)?;
                    let r_res = self.vm.call_function(&render_h, &h, &[w_h.clone()]);
                    w_h.dispose();
                    if let Ok(lines_h) = r_res {
                        let s = if let Ok(v) = self.vm.to_json_value(&lines_h)
                            && let Some(arr) = v.as_array()
                        {
                            arr.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("\n")
                        } else {
                            lines_h.to_string().unwrap_or_default()
                        };
                        lines_h.dispose();
                        s
                    } else {
                        String::new()
                    }
                } else {
                    h.to_string().unwrap_or_default()
                };
                render_h.dispose();
                rendered
            } else {
                h.to_string().unwrap_or_default()
            };
            h.dispose();
            Ok(Some(text))
        };

        let opt_text = if already_in_turn {
            run().ok().flatten()
        } else if let Ok(_guard) = self.vm_lock.try_lock() {
            enter_vm_turn(self.id, run).ok().flatten()
        } else {
            None
        };
        opt_text.map(|t| Arc::new(TuiText::new(Some(&t), Some(0), Some(0), None)) as Arc<dyn Component>)
    }

    fn invoke_markdown_transformer_sync(&self, fn_id: u64, markdown: &str, ctx_val: Value) -> String {
        let already_in_turn = ACTIVE_EXT_VM_ID.with(|c| c.get() == Some(self.id));
        let run = || -> pi_js::Result<String> {
            let fn_h = {
                let reg = self.registries.lock().unwrap_or_else(|e| e.into_inner());
                let Some(h) = reg.fn_handles.get(&fn_id) else {
                    return Ok(markdown.to_string());
                };
                h.dup()?
            };
            let md_h = self.vm.new_string(markdown)?;
            let ctx_h = self.vm.from_json_value(&ctx_val)?;
            let undef = self.vm.undefined()?;
            let out_h = self.vm.call_function(&fn_h, &undef, &[md_h.clone(), ctx_h.clone()]);
            fn_h.dispose();
            md_h.dispose();
            ctx_h.dispose();
            undef.dispose();
            match out_h {
                Ok(h) => {
                    let s = if h.is_string() {
                        h.to_string().unwrap_or_else(|_| markdown.to_string())
                    } else {
                        markdown.to_string()
                    };
                    h.dispose();
                    Ok(s)
                }
                Err(_) => Ok(markdown.to_string()),
            }
        };

        if already_in_turn {
            run().unwrap_or_else(|_| markdown.to_string())
        } else if let Ok(_guard) = self.vm_lock.try_lock() {
            enter_vm_turn(self.id, run).unwrap_or_else(|_| markdown.to_string())
        } else {
            markdown.to_string()
        }
    }
}

struct TrustContextAdapter(super::types::ProjectTrustContext);

impl super::types::ExtensionContextBackend for TrustContextAdapter {
    fn assert_active(&self) -> pi_js::Result<()> {
        Ok(())
    }
    fn ui(&self) -> Arc<dyn super::types::ExtensionUIContext> {
        Arc::clone(&self.0.ui)
    }
    fn mode(&self) -> super::types::ExtensionMode {
        self.0.mode
    }
    fn has_ui(&self) -> bool {
        self.0.has_ui
    }
    fn cwd(&self) -> String {
        self.0.cwd.clone()
    }
    fn session_manager(&self) -> super::types::mcp_wire::ReadonlySessionManager {
        Default::default()
    }
    fn model_registry(&self) -> super::types::mcp_wire::ModelRegistry {
        Default::default()
    }
    fn model(&self) -> Option<Model> {
        None
    }
    fn scoped_models(&self) -> Vec<super::types::mcp_wire::ScopedModel> {
        Vec::new()
    }
    fn thinking_level(&self) -> Option<pi_agent_core::types::ThinkingLevel> {
        None
    }
    fn is_idle(&self) -> bool {
        true
    }
    fn is_project_trusted(&self) -> bool {
        false
    }
    fn signal(&self) -> Option<AbortSignal> {
        None
    }
    fn abort(&self) {}
    fn has_pending_messages(&self) -> bool {
        false
    }
    fn shutdown(&self) {}
    fn get_context_usage(&self) -> Option<super::types::ContextUsage> {
        None
    }
    fn compact(&self, _options: Option<super::types::CompactOptions>) {}
    fn get_system_prompt(&self) -> String {
        String::new()
    }
    fn get_system_prompt_options(&self) -> super::types::BuildSystemPromptOptions {
        super::types::BuildSystemPromptOptions {
            cwd: self.0.cwd.clone(),
            ..Default::default()
        }
    }
    fn wait_for_idle(&self) -> BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn new_session(
        &self,
        _: Option<super::types::NewSessionOptions>,
    ) -> BoxFuture<pi_js::Result<super::types::CancelledResult>> {
        Box::pin(async { Ok(super::types::CancelledResult { cancelled: false }) })
    }
    fn fork(
        &self,
        _: &str,
        _: Option<super::types::ForkOptions>,
    ) -> BoxFuture<pi_js::Result<super::types::CancelledResult>> {
        Box::pin(async { Ok(super::types::CancelledResult { cancelled: false }) })
    }
    fn navigate_tree(
        &self,
        _: &str,
        _: Option<super::types::NavigateTreeOptions>,
    ) -> BoxFuture<pi_js::Result<super::types::CancelledResult>> {
        Box::pin(async { Ok(super::types::CancelledResult { cancelled: false }) })
    }
    fn switch_session(
        &self,
        _: &str,
        _: Option<super::types::SwitchSessionOptions>,
    ) -> BoxFuture<pi_js::Result<super::types::CancelledResult>> {
        Box::pin(async { Ok(super::types::CancelledResult { cancelled: false }) })
    }
    fn reload(&self) -> BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn callable_tools(&self) -> Vec<AgentTool> {
        Vec::new()
    }
    fn execute_tool(
        &self,
        caller_id: &str,
        name: &str,
        _args: Value,
        _options: super::types::ExecuteToolOptions,
    ) -> BoxFuture<pi_agent_core::types::AgentToolCallOutcome> {
        let id = format!("{caller_id}/0");
        let name = name.to_string();
        Box::pin(async move {
            pi_agent_core::types::AgentToolCallOutcome {
                tool_call: pi_agent_core::types::AgentToolCall {
                    id,
                    name,
                    arguments: pi_ai::types::JsonObject::new(),
                },
                result: AgentToolResult::text("Nested tool calls are not available in this context"),
                is_error: true,
            }
        })
    }
    fn send_message(&self, _: super::types::mcp_wire::CustomMessageDraft, _: Option<super::types::SendMessageOptions>) {
    }
    fn send_user_message(
        &self,
        _: super::types::mcp_wire::CustomMessageContent,
        _: Option<super::types::SendUserMessageOptions>,
    ) {
    }
}

struct QuickJsBashOperations {
    vm: Arc<QuickJsExtensionVm>,
    exec_fn_id: u64,
}

impl crate::core::tools::bash::BashOperations for QuickJsBashOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: crate::core::tools::bash::BashExecOptions,
    ) -> BoxFuture<pi_js::Result<crate::core::tools::bash::BashExecResult>> {
        let vm = Arc::clone(&self.vm);
        let exec_fn_id = self.exec_fn_id;
        let cmd = command.to_string();
        let cwd_str = cwd.to_string();
        let timeout = options.timeout;
        Box::pin(async move { vm.invoke_user_bash_exec(exec_fn_id, cmd, cwd_str, timeout).await })
    }
}

struct QuickJsExtensionFactory {
    vm: Arc<QuickJsExtensionVm>,
    factory_fn_id: u64,
}

#[async_trait]
impl Extension for QuickJsExtensionFactory {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        self.vm.invoke_factory(self.factory_fn_id, pi).await
    }

    fn host_keepalive(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        Some(Arc::clone(&self.vm) as Arc<dyn std::any::Any + Send + Sync>)
    }
}

fn parse_js_tool_result(val: &Value) -> pi_js::Result<AgentToolResult> {
    if val.is_null() {
        return Ok(AgentToolResult {
            content: Vec::new(),
            details: None,
            structured_content: None,
            usage: None,
            is_error: None,
            terminate: None,
            control: None,
        });
    }
    let content_raw = val.get("content").cloned().unwrap_or(Value::Array(vec![]));
    let content: Vec<pi_ai::types::UserContentBlock> = serde_json::from_value(content_raw).unwrap_or_default();
    let details = val.get("details").cloned().filter(|v| !v.is_null());
    let structured_content = val.get("structuredContent").cloned().filter(|v| !v.is_null());
    let usage = val.get("usage").cloned().and_then(|v| serde_json::from_value(v).ok());
    let is_error = val.get("isError").and_then(|v| v.as_bool());
    let terminate = val.get("terminate").and_then(|v| v.as_bool());
    let control = val.get("control").cloned().and_then(|v| serde_json::from_value(v).ok());
    Ok(AgentToolResult {
        content,
        details,
        structured_content,
        usage,
        is_error,
        terminate,
        control,
    })
}

// ---------------------------------------------------------------------------
// Public module loading & extension discovery entrypoints (loader.ts parity)
// ---------------------------------------------------------------------------

async fn load_extension_module_factory(
    resolved_path: &str,
    cache_token: Option<&ExtensionCacheToken>,
) -> pi_js::Result<Option<ExtensionFactory>> {
    if let Some(token) = cache_token {
        let cache = EXTENSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if is_current_cache_token(&cache, Some(token)) {
            if let Some(entry) = cache.entries.get(resolved_path) {
                return Ok(Some(Arc::new(QuickJsExtensionFactory {
                    vm: Arc::clone(&entry.vm),
                    factory_fn_id: entry.factory_fn_id,
                })));
            }
        }
    }

    if !pi_js::fs::exists(resolved_path) {
        return Err(pi_js::Error::msg(format!("Extension file not found: {resolved_path}")));
    }

    let vm = QuickJsExtensionVm::create().await?;
    let canonical = pi_js::fs::realpath(resolved_path).unwrap_or_else(|_| resolved_path.to_string());
    let Some(factory_fn_id) = vm.load_module_factory(&canonical).await? else {
        return Ok(None);
    };

    if let Some(token) = cache_token {
        let mut cache = EXTENSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if is_current_cache_token(&cache, Some(token)) {
            cache.entries.insert(
                resolved_path.to_string(),
                CachedEntry {
                    vm: Arc::clone(&vm),
                    factory_fn_id,
                },
            );
        }
    }

    Ok(Some(Arc::new(QuickJsExtensionFactory { vm, factory_fn_id })))
}

async fn load_extension(
    extension_path: &str,
    cwd: &str,
    event_bus: Arc<dyn EventBus>,
    runtime: ExtensionRuntime,
    cache_token: Option<&ExtensionCacheToken>,
) -> (Option<LoadedExtension>, Option<String>) {
    let resolved_path = resolve_path_wire(extension_path, cwd);
    match load_extension_module_factory(&resolved_path, cache_token).await {
        Ok(None) => (
            None,
            Some(format!(
                "Extension does not export a valid factory function: {extension_path}"
            )),
        ),
        Ok(Some(factory)) => {
            match initialize_extension(&factory, extension_path, &resolved_path, cwd, event_bus, runtime).await {
                Ok(ext) => (Some(ext), None),
                Err(err) => (None, Some(format!("Failed to load extension: {}", err.message()))),
            }
        }
        Err(err) => (None, Some(format!("Failed to load extension: {}", err.message()))),
    }
}

async fn load_extensions_internal(
    paths: &[String],
    cwd: &str,
    event_bus: Option<Arc<dyn EventBus>>,
    runtime: Option<ExtensionRuntime>,
    use_cache: bool,
) -> LoadExtensionsResult {
    let mut extensions = Vec::new();
    let mut errors = Vec::new();
    let warnings: Vec<LoadExtensionWarningEntry> = Vec::new();
    let cache_token = if use_cache {
        Some(use_extension_cache_cwd(cwd))
    } else {
        None
    };
    let resolved_cwd = cache_token
        .as_ref()
        .map(|t| t.cwd.clone())
        .unwrap_or_else(|| resolve_path_wire(cwd, &pi_js::env::cwd()));
    let resolved_event_bus: Arc<dyn EventBus> = event_bus.unwrap_or_else(|| create_event_bus() as Arc<dyn EventBus>);
    let resolved_runtime = runtime.unwrap_or_else(create_extension_runtime);

    for ext_path in paths {
        let (ext, err) = load_extension(
            ext_path,
            &resolved_cwd,
            Arc::clone(&resolved_event_bus),
            resolved_runtime.clone(),
            cache_token.as_ref(),
        )
        .await;
        if let Some(error) = err {
            errors.push(LoadExtensionErrorEntry {
                path: ext_path.clone(),
                error,
            });
            continue;
        }
        if let Some(extension) = ext {
            extensions.push(extension);
        }
    }

    LoadExtensionsResult {
        extensions,
        errors,
        warnings,
        runtime: resolved_runtime,
    }
}

pub async fn load_extensions(
    paths: &[String],
    cwd: &str,
    event_bus: Option<Arc<dyn EventBus>>,
    runtime: Option<ExtensionRuntime>,
) -> LoadExtensionsResult {
    load_extensions_internal(paths, cwd, event_bus, runtime, false).await
}

pub async fn load_extensions_cached(
    paths: &[String],
    cwd: &str,
    event_bus: Option<Arc<dyn EventBus>>,
    runtime: Option<ExtensionRuntime>,
) -> LoadExtensionsResult {
    load_extensions_internal(paths, cwd, event_bus, runtime, true).await
}

fn is_extension_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".ts")
        || lower.ends_with(".js")
        || lower.ends_with(".mts")
        || lower.ends_with(".mjs")
        || lower.ends_with(".cts")
        || lower.ends_with(".cjs")
}

fn resolve_extension_entries(dir: &str) -> Option<Vec<String>> {
    let pkg_json = pi_js::path::join(&[dir, "package.json"]);
    if pi_js::fs::exists(&pkg_json) {
        if let Some(manifest) = read_pi_manifest_wire(&pkg_json) {
            if !manifest.extensions.is_empty() {
                let mut entries = Vec::new();
                for ext_path in manifest.extensions {
                    let resolved = pi_js::path::resolve(&[dir, &ext_path]);
                    if pi_js::fs::exists(&resolved) {
                        entries.push(resolved);
                    }
                }
                if !entries.is_empty() {
                    return Some(entries);
                }
            }
        }
    }

    for candidate in &["index.ts", "index.mts", "index.js", "index.mjs"] {
        let p = pi_js::path::join(&[dir, candidate]);
        if pi_js::fs::exists(&p) {
            return Some(vec![p]);
        }
    }
    None
}

fn discover_extensions_in_dir(dir: &str) -> Vec<String> {
    if !pi_js::fs::exists(dir) {
        return Vec::new();
    }
    let Ok(entries) = pi_js::fs::readdir_with_file_types(dir) else {
        return Vec::new();
    };
    let mut discovered = Vec::new();
    for entry in entries {
        let entry_path = pi_js::path::join(&[dir, &entry.name]);
        if (entry.is_file() || entry.is_symbolic_link()) && is_extension_file(&entry.name) {
            discovered.push(entry_path);
            continue;
        }
        if entry.is_directory() || entry.is_symbolic_link() {
            if let Some(sub_entries) = resolve_extension_entries(&entry_path) {
                discovered.extend(sub_entries);
            }
        }
    }
    discovered
}

pub async fn discover_and_load_extensions(
    configured_paths: &[String],
    cwd: &str,
    agent_dir: Option<&str>,
    event_bus: Option<Arc<dyn EventBus>>,
) -> LoadExtensionsResult {
    let resolved_cwd = resolve_path_wire(cwd, &pi_js::env::cwd());
    let agent_dir_str = agent_dir.map(|s| s.to_string()).unwrap_or_else(get_agent_dir_wire);
    let resolved_agent_dir = resolve_path_wire(&agent_dir_str, &pi_js::env::cwd());

    let mut all_paths: Vec<String> = Vec::new();
    let mut seen: IndexSet<String> = IndexSet::new();

    let mut add_paths = |paths: Vec<String>| {
        for p in paths {
            let resolved = pi_js::path::resolve(&[&p]);
            if seen.insert(resolved) {
                all_paths.push(p);
            }
        }
    };

    // 1. Project-local extensions: cwd/${CONFIG_DIR_NAME}/extensions/
    let local_ext_dir = pi_js::path::join(&[&resolved_cwd, CONFIG_DIR_NAME_WIRE, "extensions"]);
    add_paths(discover_extensions_in_dir(&local_ext_dir));

    // 2. Global extensions: agentDir/extensions/
    let global_ext_dir = pi_js::path::join(&[&resolved_agent_dir, "extensions"]);
    add_paths(discover_extensions_in_dir(&global_ext_dir));

    // 3. Explicitly configured paths
    for p in configured_paths {
        let resolved = resolve_path_wire(p, &resolved_cwd);
        if let Ok(st) = pi_js::fs::stat(&resolved) {
            if st.is_directory() {
                if let Some(entries) = resolve_extension_entries(&resolved) {
                    add_paths(entries);
                } else {
                    add_paths(discover_extensions_in_dir(&resolved));
                }
                continue;
            }
        }
        add_paths(vec![resolved]);
    }

    load_extensions(&all_paths, &resolved_cwd, event_bus, None).await
}

const EXTENSION_BRIDGE_JS: &str = r#"
(function () {
    const E = globalThis.__pi_ext;

    globalThis.__pi_build_signal = function(sigId) {
        return {
            get aborted() { return E.isSignalAborted(sigId); },
            addEventListener() {},
            removeEventListener() {},
        };
    };

    globalThis.__pi_build_ctx = function(ctxId, kind) {
        const ui = {
            select: (title, options, opts) => E.ctxCall(ctxId, "ui_select", title, options, opts),
            confirm: (title, message, opts) => E.ctxCall(ctxId, "ui_confirm", title, message, opts),
            input: (title, placeholder, opts) => E.ctxCall(ctxId, "ui_input", title, placeholder, opts),
            editor: (title, prefill) => E.ctxCall(ctxId, "ui_editor", title, prefill),
            notify: (msg, type) => E.ctxCall(ctxId, "ui_notify", msg, type),
            onTerminalInput: () => () => {},
            setStatus: (key, text) => E.ctxCall(ctxId, "ui_setStatus", key, text),
            setWorkingMessage: (msg) => E.ctxCall(ctxId, "ui_setWorkingMessage", msg),
            setWorkingVisible: (vis) => E.ctxCall(ctxId, "ui_setWorkingVisible", Boolean(vis)),
            setWorkingIndicator: () => {},
            setHiddenThinkingLabel: (lbl) => E.ctxCall(ctxId, "ui_setHiddenThinkingLabel", lbl),
            setWidget: (key, content, opts) => E.ctxCall(ctxId, "ui_setWidget", key, content, opts),
            setFooter: () => {},
            setHeader: () => {},
            setTitle: (title) => E.ctxCall(ctxId, "ui_setTitle", title),
            custom: async () => undefined,
            pasteToEditor: (text) => E.ctxCall(ctxId, "ui_pasteToEditor", text),
            setEditorText: (text) => E.ctxCall(ctxId, "ui_setEditorText", text),
            getEditorText: () => E.ctxCall(ctxId, "ui_getEditorText"),
            addAutocompleteProvider: () => {},
            setEditorComponent: () => {},
            getEditorComponent: () => undefined,
            getAllThemes: () => E.ctxCall(ctxId, "ui_getAllThemes"),
            getTheme: () => undefined,
            setTheme: (t) => E.ctxCall(ctxId, "ui_setTheme", typeof t === "string" ? t : ""),
            getToolsExpanded: () => E.ctxCall(ctxId, "ui_getToolsExpanded"),
            setToolsExpanded: (e) => E.ctxCall(ctxId, "ui_setToolsExpanded", Boolean(e)),
            theme: {
                fg: (c, t) => E.ctxCall(ctxId, "ui_themeCall", "fg", String(c), String(t)),
                bg: (c, t) => E.ctxCall(ctxId, "ui_themeCall", "bg", String(c), String(t)),
                bold: (t) => E.ctxCall(ctxId, "ui_themeCall", "bold", String(t)),
                italic: (t) => E.ctxCall(ctxId, "ui_themeCall", "italic", String(t)),
                underline: (t) => E.ctxCall(ctxId, "ui_themeCall", "underline", String(t)),
                inverse: (t) => E.ctxCall(ctxId, "ui_themeCall", "inverse", String(t)),
                strikethrough: (t) => E.ctxCall(ctxId, "ui_themeCall", "strikethrough", String(t)),
                getFgAnsi: (c) => E.ctxCall(ctxId, "ui_themeCall", "getFgAnsi", String(c)),
                getBgAnsi: (c) => E.ctxCall(ctxId, "ui_themeCall", "getBgAnsi", String(c)),
            },
        };

        const sessionManager = {
            getCwd: () => E.ctxCall(ctxId, "sm_getCwd"),
            getSessionDir: () => E.ctxCall(ctxId, "sm_getSessionDir"),
            getSessionId: () => E.ctxCall(ctxId, "sm_getSessionId"),
            getSessionFile: () => E.ctxCall(ctxId, "sm_getSessionFile"),
            getLeafId: () => E.ctxCall(ctxId, "sm_getLeafId"),
            getEntry: (id) => E.ctxCall(ctxId, "sm_getEntry", id),
            getLabel: (id) => E.ctxCall(ctxId, "sm_getLabel", id),
            getBranch: (fromId) => E.ctxCall(ctxId, "sm_getBranch", fromId),
            getEntries: () => E.ctxCall(ctxId, "sm_getEntries"),
            getSessionName: () => E.ctxCall(ctxId, "sm_getSessionName"),
        };

        const modelRegistry = {
            getAvailable: () => E.ctxCall(ctxId, "mr_getAvailable"),
            getApiKey: (model) => E.ctxCall(ctxId, "mr_getApiKey", model),
        };

        const ctx = {
            get ui() { return ui; },
            get mode() { return E.ctxCall(ctxId, "mode"); },
            get hasUI() { return E.ctxCall(ctxId, "hasUI"); },
            get cwd() { return E.ctxCall(ctxId, "cwd"); },
            get sessionManager() { return sessionManager; },
            get modelRegistry() { return modelRegistry; },
            get model() { return E.ctxCall(ctxId, "model"); },
            get scopedModels() { return E.ctxCall(ctxId, "scopedModels"); },
            get thinkingLevel() { return E.ctxCall(ctxId, "thinkingLevel"); },
            get signal() { return E.ctxCall(ctxId, "signal"); },
            isIdle: () => E.ctxCall(ctxId, "isIdle"),
            isProjectTrusted: () => E.ctxCall(ctxId, "isProjectTrusted"),
            abort: () => E.ctxCall(ctxId, "abort"),
            hasPendingMessages: () => E.ctxCall(ctxId, "hasPendingMessages"),
            shutdown: () => E.ctxCall(ctxId, "shutdown"),
            getContextUsage: () => E.ctxCall(ctxId, "getContextUsage"),
            compact: (opts) => E.ctxCall(ctxId, "compact", opts),
            getSystemPrompt: () => E.ctxCall(ctxId, "getSystemPrompt"),
        };

        if (kind === "tool") {
            Object.defineProperty(ctx, "tools", { get: () => E.ctxCall(ctxId, "tool_tools"), enumerable: true });
            ctx.executeTool = (name, args, opts) => E.ctxCall(ctxId, "tool_executeTool", String(name), args, opts);
        }
        if (kind === "cmd") {
            ctx.getSystemPromptOptions = () => E.ctxCall(ctxId, "cmd_getSystemPromptOptions");
            ctx.waitForIdle = () => E.ctxCall(ctxId, "cmd_waitForIdle");
            ctx.newSession = (opts) => E.ctxCall(ctxId, "cmd_newSession", opts);
            ctx.fork = (id, opts) => E.ctxCall(ctxId, "cmd_fork", id, opts);
            ctx.navigateTree = (id, opts) => E.ctxCall(ctxId, "cmd_navigateTree", id, opts);
            ctx.switchSession = (path, opts) => E.ctxCall(ctxId, "cmd_switchSession", path, opts);
            ctx.reload = () => E.ctxCall(ctxId, "cmd_reload");
        }
        return ctx;
    };

    globalThis.__pi_build_api = function(apiId) {
        return {
            on(eventName, handler) {
                const unsubId = E.apiCall(apiId, "on", String(eventName), handler);
                return () => E.unsubscribe(unsubId);
            },
            registerTool(toolDef) { E.apiCall(apiId, "registerTool", toolDef); },
            registerCommand(name, opts) { E.apiCall(apiId, "registerCommand", name, opts); },
            registerShortcut(shortcut, opts) { E.apiCall(apiId, "registerShortcut", String(shortcut), opts); },
            registerFlag(name, opts) { E.apiCall(apiId, "registerFlag", String(name), opts || {}); },
            getFlag(name) { return E.apiCall(apiId, "getFlag", String(name)); },
            registerMessageRenderer(customType, renderer) {
                E.apiCall(apiId, "registerMessageRenderer", String(customType), renderer);
            },
            registerMarkdownTransformer(transformer) {
                E.apiCall(apiId, "registerMarkdownTransformer", transformer);
            },
            registerEntryRenderer(customType, renderer) {
                E.apiCall(apiId, "registerEntryRenderer", String(customType), renderer);
            },
            sendMessage(msg, opts) { E.apiCall(apiId, "sendMessage", msg, opts); },
            sendUserMessage(content, opts) { E.apiCall(apiId, "sendUserMessage", content, opts); },
            appendEntry(customType, data) { E.apiCall(apiId, "appendEntry", String(customType), data); },
            setSessionName(name) { E.apiCall(apiId, "setSessionName", String(name)); },
            getSessionName() { return E.apiCall(apiId, "getSessionName"); },
            setLabel(entryId, label) { E.apiCall(apiId, "setLabel", String(entryId), label); },
            exec(command, args, opts) {
                return E.apiCall(apiId, "exec", { command: String(command), args: args || [], options: opts || {} });
            },
            getActiveTools() { return E.apiCall(apiId, "getActiveTools"); },
            getAllTools() { return E.apiCall(apiId, "getAllTools"); },
            getSettings() { return E.apiCall(apiId, "getSettings"); },
            setActiveTools(toolNames) { E.apiCall(apiId, "setActiveTools", toolNames || []); },
            getCommands() { return E.apiCall(apiId, "getCommands"); },
            setModel(model) { return E.apiCall(apiId, "setModel", model); },
            getThinkingLevel() { return E.apiCall(apiId, "getThinkingLevel"); },
            setThinkingLevel(level) { E.apiCall(apiId, "setThinkingLevel", level); },
            registerProvider(nameOrObj, config) {
                if (typeof nameOrObj === "string") E.apiCall(apiId, "registerProvider", nameOrObj, config);
                else if (nameOrObj && nameOrObj.id) E.apiCall(apiId, "registerProvider", nameOrObj.id, nameOrObj);
            },
            unregisterProvider(name) { E.apiCall(apiId, "unregisterProvider", String(name)); },
            registerMcpServer(name, config) { E.apiCall(apiId, "registerMcpServer", String(name), config); },
            unregisterMcpServer(name) { E.apiCall(apiId, "unregisterMcpServer", String(name)); },
            getMcpServers() { return E.apiCall(apiId, "getMcpServers"); },
            events: {
                emit(channel, data) { E.apiCall(apiId, "eventsEmit", String(channel), data); },
                on(channel, handler) {
                    const unsubId = E.apiCall(apiId, "eventsOn", String(channel), handler);
                    return () => E.unsubscribe(unsubId);
                },
            },
        };
    };

    globalThis.__pi_invoke_event = async function(fn, ev, ctxId, kind) {
        const ctx = globalThis.__pi_build_ctx(ctxId, kind);
        if (ev && ev.type === "before_agent_start") {
            Object.defineProperty(ev, "systemPrompt", {
                get() { return ctx.getSystemPrompt(); },
                enumerable: true,
                configurable: true,
            });
        }
        try {
            let ret = await fn(ev, ctx);
            if (
                ev &&
                ev.type === "user_bash" &&
                ret !== undefined &&
                typeof ret === "object" &&
                ret !== null &&
                ret.operations !== undefined &&
                ret.result === undefined &&
                typeof ret.operations === "object" &&
                ret.operations !== null &&
                typeof ret.operations.exec === "function"
            ) {
                ret = { __piUserBashExecId: E.storeFn(ret.operations.exec) };
            }
            return {
                __piEnvelope: true,
                hasResult: ret !== undefined,
                result: ret === undefined ? null : ret,
                mutatedEvent: ev,
            };
        } catch (err) {
            return {
                __piEnvelope: true,
                hasResult: false,
                result: null,
                mutatedEvent: ev,
                threw: err instanceof Error ? err.message : String(err),
                stack: err instanceof Error ? err.stack : undefined,
            };
        }
    };

    globalThis.__pi_invoke_tool = async function(fn, toolCallId, params, sigId, updateId, ctxId) {
        const signal = sigId !== null ? globalThis.__pi_build_signal(sigId) : undefined;
        const onUpdate = updateId !== null ? (partial) => E.onUpdate(updateId, partial) : undefined;
        const ctx = globalThis.__pi_build_ctx(ctxId, "tool");
        return await fn(toolCallId, params, signal, onUpdate, ctx);
    };

    globalThis.__pi_invoke_cmd = async function(fn, args, ctxId) {
        const ctx = globalThis.__pi_build_ctx(ctxId, "cmd");
        return await fn(args, ctx);
    };

    globalThis.__pi_render_text = function(fn, ...args) {
        const comp = fn(...args, {}, { args: args[0], state: {} });
        if (!comp) return "";
        if (typeof comp.render === "function") {
            const lines = comp.render(80);
            return Array.isArray(lines) ? lines.join("\n") : String(lines ?? "");
        }
        return String(comp);
    };
})();
"#;

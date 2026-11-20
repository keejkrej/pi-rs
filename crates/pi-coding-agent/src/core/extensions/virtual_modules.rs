//! Port of packages/coding-agent/src/core/extensions/virtual-modules.ts
//! Provides virtual QuickJS ES modules and host-backed Web/Node globals.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_agent_core::types::AgentToolCallOutcome;
use pi_codemode::{JSDeferred, JSValueHandle, QuickJS};
use pi_js::BoxFuture;
use pi_js::vendor::typebox::{Schema, compile, t, value as tb_value};
use serde_json::Value;

use crate::config::get_agent_dir;
use crate::core::exec::{ExecOptions, ExecResult, exec_command};
use crate::core::tools::truncate::{
    TruncationOptions, format_size, truncate_head, truncate_line, truncate_middle, truncate_tail,
};

pub(crate) enum HostJsResult {
    Undefined,
    Json(Value),
    Bytes(Vec<u8>),
    ExecResult(ExecResult),
    FetchResponse {
        status: u16,
        status_text: String,
        ok: bool,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    AgentToolCallOutcome(AgentToolCallOutcome),
}

pub(crate) struct PendingHostTask {
    pub deferred: JSDeferred,
    pub fut: BoxFuture<pi_js::Result<HostJsResult>>,
}

#[derive(Clone, Default)]
pub(crate) struct VmHostBridge {
    pub pending_async: Arc<Mutex<Vec<PendingHostTask>>>,
    pub cancelled_timers: Arc<Mutex<IndexMap<u64, Arc<AtomicBool>>>>,
    pub next_timer_id: Arc<AtomicU64>,
}

impl VmHostBridge {
    pub fn push_async(
        &self,
        vm: &QuickJS,
        fut: BoxFuture<pi_js::Result<HostJsResult>>,
    ) -> pi_js::Result<JSValueHandle> {
        let deferred = vm.new_promise()?;
        let handle = deferred.handle.dup()?;
        self.pending_async
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(PendingHostTask { deferred, fut });
        Ok(handle)
    }

    pub fn drain_pending(&self) -> Vec<PendingHostTask> {
        let mut guard = self.pending_async.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    }
}

pub fn resolve_virtual_module(specifier: &str) -> Option<&'static str> {
    match specifier {
        "typebox" | "@sinclair/typebox" => Some(MOD_TYPEBOX),
        "typebox/value" | "@sinclair/typebox/value" => Some(MOD_TYPEBOX_VALUE),
        "typebox/compile" | "@sinclair/typebox/compile" => Some(MOD_TYPEBOX_COMPILE),
        "@earendil-works/pi-coding-agent" | "@mariozechner/pi-coding-agent" => Some(MOD_PI_CODING_AGENT),
        "@earendil-works/pi-ai"
        | "@mariozechner/pi-ai"
        | "@earendil-works/pi-ai/compat"
        | "@mariozechner/pi-ai/compat" => Some(MOD_PI_AI),
        "@earendil-works/pi-ai/models" | "@mariozechner/pi-ai/models" => Some(MOD_PI_AI_MODELS),
        "@earendil-works/pi-ai/oauth" | "@mariozechner/pi-ai/oauth" => Some(MOD_PI_AI_OAUTH),
        "@earendil-works/pi-ai/providers/all" | "@mariozechner/pi-ai/providers/all" => Some(MOD_PI_AI_PROVIDERS),
        "@earendil-works/pi-tui" | "@mariozechner/pi-tui" => Some(MOD_PI_TUI),
        "@earendil-works/pi-agent-core" | "@mariozechner/pi-agent-core" => Some(MOD_PI_AGENT_CORE),
        "node:path" | "path" => Some(MOD_NODE_PATH),
        "node:os" | "os" => Some(MOD_NODE_OS),
        "node:fs" | "fs" => Some(MOD_NODE_FS),
        "node:fs/promises" | "fs/promises" => Some(MOD_NODE_FS_PROMISES),
        "node:events" | "events" => Some(MOD_NODE_EVENTS),
        "node:url" | "url" => Some(MOD_NODE_URL),
        "node:crypto" | "crypto" => Some(MOD_NODE_CRYPTO),
        "node:module" | "module" => Some(MOD_NODE_MODULE),
        "node:child_process" | "child_process" => Some(MOD_NODE_CHILD_PROCESS),
        _ => None,
    }
}

pub fn is_virtual_module(specifier: &str) -> bool {
    resolve_virtual_module(specifier).is_some()
}

pub(crate) fn install_host_globals(
    vm: &QuickJS,
    bridge: &VmHostBridge,
    require_resolver: Arc<dyn Fn(&str, &str) -> pi_js::Result<String> + Send + Sync>,
) -> pi_js::Result<()> {
    vm.with_scope(|_scope| {
        let host = vm.new_object()?;

        // console(level, msg)
        let vm_c = vm.clone();
        let console_fn = vm.new_function(
            "__pi_console",
            Arc::new(move |_this, args| {
                let level = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let msg = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                match level.as_str() {
                    "warn" => pi_js::console::warn(&msg),
                    "error" => pi_js::console::error(&msg),
                    _ => pi_js::console::log(&msg),
                }
                vm_c.undefined()
            }),
        )?;
        host.set_prop("console", &console_fn)?;
        console_fn.dispose();

        // env(op, k, v)
        let vm_c = vm.clone();
        let env_fn = vm.new_function(
            "__pi_env",
            Arc::new(move |_this, args| {
                let op = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                match op.as_str() {
                    "get" => {
                        let k = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                        match pi_js::env::var(&k) {
                            Some(v) => vm_c.new_string(&v),
                            None => vm_c.undefined(),
                        }
                    }
                    "set" => {
                        let k = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                        let v = args.get(2).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                        pi_js::env::set_var(&k, &v);
                        vm_c.undefined()
                    }
                    "del" => {
                        let k = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                        pi_js::env::remove_var(&k);
                        vm_c.undefined()
                    }
                    "keys" => {
                        let keys: Vec<Value> = pi_js::env::vars().into_keys().map(Value::String).collect();
                        vm_c.from_json_value(&Value::Array(keys))
                    }
                    "cwd" => vm_c.new_string(&pi_js::env::cwd()),
                    "chdir" => {
                        let p = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                        pi_js::env::chdir(&p)?;
                        vm_c.undefined()
                    }
                    "home" => vm_c.new_string(&pi_js::env::home_dir()),
                    "tmp" => vm_c.new_string(&pi_js::env::tmp_dir()),
                    "platform" => vm_c.new_string(pi_js::env::platform()),
                    "arch" => vm_c.new_string(pi_js::env::arch()),
                    "release" => vm_c.new_string(&pi_js::env::os_release()),
                    "pid" => vm_c.new_number(pi_js::env::pid() as f64),
                    "agentDir" => vm_c.new_string(&get_agent_dir()),
                    _ => vm_c.undefined(),
                }
            }),
        )?;
        host.set_prop("env", &env_fn)?;
        env_fn.dispose();

        // path(op, argsJson)
        let vm_c = vm.clone();
        let path_fn = vm.new_function(
            "__pi_path",
            Arc::new(move |_this, args| {
                let op = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let a_val = args
                    .get(1)
                    .map(|h| vm_c.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let arr = a_val.as_array().cloned().unwrap_or_default();
                let as_str = |idx: usize| -> String { arr.get(idx).and_then(|v| v.as_str()).unwrap_or("").to_string() };
                let parts: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
                match op.as_str() {
                    "join" => vm_c.new_string(&pi_js::path::join(&refs)),
                    "resolve" => vm_c.new_string(&pi_js::path::resolve(&refs)),
                    "normalize" => vm_c.new_string(&pi_js::path::normalize(&as_str(0))),
                    "relative" => vm_c.new_string(&pi_js::path::relative(&as_str(0), &as_str(1))),
                    "dirname" => vm_c.new_string(&pi_js::path::dirname(&as_str(0))),
                    "basename" => {
                        let ext = arr.get(1).and_then(|v| v.as_str());
                        vm_c.new_string(&pi_js::path::basename(&as_str(0), ext))
                    }
                    "extname" => vm_c.new_string(&pi_js::path::extname(&as_str(0))),
                    "isAbsolute" => vm_c.new_boolean(pi_js::path::is_absolute(&as_str(0))),
                    "sep" => vm_c.new_string(pi_js::path::sep()),
                    "delimiter" => vm_c.new_string(pi_js::path::delimiter()),
                    "posix_join" => vm_c.new_string(&pi_js::path::posix::join(&refs)),
                    "posix_resolve" => vm_c.new_string(&pi_js::path::posix::resolve(&refs)),
                    "posix_dirname" => vm_c.new_string(&pi_js::path::posix::dirname(&as_str(0))),
                    "posix_basename" => {
                        let ext = arr.get(1).and_then(|v| v.as_str());
                        vm_c.new_string(&pi_js::path::posix::basename(&as_str(0), ext))
                    }
                    "posix_extname" => vm_c.new_string(&pi_js::path::posix::extname(&as_str(0))),
                    "posix_normalize" => vm_c.new_string(&pi_js::path::posix::normalize(&as_str(0))),
                    "posix_relative" => vm_c.new_string(&pi_js::path::posix::relative(&as_str(0), &as_str(1))),
                    "posix_isAbsolute" => vm_c.new_boolean(pi_js::path::posix::is_absolute(&as_str(0))),
                    _ => vm_c.undefined(),
                }
            }),
        )?;
        host.set_prop("path", &path_fn)?;
        path_fn.dispose();

        // fs_sync(op, argsJson)
        let vm_c = vm.clone();
        let fs_sync_fn = vm.new_function(
            "__pi_fs_sync",
            Arc::new(move |_this, args| {
                let op = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let a = args
                    .get(1)
                    .map(|h| vm_c.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                exec_fs_sync(&vm_c, &op, &a)
            }),
        )?;
        host.set_prop("fsSync", &fs_sync_fn)?;
        fs_sync_fn.dispose();

        // fs_async(op, argsJson) -> Promise
        let vm_c = vm.clone();
        let bridge_c = bridge.clone();
        let fs_async_fn = vm.new_function(
            "__pi_fs_async",
            Arc::new(move |_this, args| {
                let op = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let a = args
                    .get(1)
                    .map(|h| vm_c.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move { exec_fs_async(&op, &a).await });
                bridge_c.push_async(&vm_c, fut)
            }),
        )?;
        host.set_prop("fsAsync", &fs_async_fn)?;
        fs_async_fn.dispose();

        // crypto(op, arg)
        let vm_c = vm.clone();
        let crypto_fn = vm.new_function(
            "__pi_crypto",
            Arc::new(move |_this, args| {
                let op = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                match op.as_str() {
                    "uuid" => vm_c.new_string(&pi_js::crypto::random_uuid()),
                    "randomBytes" => {
                        let n = args.get(1).map(|h| h.to_number()).transpose()?.unwrap_or(0.0).max(0.0) as usize;
                        let bytes = pi_js::crypto::random_bytes(n);
                        vm_c.new_uint8_array(&bytes)
                    }
                    "sha256_hex" => {
                        let data = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                        vm_c.new_string(&pi_js::crypto::sha256_hex(data.as_bytes()))
                    }
                    "sha256_bytes" => {
                        let data = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                        let digest = pi_js::crypto::sha256(data.as_bytes());
                        vm_c.new_uint8_array(&digest)
                    }
                    _ => vm_c.undefined(),
                }
            }),
        )?;
        host.set_prop("crypto", &crypto_fn)?;
        crypto_fn.dispose();

        // b64(op, arg)
        let vm_c = vm.clone();
        let b64_fn = vm.new_function(
            "__pi_b64",
            Arc::new(move |_this, args| {
                let op = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                match op.as_str() {
                    "encode" => {
                        let bytes = args.get(1).map(|h| h.to_bytes()).transpose()?.unwrap_or_default();
                        vm_c.new_string(&pi_js::b64::encode(&bytes))
                    }
                    "encodeUrl" => {
                        let bytes = args.get(1).map(|h| h.to_bytes()).transpose()?.unwrap_or_default();
                        vm_c.new_string(&pi_js::b64::encode_url(&bytes))
                    }
                    "decode" => {
                        let s = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                        vm_c.new_uint8_array(&pi_js::b64::decode(&s))
                    }
                    "decodeUrl" => {
                        let s = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                        vm_c.new_uint8_array(&pi_js::b64::decode_url(&s))
                    }
                    "utf8Encode" => {
                        let s = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                        vm_c.new_uint8_array(s.as_bytes())
                    }
                    "utf8Decode" => {
                        let bytes = args.get(1).map(|h| h.to_bytes()).transpose()?.unwrap_or_default();
                        let s = String::from_utf8_lossy(&bytes).into_owned();
                        vm_c.new_string(&s)
                    }
                    _ => vm_c.undefined(),
                }
            }),
        )?;
        host.set_prop("b64", &b64_fn)?;
        b64_fn.dispose();

        // truncate(op, payload)
        let vm_c = vm.clone();
        let truncate_fn = vm.new_function(
            "__pi_truncate",
            Arc::new(move |_this, args| {
                let op = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let val = args
                    .get(1)
                    .map(|h| vm_c.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                match op.as_str() {
                    "formatSize" => {
                        let bytes = val.as_u64().unwrap_or(0) as usize;
                        vm_c.new_string(&format_size(bytes))
                    }
                    "head" | "tail" => {
                        let content = val.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        let opts = TruncationOptions {
                            max_lines: val.get("maxLines").and_then(|v| v.as_u64()).map(|n| n as usize),
                            max_bytes: val.get("maxBytes").and_then(|v| v.as_u64()).map(|n| n as usize),
                        };
                        let res = if op == "head" {
                            truncate_head(content, Some(opts))
                        } else {
                            truncate_tail(content, Some(opts))
                        };
                        let json_res = serde_json::to_value(res).unwrap_or(Value::Null);
                        vm_c.from_json_value(&json_res)
                    }
                    "line" => {
                        let line = val.get("line").and_then(|v| v.as_str()).unwrap_or("");
                        let max_chars = val.get("maxChars").and_then(|v| v.as_u64()).map(|n| n as usize);
                        let res = truncate_line(line, max_chars);
                        vm_c.from_json_value(&serde_json::json!({
                            "text": res.text,
                            "wasTruncated": res.was_truncated,
                        }))
                    }
                    "middle" => {
                        let content = val.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        let max_bytes = val.get("maxBytes").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        let res = truncate_middle(content, max_bytes);
                        vm_c.from_json_value(&serde_json::json!({
                            "content": res.content,
                            "truncated": res.truncated,
                            "removedChars": res.removed_chars,
                            "totalBytes": res.total_bytes,
                            "totalLines": res.total_lines,
                        }))
                    }
                    _ => vm_c.undefined(),
                }
            }),
        )?;
        host.set_prop("truncate", &truncate_fn)?;
        truncate_fn.dispose();

        // typebox(op, schemaJson, valueJson)
        let vm_c = vm.clone();
        let tb_fn = vm.new_function(
            "__pi_typebox",
            Arc::new(move |_this, args| {
                let op = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let schema_val = args
                    .get(1)
                    .map(|h| vm_c.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let data_val = args
                    .get(2)
                    .map(|h| vm_c.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let schema: Schema = serde_json::from_value(schema_val).unwrap_or_else(|_| t::any());
                match op.as_str() {
                    "check" => {
                        let ok = compile(&schema).check(&data_val);
                        vm_c.new_boolean(ok)
                    }
                    "errors" => {
                        let errs: Vec<Value> = compile(&schema)
                            .errors(&data_val)
                            .into_iter()
                            .map(|e| {
                                serde_json::json!({
                                    "instancePath": e.instance_path,
                                    "schemaPath": e.schema_path,
                                    "message": e.message,
                                    "params": e.params,
                                })
                            })
                            .collect();
                        vm_c.from_json_value(&Value::Array(errs))
                    }
                    "convert" => vm_c.from_json_value(&tb_value::convert(&schema, data_val)),
                    "clean" => vm_c.from_json_value(&tb_value::clean(&schema, data_val)),
                    "default" => vm_c.from_json_value(&tb_value::default(&schema, data_val)),
                    _ => vm_c.undefined(),
                }
            }),
        )?;
        host.set_prop("typebox", &tb_fn)?;
        tb_fn.dispose();

        // fetch(url, initJson) -> Promise<FetchResponse>
        let vm_c = vm.clone();
        let bridge_c = bridge.clone();
        let fetch_fn = vm.new_function(
            "__pi_fetch",
            Arc::new(move |_this, args| {
                let url = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let init = args
                    .get(1)
                    .map(|h| vm_c.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let method = init.get("method").and_then(|v| v.as_str()).unwrap_or("GET").to_string();
                let mut headers = pi_js::fetch::Headers::new();
                if let Some(hdrs) = init.get("headers").and_then(|v| v.as_object()) {
                    for (k, v) in hdrs {
                        if let Some(s) = v.as_str() {
                            headers.set(k, s);
                        }
                    }
                }
                let body = init
                    .get("body")
                    .and_then(|v| v.as_str())
                    .map(|s| pi_js::fetch::Body::Text(s.to_string()));
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    let req = pi_js::fetch::Request {
                        method,
                        url,
                        headers,
                        body,
                        signal: None,
                    };
                    let resp = pi_js::fetch::fetch(req).await?;
                    let status = resp.status();
                    let status_text = resp.status_text().to_string();
                    let ok = resp.ok();
                    let resp_url = resp.url().to_string();
                    let resp_headers = resp.headers().entries();
                    let body_bytes = resp.bytes().await?;
                    Ok(HostJsResult::FetchResponse {
                        status,
                        status_text,
                        ok,
                        url: resp_url,
                        headers: resp_headers,
                        body: body_bytes.to_vec(),
                    })
                });
                bridge_c.push_async(&vm_c, fut)
            }),
        )?;
        host.set_prop("fetch", &fetch_fn)?;
        fetch_fn.dispose();

        // sleep(ms, timerId) -> Promise
        let vm_c = vm.clone();
        let bridge_c = bridge.clone();
        let sleep_fn = vm.new_function(
            "__pi_sleep",
            Arc::new(move |_this, args| {
                let ms = args.first().map(|h| h.to_number()).transpose()?.unwrap_or(0.0).max(0.0) as u64;
                let id = bridge_c.next_timer_id.fetch_add(1, Ordering::Relaxed) + 1;
                let cancelled = Arc::new(AtomicBool::new(false));
                bridge_c
                    .cancelled_timers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id, Arc::clone(&cancelled));
                let timers = Arc::clone(&bridge_c.cancelled_timers);
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    timers.lock().unwrap_or_else(|e| e.into_inner()).shift_remove(&id);
                    Ok(HostJsResult::Json(serde_json::json!({
                        "cancelled": cancelled.load(Ordering::Acquire),
                    })))
                });
                let promise = bridge_c.push_async(&vm_c, fut)?;
                let obj = vm_c.new_object()?;
                let id_h = vm_c.new_number(id as f64)?;
                obj.set_prop("id", &id_h)?;
                obj.set_prop("promise", &promise)?;
                id_h.dispose();
                promise.dispose();
                Ok(obj)
            }),
        )?;
        host.set_prop("sleep", &sleep_fn)?;
        sleep_fn.dispose();

        let vm_c = vm.clone();
        let bridge_c = bridge.clone();
        let clear_timer_fn = vm.new_function(
            "__pi_clear_timer",
            Arc::new(move |_this, args| {
                let id = args.first().map(|h| h.to_number()).transpose()?.unwrap_or(0.0) as u64;
                if let Some(flag) = bridge_c
                    .cancelled_timers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .shift_remove(&id)
                {
                    flag.store(true, Ordering::Release);
                }
                vm_c.undefined()
            }),
        )?;
        host.set_prop("clearTimer", &clear_timer_fn)?;
        clear_timer_fn.dispose();

        // execAsync(cmd, args, opts) -> Promise<ExecResult>
        let vm_c = vm.clone();
        let bridge_c = bridge.clone();
        let exec_async_fn = vm.new_function(
            "__pi_exec_async",
            Arc::new(move |_this, args| {
                let cmd = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let argv_val = args
                    .get(1)
                    .map(|h| vm_c.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let argv: Vec<String> = argv_val
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let opts_val = args
                    .get(2)
                    .map(|h| vm_c.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let cwd = opts_val
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(pi_js::env::cwd);
                let timeout = opts_val.get("timeout").and_then(|v| v.as_u64());
                let max_buffer = opts_val
                    .get("maxBuffer")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| usize::try_from(v).ok());
                let fut: BoxFuture<pi_js::Result<HostJsResult>> = Box::pin(async move {
                    let res = exec_command(
                        &cmd,
                        &argv,
                        &cwd,
                        Some(ExecOptions {
                            signal: None,
                            timeout,
                            cwd: Some(cwd.clone()),
                            max_buffer,
                        }),
                    )
                    .await;
                    Ok(HostJsResult::ExecResult(res))
                });
                bridge_c.push_async(&vm_c, fut)
            }),
        )?;
        host.set_prop("execAsync", &exec_async_fn)?;
        exec_async_fn.dispose();

        // execSync(cmd, opts)
        let vm_c = vm.clone();
        let exec_sync_fn = vm.new_function(
            "__pi_exec_sync",
            Arc::new(move |_this, args| {
                let cmd = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let opts = args
                    .get(1)
                    .map(|h| vm_c.to_json_value(h))
                    .transpose()?
                    .unwrap_or(Value::Null);
                let cwd = opts
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(pi_js::env::cwd);
                let shell = if cfg!(windows) { "cmd" } else { "sh" };
                let flag = if cfg!(windows) { "/C" } else { "-c" };
                let output = std::process::Command::new(shell)
                    .arg(flag)
                    .arg(&cmd)
                    .current_dir(&cwd)
                    .output()
                    .map_err(|e| pi_js::Error::from_io(e, "spawn", Some(&cmd), None))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(pi_js::Error::msg(format!("Command failed: {cmd}: {stderr}")));
                }
                vm_c.new_string(&String::from_utf8_lossy(&output.stdout))
            }),
        )?;
        host.set_prop("execSync", &exec_sync_fn)?;
        exec_sync_fn.dispose();

        // require resolver fallback for CJS modules
        let vm_c = vm.clone();
        let req_fn = vm.new_function(
            "__pi_require_source",
            Arc::new(move |_this, args| {
                let from_dir = args.first().map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let spec = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                let js_src = require_resolver(&from_dir, &spec)?;
                vm_c.new_string(&js_src)
            }),
        )?;
        host.set_prop("requireSource", &req_fn)?;
        req_fn.dispose();

        let global = vm.global()?;
        global.set_prop("__pi_host", &host)?;
        host.dispose();
        global.dispose();

        let bootstrap = vm.eval_code(PRELUDE_JS, Some("<pi-prelude>"), None)?;
        bootstrap.dispose();
        Ok(())
    })
}

fn exec_fs_sync(vm: &QuickJS, op: &str, args: &Value) -> pi_js::Result<JSValueHandle> {
    let arr = args.as_array().cloned().unwrap_or_default();
    let p0 = arr.first().and_then(|v| v.as_str()).unwrap_or("");
    let p1 = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");
    match op {
        "existsSync" => vm.new_boolean(pi_js::fs::exists(p0)),
        "readFileSync" => {
            let encoding = arr
                .get(1)
                .and_then(|v| v.as_str().or_else(|| v.get("encoding").and_then(|e| e.as_str())));
            let bytes = pi_js::fs::read_file(p0)?;
            if encoding.is_some() {
                let s = String::from_utf8_lossy(&bytes).into_owned();
                vm.new_string(&s)
            } else {
                vm.new_uint8_array(&bytes)
            }
        }
        "writeFileSync" => {
            let data = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");
            pi_js::fs::write_file(p0, data.as_bytes(), None)?;
            vm.undefined()
        }
        "appendFileSync" => {
            let data = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");
            pi_js::fs::append_file(p0, data.as_bytes())?;
            vm.undefined()
        }
        "mkdirSync" => {
            let recursive = arr
                .get(1)
                .and_then(|v| v.get("recursive").and_then(|r| r.as_bool()))
                .unwrap_or(false);
            pi_js::fs::mkdir(p0, recursive, None)?;
            vm.undefined()
        }
        "readdirSync" => {
            let with_types = arr
                .get(1)
                .and_then(|v| v.get("withFileTypes").and_then(|b| b.as_bool()))
                .unwrap_or(false);
            if with_types {
                let entries = pi_js::fs::readdir_with_file_types(p0)?;
                let out: Vec<Value> = entries
                    .into_iter()
                    .map(|e| {
                        serde_json::json!({
                            "name": e.name,
                            "_isFile": e.is_file(),
                            "_isDirectory": e.is_directory(),
                            "_isSymbolicLink": e.is_symbolic_link(),
                        })
                    })
                    .collect();
                vm.from_json_value(&Value::Array(out))
            } else {
                let names: Vec<Value> = pi_js::fs::readdir(p0)?.into_iter().map(Value::String).collect();
                vm.from_json_value(&Value::Array(names))
            }
        }
        "statSync" => {
            let st = pi_js::fs::stat(p0)?;
            vm.from_json_value(&serde_json::json!({
                "_isFile": st.is_file(),
                "_isDirectory": st.is_directory(),
                "_isSymbolicLink": st.is_symbolic_link(),
                "size": st.size,
                "mtimeMs": st.mtime_ms,
                "mode": st.mode,
            }))
        }
        "lstatSync" => {
            let st = pi_js::fs::lstat(p0)?;
            vm.from_json_value(&serde_json::json!({
                "_isFile": st.is_file(),
                "_isDirectory": st.is_directory(),
                "_isSymbolicLink": st.is_symbolic_link(),
                "size": st.size,
                "mtimeMs": st.mtime_ms,
                "mode": st.mode,
            }))
        }
        "realpathSync" => vm.new_string(&pi_js::fs::realpath(p0)?),
        "unlinkSync" => {
            pi_js::fs::unlink(p0)?;
            vm.undefined()
        }
        "rmSync" => {
            let recursive = arr
                .get(1)
                .and_then(|v| v.get("recursive").and_then(|r| r.as_bool()))
                .unwrap_or(false);
            let force = arr
                .get(1)
                .and_then(|v| v.get("force").and_then(|f| f.as_bool()))
                .unwrap_or(false);
            pi_js::fs::rm(p0, recursive, force)?;
            vm.undefined()
        }
        "renameSync" => {
            pi_js::fs::rename(p0, p1)?;
            vm.undefined()
        }
        "copyFileSync" => {
            pi_js::fs::copy_file(p0, p1)?;
            vm.undefined()
        }
        "mkdtempSync" => vm.new_string(&pi_js::fs::mkdtemp(p0)?),
        "accessSync" => {
            pi_js::fs::stat(p0)?;
            vm.undefined()
        }
        "chmodSync" => {
            let mode = arr.get(1).and_then(|v| v.as_u64()).unwrap_or(0o644) as u32;
            pi_js::fs::chmod(p0, mode)?;
            vm.undefined()
        }
        _ => vm.undefined(),
    }
}

async fn exec_fs_async(op: &str, args: &Value) -> pi_js::Result<HostJsResult> {
    let arr = args.as_array().cloned().unwrap_or_default();
    let p0 = arr.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
    let p1 = arr.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match op {
        "readFile" => {
            let encoding = arr
                .get(1)
                .and_then(|v| v.as_str().or_else(|| v.get("encoding").and_then(|e| e.as_str())));
            let bytes = pi_js::fs::promises::read_file(&p0).await?;
            if encoding.is_some() {
                let s = String::from_utf8_lossy(&bytes).into_owned();
                Ok(HostJsResult::Json(Value::String(s)))
            } else {
                Ok(HostJsResult::Bytes(bytes))
            }
        }
        "writeFile" => {
            let data = arr.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
            pi_js::fs::promises::write_file(&p0, data.as_bytes(), None).await?;
            Ok(HostJsResult::Undefined)
        }
        "appendFile" => {
            let data = arr.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
            pi_js::fs::promises::append_file(&p0, data.as_bytes()).await?;
            Ok(HostJsResult::Undefined)
        }
        "mkdir" => {
            let recursive = arr
                .get(1)
                .and_then(|v| v.get("recursive").and_then(|r| r.as_bool()))
                .unwrap_or(false);
            pi_js::fs::promises::mkdir(&p0, recursive, None).await?;
            Ok(HostJsResult::Undefined)
        }
        "readdir" => {
            let with_types = arr
                .get(1)
                .and_then(|v| v.get("withFileTypes").and_then(|b| b.as_bool()))
                .unwrap_or(false);
            if with_types {
                let entries = pi_js::fs::promises::readdir_with_file_types(&p0).await?;
                let out: Vec<Value> = entries
                    .into_iter()
                    .map(|e| {
                        serde_json::json!({
                            "name": e.name,
                            "_isFile": e.is_file(),
                            "_isDirectory": e.is_directory(),
                            "_isSymbolicLink": e.is_symbolic_link(),
                        })
                    })
                    .collect();
                Ok(HostJsResult::Json(Value::Array(out)))
            } else {
                let names = pi_js::fs::promises::readdir(&p0).await?;
                Ok(HostJsResult::Json(Value::Array(
                    names.into_iter().map(Value::String).collect(),
                )))
            }
        }
        "stat" => {
            let st = pi_js::fs::promises::stat(&p0).await?;
            Ok(HostJsResult::Json(serde_json::json!({
                "_isFile": st.is_file(),
                "_isDirectory": st.is_directory(),
                "_isSymbolicLink": st.is_symbolic_link(),
                "size": st.size,
                "mtimeMs": st.mtime_ms,
                "mode": st.mode,
            })))
        }
        "lstat" => {
            let st = pi_js::fs::promises::lstat(&p0).await?;
            Ok(HostJsResult::Json(serde_json::json!({
                "_isFile": st.is_file(),
                "_isDirectory": st.is_directory(),
                "_isSymbolicLink": st.is_symbolic_link(),
                "size": st.size,
                "mtimeMs": st.mtime_ms,
                "mode": st.mode,
            })))
        }
        "realpath" => Ok(HostJsResult::Json(Value::String(
            pi_js::fs::promises::realpath(&p0).await?,
        ))),
        "unlink" => {
            pi_js::fs::promises::unlink(&p0).await?;
            Ok(HostJsResult::Undefined)
        }
        "rm" => {
            let recursive = arr
                .get(1)
                .and_then(|v| v.get("recursive").and_then(|r| r.as_bool()))
                .unwrap_or(false);
            let force = arr
                .get(1)
                .and_then(|v| v.get("force").and_then(|f| f.as_bool()))
                .unwrap_or(false);
            pi_js::fs::promises::rm(&p0, recursive, force).await?;
            Ok(HostJsResult::Undefined)
        }
        "rename" => {
            pi_js::fs::promises::rename(&p0, &p1).await?;
            Ok(HostJsResult::Undefined)
        }
        "copyFile" => {
            pi_js::fs::promises::copy_file(&p0, &p1).await?;
            Ok(HostJsResult::Undefined)
        }
        "mkdtemp" => Ok(HostJsResult::Json(Value::String(
            pi_js::fs::promises::mkdtemp(&p0).await?,
        ))),
        "access" => {
            pi_js::fs::promises::stat(&p0).await?;
            Ok(HostJsResult::Undefined)
        }
        "chmod" => {
            let mode = arr.get(1).and_then(|v| v.as_u64()).unwrap_or(0o644) as u32;
            pi_js::fs::promises::chmod(&p0, mode).await?;
            Ok(HostJsResult::Undefined)
        }
        _ => Ok(HostJsResult::Undefined),
    }
}

const PRELUDE_JS: &str = r#"
(function () {
    const H = globalThis.__pi_host;

    function formatArg(v) {
        if (typeof v === "string") return v;
        if (v instanceof Error) return v.stack || (v.name + ": " + v.message);
        try { return JSON.stringify(v); } catch { return String(v); }
    }

    globalThis.console = {
        log: (...args) => H.console("log", args.map(formatArg).join(" ")),
        info: (...args) => H.console("log", args.map(formatArg).join(" ")),
        debug: (...args) => H.console("log", args.map(formatArg).join(" ")),
        warn: (...args) => H.console("warn", args.map(formatArg).join(" ")),
        error: (...args) => H.console("error", args.map(formatArg).join(" ")),
    };

    const envProxy = new Proxy({}, {
        get(_t, prop) { return typeof prop === "string" ? H.env("get", prop) : undefined; },
        set(_t, prop, val) { if (typeof prop === "string") H.env("set", prop, String(val)); return true; },
        has(_t, prop) { return typeof prop === "string" && H.env("get", prop) !== undefined; },
        deleteProperty(_t, prop) { if (typeof prop === "string") H.env("del", prop); return true; },
        ownKeys() { return H.env("keys"); },
        getOwnPropertyDescriptor(_t, prop) {
            if (typeof prop !== "string") return undefined;
            const v = H.env("get", prop);
            return v === undefined ? undefined : { enumerable: true, configurable: true, writable: true, value: v };
        }
    });

    globalThis.process = {
        env: envProxy,
        cwd: () => H.env("cwd"),
        chdir: (p) => H.env("chdir", p),
        platform: H.env("platform"),
        arch: H.env("arch"),
        pid: H.env("pid"),
        versions: { node: "24.0.0" },
        features: {},
        exit: (code) => { throw new Error("process.exit(" + (code ?? 0) + ")"); },
        nextTick: (fn, ...args) => Promise.resolve().then(() => fn(...args)),
        getBuiltinModule: () => undefined,
    };

    if (typeof globalThis.TextEncoder === "undefined") {
        globalThis.TextEncoder = class TextEncoder {
            encode(str = "") { return H.b64("utf8Encode", String(str)); }
        };
    }
    if (typeof globalThis.TextDecoder === "undefined") {
        globalThis.TextDecoder = class TextDecoder {
            decode(buf) {
                if (!buf) return "";
                const u8 = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
                return H.b64("utf8Decode", u8);
            }
        };
    }

    if (typeof globalThis.Buffer === "undefined") {
        class PiBuffer extends Uint8Array {
            static from(input, encoding = "utf8") {
                if (typeof input === "string") {
                    if (encoding === "base64") return new PiBuffer(H.b64("decode", input));
                    if (encoding === "base64url") return new PiBuffer(H.b64("decodeUrl", input));
                    if (encoding === "hex") {
                        const arr = new Uint8Array(Math.floor(input.length / 2));
                        for (let i = 0; i < arr.length; i++) arr[i] = parseInt(input.slice(i * 2, i * 2 + 2), 16) || 0;
                        return new PiBuffer(arr);
                    }
                    return new PiBuffer(H.b64("utf8Encode", input));
                }
                if (input instanceof ArrayBuffer) return new PiBuffer(new Uint8Array(input));
                return new PiBuffer(input || 0);
            }
            static alloc(size, fill = 0) {
                const b = new PiBuffer(size);
                if (fill) b.fill(fill);
                return b;
            }
            static isBuffer(obj) { return obj instanceof PiBuffer || obj instanceof Uint8Array; }
            static byteLength(str) { return typeof str === "string" ? H.b64("utf8Encode", str).length : (str?.byteLength ?? 0); }
            static concat(list) {
                const total = list.reduce((s, b) => s + b.length, 0);
                const out = new PiBuffer(total);
                let off = 0;
                for (const b of list) { out.set(b, off); off += b.length; }
                return out;
            }
            toString(encoding = "utf8") {
                if (encoding === "base64") return H.b64("encode", this);
                if (encoding === "base64url") return H.b64("encodeUrl", this);
                if (encoding === "hex") return Array.from(this, (b) => b.toString(16).padStart(2, "0")).join("");
                return H.b64("utf8Decode", this);
            }
        }
        globalThis.Buffer = PiBuffer;
    }

    if (typeof globalThis.URLSearchParams === "undefined") {
        class PiURLSearchParams {
            constructor(init = "") {
                this._entries = [];
                if (typeof init === "string") {
                    const raw = init.startsWith("?") ? init.slice(1) : init;
                    if (raw) {
                        for (const part of raw.split("&")) {
                            const idx = part.indexOf("=");
                            const k = idx >= 0 ? part.slice(0, idx) : part;
                            const v = idx >= 0 ? part.slice(idx + 1) : "";
                            this._entries.push([decodeURIComponent(k.replace(/\+/g, " ")), decodeURIComponent(v.replace(/\+/g, " "))]);
                        }
                    }
                } else if (Array.isArray(init)) {
                    for (const [k, v] of init) this._entries.push([String(k), String(v)]);
                } else if (init && typeof init === "object") {
                    for (const [k, v] of Object.entries(init)) this._entries.push([k, String(v)]);
                }
            }
            get(name) { const e = this._entries.find(([k]) => k === name); return e ? e[1] : null; }
            getAll(name) { return this._entries.filter(([k]) => k === name).map(([, v]) => v); }
            has(name) { return this._entries.some(([k]) => k === name); }
            append(name, value) { this._entries.push([String(name), String(value)]); }
            set(name, value) {
                this.delete(name);
                this._entries.push([String(name), String(value)]);
            }
            delete(name) { this._entries = this._entries.filter(([k]) => k !== name); }
            entries() { return this._entries[Symbol.iterator](); }
            [Symbol.iterator]() { return this.entries(); }
            toString() {
                return this._entries.map(([k, v]) => encodeURIComponent(k) + "=" + encodeURIComponent(v)).join("&");
            }
        }
        globalThis.URLSearchParams = PiURLSearchParams;
    }

    if (typeof globalThis.URL === "undefined") {
        class PiURL {
            static canParse(input, base) { try { new PiURL(input, base); return true; } catch { return false; } }
            constructor(input, base) {
                let raw = String(input);
                if (base !== undefined && !/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(raw)) {
                    const b = new PiURL(base);
                    if (raw.startsWith("/")) raw = b.origin + raw;
                    else {
                        const baseDir = b.pathname.replace(/\/[^/]*$/, "/");
                        raw = b.origin + baseDir + raw;
                    }
                }
                const m = /^([a-zA-Z][a-zA-Z0-9+.-]*):\/\/([^/?#]*)([^?#]*)(\?[^#]*)?(#.*)?$/.exec(raw);
                if (!m) {
                    const fileM = /^(file):\/\/\/(.*)$/.exec(raw);
                    if (!fileM) throw new TypeError("Invalid URL: " + input);
                    this.protocol = "file:";
                    this.host = "";
                    this.hostname = "";
                    this.port = "";
                    this.pathname = "/" + fileM[2];
                    this.search = "";
                    this.hash = "";
                    this.searchParams = new URLSearchParams();
                    return;
                }
                this.protocol = m[1].toLowerCase() + ":";
                this.host = m[2] || "";
                const colon = this.host.lastIndexOf(":");
                if (colon >= 0 && !this.host.startsWith("[")) {
                    this.hostname = this.host.slice(0, colon);
                    this.port = this.host.slice(colon + 1);
                } else {
                    this.hostname = this.host;
                    this.port = "";
                }
                this.pathname = m[3] || "/";
                this.search = m[4] || "";
                this.hash = m[5] || "";
                this.searchParams = new URLSearchParams(this.search);
            }
            get origin() { return this.protocol + "//" + this.host; }
            get href() {
                const q = this.searchParams.toString();
                const search = q ? "?" + q : "";
                return this.protocol + "//" + this.host + this.pathname + search + this.hash;
            }
            toString() { return this.href; }
        }
        globalThis.URL = PiURL;
    }

    if (typeof globalThis.AbortSignal === "undefined") {
        class PiAbortSignal {
            constructor() { this.aborted = false; this.reason = undefined; this._listeners = []; }
            addEventListener(type, fn, opts) {
                if (type !== "abort" || typeof fn !== "function") return;
                if (this.aborted) { fn(); return; }
                const entry = { fn, once: Boolean(opts && opts.once) };
                this._listeners.push(entry);
            }
            removeEventListener(type, fn) {
                if (type !== "abort") return;
                this._listeners = this._listeners.filter((l) => l.fn !== fn);
            }
            throwIfAborted() { if (this.aborted) throw this.reason ?? new Error("Aborted"); }
            static abort(reason) {
                const s = new PiAbortSignal();
                s.aborted = true;
                s.reason = reason ?? new Error("The operation was aborted");
                return s;
            }
        }
        class PiAbortController {
            constructor() { this.signal = new PiAbortSignal(); }
            abort(reason) {
                if (this.signal.aborted) return;
                this.signal.aborted = true;
                this.signal.reason = reason ?? new Error("The operation was aborted");
                const list = this.signal._listeners.slice();
                this.signal._listeners = [];
                for (const l of list) l.fn();
            }
        }
        globalThis.AbortSignal = PiAbortSignal;
        globalThis.AbortController = PiAbortController;
    }

    globalThis.setTimeout = function(fn, ms = 0, ...args) {
        if (typeof fn !== "function") return 0;
        const { id, promise } = H.sleep(Number(ms) || 0);
        promise.then((res) => { if (!res?.cancelled) fn(...args); });
        return id;
    };
    globalThis.clearTimeout = function(id) { H.clearTimer(Number(id) || 0); };
    globalThis.setInterval = function(fn, ms = 0, ...args) {
        let active = true;
        let currentId = 0;
        const tick = () => {
            if (!active) return;
            const { id, promise } = H.sleep(Number(ms) || 0);
            currentId = id;
            promise.then((res) => {
                if (!active || res?.cancelled) return;
                fn(...args);
                tick();
            });
        };
        tick();
        return { _stop: () => { active = false; H.clearTimer(currentId); } };
    };
    globalThis.clearInterval = function(handle) {
        if (handle && typeof handle._stop === "function") handle._stop();
        else H.clearTimer(Number(handle) || 0);
    };

    if (typeof globalThis.structuredClone === "undefined") {
        globalThis.structuredClone = function(v) {
            if (v === undefined) return undefined;
            return JSON.parse(JSON.stringify(v));
        };
    }

    globalThis.fetch = function(url, init) {
        const initObj = init ? {
            method: init.method || "GET",
            headers: init.headers || undefined,
            body: typeof init.body === "string" ? init.body : undefined,
        } : undefined;
        return H.fetch(String(url), initObj).then((raw) => {
            const headerMap = new Map();
            for (const [k, v] of (raw.headers || [])) headerMap.set(String(k).toLowerCase(), String(v));
            const headersObj = {
                get: (name) => headerMap.get(String(name).toLowerCase()) ?? null,
                has: (name) => headerMap.has(String(name).toLowerCase()),
                entries: () => (raw.headers || [])[Symbol.iterator](),
            };
            const bytes = raw.body instanceof Uint8Array ? raw.body : new Uint8Array(raw.body || []);
            return {
                status: raw.status,
                statusText: raw.statusText,
                ok: raw.ok,
                url: raw.url,
                headers: headersObj,
                arrayBuffer: async () => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
                bytes: async () => bytes,
                text: async () => new TextDecoder().decode(bytes),
                json: async () => JSON.parse(new TextDecoder().decode(bytes)),
            };
        });
    };

    const cjsCache = new Map();
    globalThis.__pi_create_require = function(fromDir, _fromFile) {
        return function req(spec) {
            const key = fromDir + "::" + spec;
            if (cjsCache.has(key)) return cjsCache.get(key);
            const src = H.requireSource(fromDir, spec);
            const mod = { exports: {} };
            const fn = new Function("exports", "require", "module", "__filename", "__dirname", src);
            fn(mod.exports, globalThis.__pi_create_require(fromDir, spec), mod, spec, fromDir);
            cjsCache.set(key, mod.exports);
            return mod.exports;
        };
    };
})();
"#;

const MOD_TYPEBOX: &str = r#"
export const Kind = Symbol.for("TypeBox.Kind");
export const OptionalKind = Symbol.for("TypeBox.Optional");

function cloneWithOpts(base, options) {
    const out = Object.assign({}, base);
    if (options && typeof options === "object") {
        for (const [k, v] of Object.entries(options)) {
            if (v !== undefined) out[k] = v;
        }
    }
    return out;
}

function isOptional(schema) {
    return Boolean(schema && (schema[OptionalKind] === "Optional" || schema["~optional"] === true));
}

export function StringEnum(values, options = {}) {
    const obj = { type: "string", enum: Array.from(values) };
    if (options.description) obj.description = options.description;
    if (options.default) obj.default = options.default;
    return obj;
}

export const Type = {
    String: (options) => cloneWithOpts({ [Kind]: "String", type: "string" }, options),
    Number: (options) => cloneWithOpts({ [Kind]: "Number", type: "number" }, options),
    Integer: (options) => cloneWithOpts({ [Kind]: "Integer", type: "integer" }, options),
    Boolean: (options) => cloneWithOpts({ [Kind]: "Boolean", type: "boolean" }, options),
    Null: (options) => cloneWithOpts({ [Kind]: "Null", type: "null" }, options),
    Any: (options) => cloneWithOpts({ [Kind]: "Any" }, options),
    Unknown: (options) => cloneWithOpts({ [Kind]: "Unknown" }, options),
    Never: (options) => cloneWithOpts({ [Kind]: "Never", not: {} }, options),
    Literal: (value, options) => cloneWithOpts({ [Kind]: "Literal", const: value, type: typeof value }, options),
    Array: (items, options) => cloneWithOpts({ [Kind]: "Array", type: "array", items }, options),
    Tuple: (items, options) => cloneWithOpts({
        [Kind]: "Tuple",
        type: "array",
        items,
        additionalItems: false,
        minItems: items.length,
        maxItems: items.length,
    }, options),
    Union: (anyOf, options) => cloneWithOpts({ [Kind]: "Union", anyOf }, options),
    Intersect: (allOf, options) => cloneWithOpts({ [Kind]: "Intersect", allOf }, options),
    Record: (key, pattern, options) => cloneWithOpts({
        [Kind]: "Record",
        type: "object",
        patternProperties: { "^.*$": pattern },
        additionalProperties: false,
    }, options),
    Enum: (valuesOrObj, options) => {
        const vals = Array.isArray(valuesOrObj) ? valuesOrObj : Object.values(valuesOrObj);
        return cloneWithOpts({ [Kind]: "Enum", anyOf: vals.map((v) => ({ const: v, type: typeof v })) }, options);
    },
    Optional: (schema) => {
        const copy = Object.assign({}, schema);
        Object.defineProperty(copy, OptionalKind, { value: "Optional", enumerable: false, configurable: true });
        return copy;
    },
    Unsafe: (schema) => Object.assign({}, schema),
    Object: (properties, options) => {
        const cleanProps = {};
        const required = [];
        for (const [k, propSchema] of Object.entries(properties || {})) {
            if (!isOptional(propSchema)) required.push(k);
            cleanProps[k] = propSchema;
        }
        const out = cloneWithOpts({ [Kind]: "Object", type: "object", properties: cleanProps }, options);
        if (required.length > 0) out.required = required;
        return out;
    },
};

export default { Type, StringEnum, Kind, OptionalKind };
"#;

const MOD_TYPEBOX_VALUE: &str = r#"
export const Value = {
    Check: (schema, value) => globalThis.__pi_host.typebox("check", schema, value),
    Errors: (schema, value) => globalThis.__pi_host.typebox("errors", schema, value),
    Convert: (schema, value) => globalThis.__pi_host.typebox("convert", schema, value),
    Clean: (schema, value) => globalThis.__pi_host.typebox("clean", schema, value),
    Default: (schema, value) => globalThis.__pi_host.typebox("default", schema, value),
    Clone: (value) => globalThis.structuredClone(value),
};
export default Value;
"#;

const MOD_TYPEBOX_COMPILE: &str = r#"
export function Compile(schema) {
    return {
        Check: (value) => globalThis.__pi_host.typebox("check", schema, value),
        Errors: (value) => globalThis.__pi_host.typebox("errors", schema, value),
    };
}
export default { Compile };
"#;

const MOD_PI_TUI: &str = r#"
export const Key = {
    escape: "escape", esc: "escape", enter: "enter", return: "enter", tab: "tab", space: "space",
    backspace: "backspace", delete: "delete", home: "home", end: "end", up: "up", down: "down",
    left: "left", right: "right", pageUp: "pageUp", pageDown: "pageDown",
    ctrl: (k) => "ctrl+" + k,
    shift: (k) => "shift+" + k,
    alt: (k) => "alt+" + k,
    super: (k) => "super+" + k,
    ctrlShift: (k) => "ctrl+shift+" + k,
    shiftCtrl: (k) => "ctrl+shift+" + k,
    ctrlAlt: (k) => "ctrl+alt+" + k,
};

export function matchesKey(data, keyId) {
    return String(data || "").toLowerCase() === String(keyId || "").toLowerCase();
}
export function parseKey(data) { return data || undefined; }
export function visibleWidth(str) { return String(str || "").length; }
export function truncateToWidth(str, width, ellipsis = "...") {
    const s = String(str || "");
    if (s.length <= width) return s;
    if (width <= ellipsis.length) return ellipsis.slice(0, width);
    return s.slice(0, width - ellipsis.length) + ellipsis;
}
export function hyperlink(text, url) { return `\x1b]8;;${url}\x07${text}\x1b]8;;\x07`; }

export class Text {
    constructor(text = "", paddingX = 1, paddingY = 1) {
        this._text = String(text ?? "");
        this.paddingX = paddingX;
        this.paddingY = paddingY;
    }
    setText(t) { this._text = String(t ?? ""); }
    render(_width) { return this._text.split("\n"); }
    invalidate() {}
}

export class Spacer {
    constructor(lines = 1) { this.lines = lines; }
    render(_width) { return Array(Math.max(0, this.lines)).fill(""); }
    invalidate() {}
}

export class Container {
    constructor() { this.children = []; }
    addChild(c) { this.children.push(c); }
    removeChild(c) { this.children = this.children.filter((x) => x !== c); }
    clear() { this.children = []; }
    render(width) {
        const lines = [];
        for (const c of this.children) {
            if (c && typeof c.render === "function") lines.push(...c.render(width));
        }
        return lines;
    }
    invalidate() { for (const c of this.children) c?.invalidate?.(); }
}

export class Box extends Container {}
export class Markdown extends Text {}
export class SelectList {
    constructor(items = []) { this.items = items; }
    render(_width) { return this.items.map((i) => typeof i === "string" ? i : (i.label || i.value || "")); }
    handleInput() {}
    invalidate() {}
}
export class SettingsList extends SelectList {}
export class Input {
    constructor() { this._value = ""; }
    getValue() { return this._value; }
    setValue(v) { this._value = String(v ?? ""); }
    render(_width) { return [this._value]; }
    handleInput() {}
    invalidate() {}
}
export class Editor extends Input {}
export class CustomEditor extends Editor {
    constructor(tui, theme, keybindings) {
        super();
        this.tui = tui;
        this.theme = theme;
        this.keybindings = keybindings;
    }
}
"#;

const MOD_PI_AI: &str = r#"
export { Type, StringEnum } from "typebox";
export function contentText(msg) {
    if (!msg) return "";
    if (typeof msg.content === "string") return msg.content;
    if (Array.isArray(msg.content)) {
        return msg.content.filter((b) => b?.type === "text").map((b) => b.text || "").join("");
    }
    return "";
}
export function modelsAreEqual(a, b) {
    return Boolean(a && b && a.provider === b.provider && a.id === b.id);
}
export function getProviders() { return []; }
export function getModels() { return []; }
export function getModel() { return undefined; }
const EXTENDED_THINKING_LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
export function getSupportedThinkingLevels(model) {
    if (!model || !model.reasoning) return ["off"];
    return EXTENDED_THINKING_LEVELS.filter((level) => {
        const mapped = model.thinkingLevelMap ? model.thinkingLevelMap[level] : undefined;
        if (mapped === null) return false;
        if (mapped === undefined && (level === "xhigh" || level === "max")) return false;
        return true;
    });
}
export function clampThinkingLevel(model, level) {
    const available = getSupportedThinkingLevels(model);
    if (available.includes(level)) return level;
    const requestedIndex = EXTENDED_THINKING_LEVELS.indexOf(level);
    if (requestedIndex === -1) return available[0] ?? "off";
    for (let i = requestedIndex; i < EXTENDED_THINKING_LEVELS.length; i++) {
        if (available.includes(EXTENDED_THINKING_LEVELS[i])) return EXTENDED_THINKING_LEVELS[i];
    }
    for (let i = requestedIndex - 1; i >= 0; i--) {
        if (available.includes(EXTENDED_THINKING_LEVELS[i])) return EXTENDED_THINKING_LEVELS[i];
    }
    return available[0] ?? "off";
}
export async function complete() { throw new Error("complete() in QuickJS guest requires host provider bridge"); }
export async function completeSimple() { throw new Error("completeSimple() in QuickJS guest requires host provider bridge"); }
"#;

const MOD_PI_AI_MODELS: &str = r#"
export { clampThinkingLevel, getModel, getModels, getProviders, getSupportedThinkingLevels, modelsAreEqual } from "@earendil-works/pi-ai";
"#;

const MOD_PI_AI_OAUTH: &str = r#"
export const oauthSupported = true;
"#;

const MOD_PI_AI_PROVIDERS: &str = r#"
export function registerBuiltInApiProviders() {}
"#;

const MOD_PI_AGENT_CORE: &str = r#"
export const DEFAULT_THINKING_LEVEL = "off";
"#;

const MOD_PI_CODING_AGENT: &str = r#"
export { Key, Text, Container, Spacer, Box, Markdown, SettingsList, SelectList, CustomEditor } from "@earendil-works/pi-tui";

export const VERSION = "1.5.2";
export const APP_NAME = "pi";
export const CONFIG_DIR_NAME = ".pi";
export const DEFAULT_MAX_LINES = 2000;
export const DEFAULT_MAX_BYTES = 50 * 1024;
export const GREP_MAX_LINE_LENGTH = 500;

export function defineTool(tool) { return tool; }
export function getAgentDir() { return globalThis.__pi_host.env("agentDir"); }
export function formatSize(bytes) { return globalThis.__pi_host.truncate("formatSize", Number(bytes) || 0); }
export function truncateHead(content, options) {
    return globalThis.__pi_host.truncate("head", { content: String(content ?? ""), ...(options || {}) });
}
export function truncateTail(content, options) {
    return globalThis.__pi_host.truncate("tail", { content: String(content ?? ""), ...(options || {}) });
}
export function truncateLine(line, maxChars) {
    return globalThis.__pi_host.truncate("line", { line: String(line ?? ""), maxChars });
}
export function truncateMiddle(content, maxBytes) {
    return globalThis.__pi_host.truncate("middle", { content: String(content ?? ""), maxBytes });
}

const mutationQueues = new Map();
export function withFileMutationQueue(filePath, fn) {
    const key = String(filePath);
    const prev = mutationQueues.get(key) || Promise.resolve();
    const next = prev.then(() => fn(), () => fn());
    mutationQueues.set(key, next.catch(() => {}));
    return next;
}

export function isBashToolResult(e) { return e?.toolName === "bash"; }
export function isPowerShellToolResult(e) { return e?.toolName === "powershell"; }
export function isReadToolResult(e) { return e?.toolName === "read"; }
export function isEditToolResult(e) { return e?.toolName === "edit"; }
export function isWriteToolResult(e) { return e?.toolName === "write"; }
export function isGrepToolResult(e) { return e?.toolName === "grep"; }
export function isFindToolResult(e) { return e?.toolName === "find"; }
export function isLsToolResult(e) { return e?.toolName === "ls"; }
export function isToolCallEventType(toolName, event) { return event?.toolName === toolName; }

export function getSettingsListTheme() { return {}; }
export function getMarkdownTheme() { return {}; }
export function getSelectListTheme() { return {}; }
export class DynamicBorder {
    constructor() {}
    render() { return ["---"]; }
    invalidate() {}
}
"#;

const MOD_NODE_PATH: &str = r#"
export const sep = globalThis.__pi_host.path("sep", []);
export const delimiter = globalThis.__pi_host.path("delimiter", []);
export function join(...parts) { return globalThis.__pi_host.path("join", parts.map(String)); }
export function resolve(...parts) { return globalThis.__pi_host.path("resolve", parts.map(String)); }
export function normalize(p) { return globalThis.__pi_host.path("normalize", [String(p)]); }
export function relative(from, to) { return globalThis.__pi_host.path("relative", [String(from), String(to)]); }
export function dirname(p) { return globalThis.__pi_host.path("dirname", [String(p)]); }
export function basename(p, ext) { return globalThis.__pi_host.path("basename", ext !== undefined ? [String(p), String(ext)] : [String(p)]); }
export function extname(p) { return globalThis.__pi_host.path("extname", [String(p)]); }
export function isAbsolute(p) { return globalThis.__pi_host.path("isAbsolute", [String(p)]); }
export const posix = {
    sep: "/", delimiter: ":",
    join: (...parts) => globalThis.__pi_host.path("posix_join", parts.map(String)),
    resolve: (...parts) => globalThis.__pi_host.path("posix_resolve", parts.map(String)),
    normalize: (p) => globalThis.__pi_host.path("posix_normalize", [String(p)]),
    relative: (a, b) => globalThis.__pi_host.path("posix_relative", [String(a), String(b)]),
    dirname: (p) => globalThis.__pi_host.path("posix_dirname", [String(p)]),
    basename: (p, ext) => globalThis.__pi_host.path("posix_basename", ext !== undefined ? [String(p), String(ext)] : [String(p)]),
    extname: (p) => globalThis.__pi_host.path("posix_extname", [String(p)]),
    isAbsolute: (p) => globalThis.__pi_host.path("posix_isAbsolute", [String(p)]),
};
export default { sep, delimiter, join, resolve, normalize, relative, dirname, basename, extname, isAbsolute, posix };
"#;

const MOD_NODE_OS: &str = r#"
export const EOL = globalThis.__pi_host.env("platform") === "win32" ? "\r\n" : "\n";
export function homedir() { return globalThis.__pi_host.env("home"); }
export function tmpdir() { return globalThis.__pi_host.env("tmp"); }
export function platform() { return globalThis.__pi_host.env("platform"); }
export function arch() { return globalThis.__pi_host.env("arch"); }
export function release() { return globalThis.__pi_host.env("release"); }
export default { EOL, homedir, tmpdir, platform, arch, release };
"#;

const MOD_NODE_FS_PROMISES: &str = r#"
function wrapStat(raw) {
    return {
        isFile: () => Boolean(raw._isFile),
        isDirectory: () => Boolean(raw._isDirectory),
        isSymbolicLink: () => Boolean(raw._isSymbolicLink),
        size: raw.size || 0,
        mtimeMs: raw.mtimeMs || 0,
        mode: raw.mode || 0,
    };
}
function wrapDirent(raw) {
    return {
        name: raw.name,
        isFile: () => Boolean(raw._isFile),
        isDirectory: () => Boolean(raw._isDirectory),
        isSymbolicLink: () => Boolean(raw._isSymbolicLink),
    };
}
export function readFile(p, opts) {
    const enc = typeof opts === "string" ? { encoding: opts } : opts;
    return globalThis.__pi_host.fsAsync("readFile", [String(p), enc]);
}
export function writeFile(p, data, _opts) { return globalThis.__pi_host.fsAsync("writeFile", [String(p), String(data)]); }
export function appendFile(p, data, _opts) { return globalThis.__pi_host.fsAsync("appendFile", [String(p), String(data)]); }
export function mkdir(p, opts) { return globalThis.__pi_host.fsAsync("mkdir", [String(p), opts || {}]); }
export async function readdir(p, opts) {
    const res = await globalThis.__pi_host.fsAsync("readdir", [String(p), opts || {}]);
    if (opts && opts.withFileTypes) return res.map(wrapDirent);
    return res;
}
export async function stat(p) { return wrapStat(await globalThis.__pi_host.fsAsync("stat", [String(p)])); }
export async function lstat(p) { return wrapStat(await globalThis.__pi_host.fsAsync("lstat", [String(p)])); }
export function realpath(p) { return globalThis.__pi_host.fsAsync("realpath", [String(p)]); }
export function unlink(p) { return globalThis.__pi_host.fsAsync("unlink", [String(p)]); }
export function rm(p, opts) { return globalThis.__pi_host.fsAsync("rm", [String(p), opts || {}]); }
export function rename(a, b) { return globalThis.__pi_host.fsAsync("rename", [String(a), String(b)]); }
export function copyFile(a, b) { return globalThis.__pi_host.fsAsync("copyFile", [String(a), String(b)]); }
export function mkdtemp(p) { return globalThis.__pi_host.fsAsync("mkdtemp", [String(p)]); }
export function access(p) { return globalThis.__pi_host.fsAsync("access", [String(p)]); }
export function chmod(p, mode) { return globalThis.__pi_host.fsAsync("chmod", [String(p), Number(mode)]); }
export default { readFile, writeFile, appendFile, mkdir, readdir, stat, lstat, realpath, unlink, rm, rename, copyFile, mkdtemp, access, chmod };
"#;

const MOD_NODE_FS: &str = r#"
import * as promisesMod from "node:fs/promises";
export const promises = promisesMod;
export const constants = { F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1 };
function wrapStat(raw) {
    return {
        isFile: () => Boolean(raw._isFile),
        isDirectory: () => Boolean(raw._isDirectory),
        isSymbolicLink: () => Boolean(raw._isSymbolicLink),
        size: raw.size || 0,
        mtimeMs: raw.mtimeMs || 0,
        mode: raw.mode || 0,
    };
}
function wrapDirent(raw) {
    return {
        name: raw.name,
        isFile: () => Boolean(raw._isFile),
        isDirectory: () => Boolean(raw._isDirectory),
        isSymbolicLink: () => Boolean(raw._isSymbolicLink),
    };
}
export function existsSync(p) { return globalThis.__pi_host.fsSync("existsSync", [String(p)]); }
export function readFileSync(p, opts) {
    const enc = typeof opts === "string" ? { encoding: opts } : opts;
    return globalThis.__pi_host.fsSync("readFileSync", [String(p), enc]);
}
export function writeFileSync(p, data) { return globalThis.__pi_host.fsSync("writeFileSync", [String(p), String(data)]); }
export function appendFileSync(p, data) { return globalThis.__pi_host.fsSync("appendFileSync", [String(p), String(data)]); }
export function mkdirSync(p, opts) { return globalThis.__pi_host.fsSync("mkdirSync", [String(p), opts || {}]); }
export function readdirSync(p, opts) {
    const res = globalThis.__pi_host.fsSync("readdirSync", [String(p), opts || {}]);
    if (opts && opts.withFileTypes) return res.map(wrapDirent);
    return res;
}
export function statSync(p) { return wrapStat(globalThis.__pi_host.fsSync("statSync", [String(p)])); }
export function lstatSync(p) { return wrapStat(globalThis.__pi_host.fsSync("lstatSync", [String(p)])); }
export function realpathSync(p) { return globalThis.__pi_host.fsSync("realpathSync", [String(p)]); }
export function unlinkSync(p) { return globalThis.__pi_host.fsSync("unlinkSync", [String(p)]); }
export function rmSync(p, opts) { return globalThis.__pi_host.fsSync("rmSync", [String(p), opts || {}]); }
export function renameSync(a, b) { return globalThis.__pi_host.fsSync("renameSync", [String(a), String(b)]); }
export function copyFileSync(a, b) { return globalThis.__pi_host.fsSync("copyFileSync", [String(a), String(b)]); }
export function mkdtempSync(p) { return globalThis.__pi_host.fsSync("mkdtempSync", [String(p)]); }
export function accessSync(p) { return globalThis.__pi_host.fsSync("accessSync", [String(p)]); }
export function chmodSync(p, mode) { return globalThis.__pi_host.fsSync("chmodSync", [String(p), Number(mode)]); }
export default {
    promises, constants, existsSync, readFileSync, writeFileSync, appendFileSync,
    mkdirSync, readdirSync, statSync, lstatSync, realpathSync, unlinkSync, rmSync,
    renameSync, copyFileSync, mkdtempSync, accessSync, chmodSync,
};
"#;

const MOD_NODE_EVENTS: &str = r#"
export class EventEmitter {
    constructor() { this._events = new Map(); }
    on(event, listener) {
        let list = this._events.get(event);
        if (!list) { list = []; this._events.set(event, list); }
        list.push(listener);
        return this;
    }
    addListener(event, listener) { return this.on(event, listener); }
    once(event, listener) {
        const wrapper = (...args) => { this.off(event, wrapper); listener.apply(this, args); };
        wrapper._orig = listener;
        return this.on(event, wrapper);
    }
    off(event, listener) { return this.removeListener(event, listener); }
    removeListener(event, listener) {
        const list = this._events.get(event);
        if (!list) return this;
        const next = list.filter((fn) => fn !== listener && fn._orig !== listener);
        if (next.length) this._events.set(event, next);
        else this._events.delete(event);
        return this;
    }
    removeAllListeners(event) {
        if (event === undefined) this._events.clear();
        else this._events.delete(event);
        return this;
    }
    emit(event, ...args) {
        const list = this._events.get(event);
        if (!list || !list.length) return false;
        for (const fn of list.slice()) fn.apply(this, args);
        return true;
    }
    listeners(event) { return (this._events.get(event) || []).slice(); }
    listenerCount(event) { return (this._events.get(event) || []).length; }
    eventNames() { return Array.from(this._events.keys()); }
}
export default EventEmitter;
"#;

const MOD_NODE_URL: &str = r#"
export const URL = globalThis.URL;
export const URLSearchParams = globalThis.URLSearchParams;
export function fileURLToPath(u) {
    const url = typeof u === "string" ? new URL(u) : u;
    return decodeURIComponent(url.pathname);
}
export function pathToFileURL(p) {
    const s = String(p).replace(/\\/g, "/");
    return new URL("file://" + (s.startsWith("/") ? "" : "/") + encodeURI(s));
}
export default { URL, URLSearchParams, fileURLToPath, pathToFileURL };
"#;

const MOD_NODE_CRYPTO: &str = r#"
export function randomUUID() { return globalThis.__pi_host.crypto("uuid"); }
export function randomBytes(n) { return globalThis.__pi_host.crypto("randomBytes", Number(n) || 0); }
export function createHash(alg) {
    if (String(alg).toLowerCase() !== "sha256") throw new Error("Unsupported hash algorithm: " + alg);
    let buf = "";
    return {
        update(data) { buf += typeof data === "string" ? data : new TextDecoder().decode(data); return this; },
        digest(enc) {
            if (enc === "hex") return globalThis.__pi_host.crypto("sha256_hex", buf);
            return globalThis.__pi_host.crypto("sha256_bytes", buf);
        },
    };
}
export default { randomUUID, randomBytes, createHash };
"#;

const MOD_NODE_MODULE: &str = r#"
export const builtinModules = ["path", "os", "fs", "fs/promises", "events", "url", "crypto", "module", "child_process"];
export function createRequire(urlOrPath) {
    const p = String(urlOrPath || "");
    const dir = p.startsWith("file://") ? decodeURIComponent(new URL(p).pathname).replace(/\/[^/]*$/, "") : p.replace(/\/[^/]*$/, "");
    return globalThis.__pi_create_require(dir || ".", p);
}
export default { builtinModules, createRequire };
"#;

const MOD_NODE_CHILD_PROCESS: &str = r#"
export function execSync(command, options) {
    return globalThis.__pi_host.execSync(String(command), options || {});
}
export function execFile(file, args, options, cb) {
    if (typeof args === "function") { cb = args; args = []; options = {}; }
    else if (typeof options === "function") { cb = options; options = {}; }
    globalThis.__pi_host.execAsync(String(file), args || [], options || {}).then(
        (res) => cb && cb(res.code !== 0 ? new Error("Exit code " + res.code) : null, res.stdout, res.stderr),
        (err) => cb && cb(err, "", String(err))
    );
}
export function exec(command, options, cb) {
    if (typeof options === "function") { cb = options; options = {}; }
    execFile("sh", ["-c", String(command)], options || {}, cb);
}
export default { execSync, execFile, exec };
"#;

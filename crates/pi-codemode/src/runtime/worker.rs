//! Port of packages/codemode/src/runtime/worker.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use serde_json::{Map, Value};

use crate::runtime::prelude_source::PRELUDE_SOURCE;
use crate::runtime::protocol::{HostToWorkerMessage, WorkerCallTarget, WorkerData, WorkerToHostMessage};
use crate::vendor::quickjs_wasi::{
    JSException, MAX_STACK_SIZE, QuickJS, QuickJSOptions, WasiMemory, WasiOptions, WasiOverrides,
};

// PORT: body of a dedicated `std::thread` (PORTING.md §2.6). The TS module is a Node worker
// entry (`parentPort` / `workerData`) that starts on import. The host spawns the thread;
// `incoming` is `parentPort.on("message")` and `outgoing` is `parentPort.postMessage`.
// Returning from [`run`] is the worker `exit`.

/// Runs one script on the calling thread. Ports `main` in `worker.ts`.
pub(crate) fn run(data: WorkerData, incoming: Receiver<HostToWorkerMessage>, outgoing: Sender<WorkerToHostMessage>) {
    if let Err(err) = run_inner(data, incoming, &outgoing) {
        crash(&outgoing, &err);
    }
}

struct VmDisposeGuard(QuickJS);

impl Drop for VmDisposeGuard {
    fn drop(&mut self) {
        self.0.dispose();
    }
}

fn run_inner(
    data: WorkerData,
    incoming: Receiver<HostToWorkerMessage>,
    outgoing: &Sender<WorkerToHostMessage>,
) -> pi_js::Result<()> {
    let interrupt_flag = Arc::clone(&data.interrupt);
    let interrupt_cb_flag = Arc::clone(&interrupt_flag);
    let vm = QuickJS::create(QuickJSOptions {
        wasm: data.wasm,
        memory_limit: data.memory_limit_bytes.filter(|v| v.is_finite() && *v > 0.0),
        max_stack_size: Some(MAX_STACK_SIZE),
        interrupt_handler: Some(Arc::new(move || interrupt_cb_flag.load(Ordering::Acquire) != 0)),
        wasi: Some(discard_output()),
    })?;
    let _guard = VmDisposeGuard(vm.clone());

    let done_sent = Arc::new(AtomicBool::new(false));
    let done_bridge = Arc::clone(&done_sent);
    let bridge_out = outgoing.clone();
    let bridge_vm = vm.clone();

    let bridge_fn = vm.new_function(
        "bridge",
        Arc::new(move |_this, args| {
            let Some(tag_h) = args.first() else {
                return bridge_vm.undefined();
            };
            let tag = tag_h.to_string()?;
            match tag.as_str() {
                "call" | "global" => {
                    let target = if tag == "global" {
                        WorkerCallTarget::Global
                    } else {
                        WorkerCallTarget::Tool
                    };
                    let id = args.get(1).map(|h| h.to_number()).transpose()?.unwrap_or(0.0) as i64;
                    let name = args.get(2).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                    let args_json = match args.get(3) {
                        Some(h) if !h.is_undefined() => Some(h.to_string()?),
                        _ => None,
                    };
                    post(
                        &bridge_out,
                        WorkerToHostMessage::Call {
                            id,
                            target,
                            name,
                            args: args_json,
                        },
                    );
                }
                "output" => {
                    let kind = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                    match kind.as_str() {
                        "text" => {
                            let text = args.get(2).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                            post(
                                &bridge_out,
                                WorkerToHostMessage::Output {
                                    item: crate::types::CodemodeOutputItem::Text { text },
                                },
                            );
                        }
                        "image" => {
                            let data = args.get(2).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                            let mime_type = args.get(3).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                            post(
                                &bridge_out,
                                WorkerToHostMessage::Output {
                                    item: crate::types::CodemodeOutputItem::Image { data, mime_type },
                                },
                            );
                        }
                        _ => {}
                    }
                }
                "done" => {
                    let ok = args.get(1).map(|h| h.to_boolean()).transpose()?.unwrap_or(false);
                    let payload = match args.get(2) {
                        Some(h) if !h.is_undefined() => Some(h.to_string()?),
                        _ => None,
                    };
                    let writes = match args.get(3) {
                        Some(h) if !h.is_undefined() => Some(h.to_string()?),
                        _ => None,
                    };
                    done_bridge.store(true, Ordering::Release);
                    if ok {
                        post(
                            &bridge_out,
                            WorkerToHostMessage::Done {
                                ok: true,
                                value: payload,
                                writes,
                                error: None,
                            },
                        );
                    } else {
                        post(
                            &bridge_out,
                            WorkerToHostMessage::Done {
                                ok: false,
                                value: None,
                                writes: None,
                                error: payload,
                            },
                        );
                    }
                }
                _ => {}
            }
            bridge_vm.undefined()
        }),
    )?;

    let factory = vm
        .eval_code(PRELUDE_SOURCE, Some("codemode-prelude.js"), None)
        .map_err(map_js_exception)?;

    let tools_json = pi_js::json::stringify(&data.tools);
    let globals_json = pi_js::json::stringify(&data.globals);
    let store_json = pi_js::json::stringify(&data.store);

    let tools_h = vm.new_string(&tools_json)?;
    let globals_h = vm.new_string(&globals_json)?;
    let store_h = vm.new_string(&store_json)?;
    let undef_h = vm.undefined()?;

    let api_res = vm.call_function(
        &factory,
        &undef_h,
        &[bridge_fn.clone(), tools_h.clone(), globals_h.clone(), store_h.clone()],
    );
    factory.dispose();
    tools_h.dispose();
    globals_h.dispose();
    store_h.dispose();
    undef_h.dispose();

    let api = api_res.map_err(map_js_exception)?;
    let run_fn = api.get_prop("run")?;
    let settle_fn = api.get_prop("settle")?;
    let stalled_fn = api.get_prop("stalled")?;
    api.dispose();

    // The prefix shares the first line with the script so reported line numbers
    // match the script as written.
    let wrapped = format!("(async (tools, console) => {{{}\n}})", data.code);
    let script_fn = match vm.eval_code(&wrapped, Some("codemode.js"), None) {
        Ok(h) => h,
        Err(err) => {
            run_fn.dispose();
            settle_fn.dispose();
            stalled_fn.dispose();
            bridge_fn.dispose();
            if interrupt_flag.load(Ordering::Acquire) != 0 {
                return Ok(());
            }
            if let Some(jse) = err.downcast_ref::<JSException>() {
                post(
                    outgoing,
                    WorkerToHostMessage::Done {
                        ok: false,
                        value: None,
                        writes: None,
                        error: Some(describe_exception(jse)),
                    },
                );
                return Ok(());
            }
            return Err(err);
        }
    };

    let undef = vm.undefined()?;
    let run_res = vm.call_function(&run_fn, &undef, &[script_fn.clone()]);
    undef.dispose();
    script_fn.dispose();

    match run_res {
        Ok(h) => h.dispose(),
        Err(err) => {
            run_fn.dispose();
            settle_fn.dispose();
            stalled_fn.dispose();
            bridge_fn.dispose();
            if interrupt_flag.load(Ordering::Acquire) != 0 {
                return Ok(());
            }
            return Err(map_js_exception(err));
        }
    }

    if let Err(err) = drain_and_check_stalled(&vm, &stalled_fn, &done_sent, &interrupt_flag) {
        run_fn.dispose();
        settle_fn.dispose();
        stalled_fn.dispose();
        bridge_fn.dispose();
        return Err(err);
    }

    while !done_sent.load(Ordering::Acquire) && interrupt_flag.load(Ordering::Acquire) == 0 {
        let Ok(msg) = incoming.recv() else {
            break;
        };
        if done_sent.load(Ordering::Acquire) || interrupt_flag.load(Ordering::Acquire) != 0 {
            break;
        }
        match msg {
            HostToWorkerMessage::Result { id, ok, payload } => {
                let id_h = vm.new_number(id as f64)?;
                let ok_h = vm.new_boolean(ok)?;
                let payload_h = match payload {
                    Some(ref s) => vm.new_string(s)?,
                    None => vm.undefined()?,
                };
                let undef = vm.undefined()?;
                let res = vm.call_function(&settle_fn, &undef, &[id_h.clone(), ok_h.clone(), payload_h.clone()]);
                id_h.dispose();
                ok_h.dispose();
                payload_h.dispose();
                undef.dispose();
                match res {
                    Ok(h) => h.dispose(),
                    Err(err) => {
                        run_fn.dispose();
                        settle_fn.dispose();
                        stalled_fn.dispose();
                        bridge_fn.dispose();
                        if interrupt_flag.load(Ordering::Acquire) != 0 {
                            return Ok(());
                        }
                        return Err(map_js_exception(err));
                    }
                }
                if let Err(err) = drain_and_check_stalled(&vm, &stalled_fn, &done_sent, &interrupt_flag) {
                    run_fn.dispose();
                    settle_fn.dispose();
                    stalled_fn.dispose();
                    bridge_fn.dispose();
                    return Err(err);
                }
            }
        }
    }

    run_fn.dispose();
    settle_fn.dispose();
    stalled_fn.dispose();
    bridge_fn.dispose();
    Ok(())
}

fn drain_and_check_stalled(
    vm: &QuickJS,
    stalled_fn: &crate::vendor::quickjs_wasi::JSValueHandle,
    done_sent: &AtomicBool,
    interrupt_flag: &AtomicI32,
) -> pi_js::Result<()> {
    if let Err(err) = vm.execute_pending_jobs() {
        if interrupt_flag.load(Ordering::Acquire) != 0 {
            return Ok(());
        }
        return Err(map_js_exception(err));
    }
    if !done_sent.load(Ordering::Acquire) && interrupt_flag.load(Ordering::Acquire) == 0 {
        let undef = vm.undefined()?;
        let res = vm.call_function(stalled_fn, &undef, &[]);
        undef.dispose();
        match res {
            Ok(h) => h.dispose(),
            Err(err) => {
                if interrupt_flag.load(Ordering::Acquire) != 0 {
                    return Ok(());
                }
                return Err(map_js_exception(err));
            }
        }
    }
    Ok(())
}

fn describe_exception(error: &JSException) -> String {
    let head = if error.message().is_empty() {
        error.name().to_string()
    } else {
        format!("{}: {}", error.name(), error.message())
    };
    let stack = match error.stack() {
        Some(frames) if !frames.trim_end().is_empty() => format!("{head}\n{}", frames.trim_end()),
        _ => head,
    };
    let mut obj = Map::new();
    obj.insert("name".to_string(), Value::String(error.name().to_string()));
    obj.insert("message".to_string(), Value::String(error.message().to_string()));
    obj.insert("stack".to_string(), Value::String(stack));
    pi_js::json::stringify(&Value::Object(obj))
}

fn map_js_exception(err: pi_js::Error) -> pi_js::Error {
    if let Some(jse) = err.downcast_ref::<JSException>() {
        let msg = match jse.stack() {
            Some(stack) if !stack.trim_end().is_empty() => format!("{}\n{}", jse.message(), stack.trim_end()),
            _ => jse.message().to_string(),
        };
        pi_js::Error::js(jse.name(), msg)
    } else {
        err
    }
}

fn post(outgoing: &Sender<WorkerToHostMessage>, message: WorkerToHostMessage) {
    let _ = outgoing.send(message);
}

fn crash(outgoing: &Sender<WorkerToHostMessage>, error: &pi_js::Error) {
    post(
        outgoing,
        WorkerToHostMessage::Crash {
            message: format!("{}: {}", error.name(), error.message()),
        },
    );
}

/// QuickJS writes engine diagnostics to fd 1 and 2, which the default shim
/// forwards to the host's stdout and stderr. That output belongs to the host
/// application (for example a TUI), so it is discarded. Reporting every byte as
/// written keeps libc from retrying.
fn discard_output() -> WasiOptions {
    Arc::new(|_memory| WasiOverrides {
        fd_write: Some(Arc::new(
            |memory: &WasiMemory, fd: i64, iovs_ptr: i64, iovs_len: i64, nwritten_ptr: i64| -> i64 {
                if fd != 1 && fd != 2 {
                    return 8;
                }
                let mut written = 0i64;
                for i in 0..iovs_len {
                    written += memory.read_u32_le(iovs_ptr + i * 8 + 4);
                }
                memory.write_u32_le(nwritten_ptr, written);
                0
            },
        )),
    })
}

//! Port of packages/codemode/src/runtime/vm.ts
//!
//! Drives one QuickJS VM through one script, independent of the transport that connects it to the host.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::runtime::prelude_source::PRELUDE_SOURCE;
use crate::runtime::protocol::{HostToWorkerMessage, VmStartData, WorkerCallTarget, WorkerToHostMessage};
use crate::types::{CodemodeDuring, CodemodeOutputItem};
use crate::vendor::quickjs_wasi::{
    Intrinsics, JSException, JSValueHandle, MAX_STACK_SIZE, QuickJS, QuickJSOptions, TimezoneOffset, WasiMemory,
    WasiOptions, WasiOverrides,
};
use crate::wasm::CodemodeWasmModule;

/// Bumped whenever this module or the prelude changes what a deterministic script observes, beyond
/// what the fingerprint's other parts (engine versions, prelude hash, limits) already capture.
const DETERMINISTIC_VERSION: u32 = 1;

/// Promise jobs run between two `shouldInterrupt` polls while draining the job queue.
const JOBS_PER_POLL: usize = 50;

/// FNV-1a over UTF-16 code units, as 8 hex digits: identifies the prelude in the fingerprint.
fn fnv1a_utf16(text: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in text.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

static PRELUDE_HASH: LazyLock<String> = LazyLock::new(|| fnv1a_utf16(PRELUDE_SOURCE));

pub struct StartVmOptions {
    pub data: VmStartData,
    pub wasm: CodemodeWasmModule,
    pub max_stack_size: Option<i64>,
    pub interrupt_budget: Option<usize>,
    pub tz: Option<String>,
    pub post: Arc<dyn Fn(WorkerToHostMessage) + Send + Sync>,
    pub should_interrupt: Arc<dyn Fn() -> bool + Send + Sync>,
}

struct VirtualClock {
    creating: AtomicBool,
    started_at: f64,
    now_bits: AtomicU64,
    seed: u32,
}

impl VirtualClock {
    fn new(started_at: f64, seed: u32) -> Arc<Self> {
        Arc::new(Self {
            creating: AtomicBool::new(true),
            started_at,
            now_bits: AtomicU64::new(started_at.to_bits()),
            seed,
        })
    }

    fn set_now_monotonic(&self, now: f64) {
        if !now.is_finite() {
            return;
        }
        let mut cur_bits = self.now_bits.load(Ordering::Acquire);
        loop {
            let cur = f64::from_bits(cur_bits);
            if now <= cur {
                break;
            }
            match self
                .now_bits
                .compare_exchange_weak(cur_bits, now.to_bits(), Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(actual) => cur_bits = actual,
            }
        }
    }

    fn now(&self) -> f64 {
        f64::from_bits(self.now_bits.load(Ordering::Acquire))
    }
}

/// Exact JS `Math.round(x)` semantics for finite numbers.
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// Discards guest stdout/stderr writes and reports every byte written so libc does not retry.
pub(crate) fn discard_output() -> WasiOptions {
    Arc::new(|_memory| WasiOverrides {
        fd_write: Some(Arc::new(
            |memory: &WasiMemory, _fd: i64, iovs_ptr: i64, iovs_len: i64, nwritten_ptr: i64| {
                let mut written: i64 = 0;
                for i in 0..iovs_len {
                    let len = memory.read_u32_le(iovs_ptr + i * 8 + 4);
                    written += len;
                }
                let Ok(nwritten_offset) = u32::try_from(nwritten_ptr) else {
                    return 28; // WASI_ERRNO_INVAL
                };
                if !memory.write_bytes(nwritten_offset, &(written as u32).to_le_bytes()) {
                    return 28; // WASI_ERRNO_INVAL
                }
                0
            },
        )),
        ..Default::default()
    })
}

fn deterministic_wasi(clock: Arc<VirtualClock>) -> WasiOptions {
    let prng_state = Arc::new(Mutex::new(clock.seed ^ 0x9e37_79b9));
    Arc::new(move |memory| {
        let mut overrides = discard_output()(memory);
        let clock_for_time = Arc::clone(&clock);
        overrides.clock_time_get = Some(Arc::new(
            move |mem: &WasiMemory, clock_id: i32, _precision: i64, result_ptr: i32| {
                let nanoseconds: u64 = if clock_for_time.creating.load(Ordering::Acquire) {
                    if clock_id == 0 {
                        // QuickJS seeds Math.random() from this reading in microseconds and turns 0 into 1.
                        u64::from(clock_for_time.seed).saturating_add(1).saturating_mul(1_000)
                    } else {
                        0
                    }
                } else {
                    let ms = if clock_id == 0 {
                        js_round(clock_for_time.now())
                    } else {
                        js_round(clock_for_time.now() - clock_for_time.started_at)
                    };
                    (ms.max(0.0) as u64).saturating_mul(1_000_000)
                };
                mem.write_u64_le(i64::from(result_ptr), nanoseconds);
                0
            },
        ));
        let prng_for_random = Arc::clone(&prng_state);
        overrides.random_get = Some(Arc::new(move |mem: &WasiMemory, buf_ptr: i32, buf_len: i32| {
            let Ok(ptr) = u32::try_from(buf_ptr) else {
                return 28; // WASI_ERRNO_INVAL
            };
            let Ok(len) = usize::try_from(buf_len) else {
                return 28;
            };
            let mut state = prng_for_random.lock().unwrap_or_else(|e| e.into_inner());
            let mut bytes = Vec::with_capacity(len);
            let mut offset = 0;
            while offset < len {
                *state = state.wrapping_add(0x6d2b_79f5);
                let mut value = (*state ^ (*state >> 15)).wrapping_mul(1 | *state);
                value = value.wrapping_add((value ^ (value >> 7)).wrapping_mul(61 | value)) ^ value;
                let draw = value ^ (value >> 14);
                for b in 0..4 {
                    if offset + b < len {
                        bytes.push((draw >> (b * 8)) as u8);
                    }
                }
                offset += 4;
            }
            if mem.write_bytes(ptr, &bytes) { 0 } else { 28 }
        }));
        overrides
    })
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

fn crash_message(error: &pi_js::Error) -> WorkerToHostMessage {
    let message = if let Some(jse) = error.downcast_ref::<JSException>() {
        format!("{}: {}", jse.name(), jse.message())
    } else {
        error.message().to_string()
    };
    WorkerToHostMessage::Crash { message }
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

struct VmDeliveryState {
    disposed: bool,
    next_seq: i64,
    early: IndexMap<i64, HostToWorkerMessage>,
}

/// Owns a live QuickJS VM running a single codemode script.
pub struct VmSession {
    vm: QuickJS,
    handles: Mutex<Option<VmHandles>>,
    clock: Option<Arc<VirtualClock>>,
    issued: Arc<AtomicI64>,
    current: Arc<AtomicI64>,
    delivery: Mutex<VmDeliveryState>,
    post: Arc<dyn Fn(WorkerToHostMessage) + Send + Sync>,
    should_interrupt: Arc<dyn Fn() -> bool + Send + Sync>,
}

#[derive(Clone)]
struct VmHandles {
    api: JSValueHandle,
    settle: JSValueHandle,
    stalled: JSValueHandle,
    bridge: JSValueHandle,
}

impl VmHandles {
    fn dispose(self) {
        self.stalled.dispose();
        self.settle.dispose();
        self.bridge.dispose();
        self.api.dispose();
    }
}

impl Drop for VmSession {
    fn drop(&mut self) {
        self.dispose();
    }
}

impl VmSession {
    /// Hands the VM a tool result and runs the script until it waits again.
    pub fn deliver(&self, message: HostToWorkerMessage) {
        let mut guard = self.delivery.lock().unwrap_or_else(|e| e.into_inner());
        if guard.disposed {
            return;
        }
        let HostToWorkerMessage::Result { id, seq, .. } = &message;
        let id = *id;
        let Some(ref clock) = self.clock else {
            drop(guard);
            if let Err(err) = self.apply(&message) {
                (self.post)(crash_message(&err));
            }
            return;
        };

        let Some(seq_val) = *seq else {
            let expected = guard.next_seq;
            drop(guard);
            (self.post)(WorkerToHostMessage::Crash {
                message: format!(
                    "Error: Result {id} has sequence number undefined, expected {expected} or later, once"
                ),
            });
            return;
        };
        if seq_val < guard.next_seq || guard.early.contains_key(&seq_val) {
            let expected = guard.next_seq;
            drop(guard);
            (self.post)(WorkerToHostMessage::Crash {
                message: format!(
                    "Error: Result {id} has sequence number {seq_val}, expected {expected} or later, once"
                ),
            });
            return;
        }
        guard.early.insert(seq_val, message);
        loop {
            let next_seq = guard.next_seq;
            let Some(next) = guard.early.shift_remove(&next_seq) else {
                break;
            };
            drop(guard);
            if let HostToWorkerMessage::Result { now: Some(now), .. } = &next {
                clock.set_now_monotonic(*now);
            }
            self.current.store(next_seq, Ordering::Release);
            if let Err(err) = self.apply(&next) {
                (self.post)(crash_message(&err));
                return;
            }
            (self.post)(WorkerToHostMessage::Idle {
                seq: next_seq,
                issued: self.issued.load(Ordering::Acquire),
            });
            guard = self.delivery.lock().unwrap_or_else(|e| e.into_inner());
            if guard.disposed {
                return;
            }
            guard.next_seq += 1;
        }
    }

    /// Releases the VM's wasm instance. Safe to call more than once.
    pub fn dispose(&self) {
        let mut guard = self.delivery.lock().unwrap_or_else(|e| e.into_inner());
        if guard.disposed {
            return;
        }
        guard.disposed = true;
        guard.early.clear();
        drop(guard);
        let handles = self.handles.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = handles {
            h.dispose();
        }
        self.vm.dispose();
    }

    fn apply(&self, message: &HostToWorkerMessage) -> pi_js::Result<()> {
        let Some(handles) = self.handles.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return Ok(());
        };
        let HostToWorkerMessage::Result {
            id,
            ok,
            payload,
            properties,
            ..
        } = message;
        self.vm.with_scope(|_| {
            let id_h = self.vm.new_number(*id as f64)?;
            let ok_h = self.vm.new_boolean(*ok)?;
            let payload_h = match payload {
                Some(s) => self.vm.new_string(s)?,
                None => self.vm.undefined()?,
            };
            let props_h = match properties {
                Some(s) => self.vm.new_string(s)?,
                None => self.vm.undefined()?,
            };
            self.vm
                .call_function(&handles.settle, &handles.api, &[id_h, ok_h, payload_h, props_h])?;
            Ok::<(), pi_js::Error>(())
        })?;
        drain(&self.vm, &handles.stalled, &handles.api, &self.should_interrupt)
    }
}

fn drain(
    vm: &QuickJS,
    stalled: &JSValueHandle,
    api: &JSValueHandle,
    should_interrupt: &Arc<dyn Fn() -> bool + Send + Sync>,
) -> pi_js::Result<()> {
    let mut jobs: usize = 0;
    while vm.execute_pending_job_step()? {
        jobs += 1;
        if jobs.is_multiple_of(JOBS_PER_POLL) && should_interrupt() {
            return Err(pi_js::Error::msg("Error: Execution interrupted"));
        }
    }
    vm.call_function(stalled, api, &[])?.dispose();
    Ok(())
}

/// Creates a fresh QuickJS VM, compiles the prelude and script, and starts execution.
///
/// Only returns `Err` when VM creation itself fails. Any failure afterwards is reported as a
/// `crash` message via `options.post`.
pub fn start_vm(options: StartVmOptions) -> pi_js::Result<VmSession> {
    let max_stack_size = options.max_stack_size.unwrap_or(MAX_STACK_SIZE);
    let clock = options
        .data
        .deterministic
        .as_ref()
        .map(|det| VirtualClock::new(det.started_at, det.seed));
    let (intrinsics, timezone_offset, wasi) = match &clock {
        Some(clk) => (
            Intrinsics::ALL & !Intrinsics::WEAK_REF,
            Some(TimezoneOffset::FixedMinutes(0)),
            Some(deterministic_wasi(Arc::clone(clk))),
        ),
        None => (
            Intrinsics::ALL,
            Some(TimezoneOffset::Host { tz: options.tz.clone() }),
            Some(discard_output()),
        ),
    };

    let vm = QuickJS::create_with_settings(
        QuickJSOptions {
            wasm: options.wasm,
            memory_limit: options.data.memory_limit_bytes.filter(|v| v.is_finite() && *v > 0.0),
            max_stack_size: Some(max_stack_size),
            interrupt_handler: Some(Arc::clone(&options.should_interrupt)),
            wasi,
        },
        intrinsics,
        timezone_offset,
    )?;
    if let Some(ref clk) = clock {
        clk.creating.store(false, Ordering::Release);
    }

    let issued = Arc::new(AtomicI64::new(0));
    let current = Arc::new(AtomicI64::new(0));

    let bridge_post = Arc::clone(&options.post);
    let bridge_vm = vm.clone();
    let bridge_issued = Arc::clone(&issued);
    let bridge_current = Arc::clone(&current);
    let is_det = clock.is_some();

    let bridge_res = vm.new_function(
        "bridge",
        Arc::new(move |_this, args| {
            let Some(tag_h) = args.first() else {
                return bridge_vm.undefined();
            };
            let tag = tag_h.to_string()?;
            match tag.as_str() {
                "call" | "global" => {
                    bridge_issued.fetch_add(1, Ordering::AcqRel);
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
                    (bridge_post)(WorkerToHostMessage::Call {
                        id,
                        target,
                        name,
                        args: args_json,
                    });
                }
                "output" => {
                    let kind = args.get(1).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                    match kind.as_str() {
                        "image" => {
                            let data = args.get(2).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                            let mime_type = args.get(3).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                            (bridge_post)(WorkerToHostMessage::Output {
                                item: CodemodeOutputItem::Image { data, mime_type },
                                console: false,
                            });
                        }
                        "console" => {
                            let text = args.get(2).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                            (bridge_post)(WorkerToHostMessage::Output {
                                item: CodemodeOutputItem::Text { text },
                                console: true,
                            });
                        }
                        _ => {
                            let text = args.get(2).map(|h| h.to_string()).transpose()?.unwrap_or_default();
                            (bridge_post)(WorkerToHostMessage::Output {
                                item: CodemodeOutputItem::Text { text },
                                console: false,
                            });
                        }
                    }
                }
                "done" => {
                    let during = is_det.then(|| CodemodeDuring {
                        seq: bridge_current.load(Ordering::Acquire),
                        issued: bridge_issued.load(Ordering::Acquire),
                    });
                    let ok = args.get(1).map(|h| h.to_boolean()).transpose()?.unwrap_or(false);
                    let payload = match args.get(2) {
                        Some(h) if !h.is_undefined() => Some(h.to_string()?),
                        _ => None,
                    };
                    let writes = match args.get(3) {
                        Some(h) if !h.is_undefined() => Some(h.to_string()?),
                        _ => None,
                    };
                    if ok {
                        (bridge_post)(WorkerToHostMessage::Done {
                            ok: true,
                            value: payload,
                            writes: Some(writes.unwrap_or_default()),
                            error: None,
                            during,
                        });
                    } else {
                        (bridge_post)(WorkerToHostMessage::Done {
                            ok: false,
                            value: None,
                            writes: None,
                            error: Some(payload.unwrap_or_default()),
                            during,
                        });
                    }
                }
                _ => {}
            }
            bridge_vm.undefined()
        }),
    );

    let init_res: pi_js::Result<(VmHandles, Option<JSValueHandle>, Option<String>)> = (|| {
        let bridge = bridge_res?;
        let api = vm
            .with_scope(|scope| {
                let factory = vm.eval_code(PRELUDE_SOURCE, Some("codemode-prelude.js"), None)?;
                let tools_json = pi_js::json::stringify(&options.data.tools);
                let globals_json = pi_js::json::stringify(&options.data.globals);
                let store_json = pi_js::json::stringify(&options.data.store);
                let undef = vm.undefined()?;
                let api = vm.call_function(
                    &factory,
                    &undef,
                    &[
                        bridge.clone(),
                        vm.new_string(&tools_json)?,
                        vm.new_string(&globals_json)?,
                        vm.new_string(&store_json)?,
                    ],
                )?;
                Ok(scope.escape(api))
            })
            .map_err(map_js_exception)?;
        let settle = api.get_prop("settle")?;
        let run = api.get_prop("run")?;
        let stalled = api.get_prop("stalled")?;

        let fingerprint = if clock.is_some() {
            let versions = vm.versions()?;
            let qw = versions.get("quickjs-wasi").map(String::as_str).unwrap_or("");
            let qj = versions.get("quickjs").map(String::as_str).unwrap_or("");
            let heap = options
                .data
                .memory_limit_bytes
                .filter(|v| v.is_finite())
                .map(pi_js::json::number_to_string)
                .unwrap_or_else(|| "none".to_string());
            let budget = options
                .interrupt_budget
                .map(|b| b.to_string())
                .unwrap_or_else(|| "none".to_string());
            Some(format!(
                "quickjs-wasi {qw}, quickjs {qj}, prelude {}, deterministic {DETERMINISTIC_VERSION}, stack {max_stack_size}, heap {heap}, budget {budget}",
                *PRELUDE_HASH
            ))
        } else {
            None
        };

        Ok((
            VmHandles {
                api,
                settle,
                stalled,
                bridge,
            },
            Some(run),
            fingerprint,
        ))
    })();

    let (handles, run_handle, fingerprint) = match init_res {
        Ok(parts) => parts,
        Err(err) => {
            (options.post)(crash_message(&err));
            return Ok(VmSession {
                vm,
                handles: Mutex::new(None),
                clock,
                issued,
                current,
                delivery: Mutex::new(VmDeliveryState {
                    disposed: false,
                    next_seq: 1,
                    early: IndexMap::new(),
                }),
                post: options.post,
                should_interrupt: options.should_interrupt,
            });
        }
    };

    if let Some(fp) = fingerprint {
        (options.post)(WorkerToHostMessage::Started { fingerprint: fp });
    }

    // The prefix shares the first line with the script so reported line numbers
    // match the script as written.
    let wrapped = format!("(async (tools, console) => {{{}\n}})", options.data.code);
    let script_fn = match vm.eval_code(&wrapped, Some("codemode.js"), None) {
        Ok(fn_h) => fn_h,
        Err(err) => {
            if let Some(run_h) = run_handle {
                run_h.dispose();
            }
            if let Some(jse) = err.downcast_ref::<JSException>() {
                (options.post)(WorkerToHostMessage::Done {
                    ok: false,
                    value: None,
                    writes: None,
                    error: Some(describe_exception(jse)),
                    during: None,
                });
            } else {
                (options.post)(crash_message(&err));
            }
            handles.dispose();
            return Ok(VmSession {
                vm,
                handles: Mutex::new(None),
                clock,
                issued,
                current,
                delivery: Mutex::new(VmDeliveryState {
                    disposed: false,
                    next_seq: 1,
                    early: IndexMap::new(),
                }),
                post: options.post,
                should_interrupt: options.should_interrupt,
            });
        }
    };

    let run_fn = run_handle.unwrap();
    let run_res = (|| -> pi_js::Result<()> {
        vm.call_function(&run_fn, &handles.api, &[script_fn.clone()])?.dispose();
        Ok(())
    })();
    script_fn.dispose();
    run_fn.dispose();

    let session = VmSession {
        vm,
        handles: Mutex::new(Some(handles)),
        clock: clock.clone(),
        issued: Arc::clone(&issued),
        current,
        delivery: Mutex::new(VmDeliveryState {
            disposed: false,
            next_seq: 1,
            early: IndexMap::new(),
        }),
        post: Arc::clone(&options.post),
        should_interrupt: options.should_interrupt,
    };

    if let Err(err) = run_res {
        (options.post)(crash_message(&err));
        return Ok(session);
    }

    let drain_handles = session.handles.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let drain_res = drain_handles.as_ref().map_or(Ok(()), |h| {
        drain(&session.vm, &h.stalled, &h.api, &session.should_interrupt)
    });
    if let Err(err) = drain_res {
        (options.post)(crash_message(&err));
        return Ok(session);
    }

    if clock.is_some() {
        (options.post)(WorkerToHostMessage::Idle {
            seq: 0,
            issued: issued.load(Ordering::Acquire),
        });
    }

    Ok(session)
}

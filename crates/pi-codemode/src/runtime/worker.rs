//! Port of packages/codemode/src/runtime/worker.ts

#![allow(dead_code, unused_variables)]

use std::sync::mpsc::{Receiver, Sender};

use crate::runtime::protocol::{HostToWorkerMessage, WorkerData, WorkerToHostMessage};
use crate::vendor::quickjs_wasi::{JSException, WasiOptions};

// PORT: body of a dedicated `std::thread` (PORTING.md §2.6). The TS module is a Node worker
// entry (`parentPort` / `workerData`) that starts on import. The host spawns the thread;
// `incoming` is `parentPort.on("message")` and `outgoing` is `parentPort.postMessage`.
// Returning from [`run`] is the worker `exit`.

/// Runs one script on the calling thread. Ports `main` in `worker.ts`.
pub(crate) fn run(data: WorkerData, incoming: Receiver<HostToWorkerMessage>, outgoing: Sender<WorkerToHostMessage>) {
    todo!("port: run")
}

fn post(outgoing: &Sender<WorkerToHostMessage>, message: WorkerToHostMessage) {
    todo!("port: post")
}

fn crash(outgoing: &Sender<WorkerToHostMessage>, error: &pi_js::Error) {
    todo!("port: crash")
}

/// QuickJS writes engine diagnostics to fd 1 and 2, which the default shim
/// forwards to the host's stdout and stderr. That output belongs to the host
/// application (for example a TUI), so it is discarded. Reporting every byte as
/// written keeps libc from retrying.
fn discard_output() -> WasiOptions {
    todo!("port: discard_output")
}

fn describe_exception(error: &JSException) -> String {
    todo!("port: describe_exception")
}

//! Port of packages/codemode/src/runtime/worker.ts

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender};

use crate::runtime::protocol::{HostToWorkerMessage, VmStartData, WorkerData, WorkerToHostMessage};
use crate::runtime::vm::{StartVmOptions, start_vm};

/// Worker thread entry point. Runs one script in a fresh QuickJS VM and relays messages until the
/// host drops the channel or requests interruption.
pub(crate) fn run(data: WorkerData, incoming: Receiver<HostToWorkerMessage>, outgoing: Sender<WorkerToHostMessage>) {
    let interrupt = Arc::clone(&data.interrupt);
    let post_tx = outgoing.clone();
    let post = Arc::new(move |msg: WorkerToHostMessage| {
        let _ = post_tx.send(msg);
    });
    let interrupt_cb_flag = Arc::clone(&interrupt);
    let should_interrupt = Arc::new(move || interrupt_cb_flag.load(Ordering::Acquire) != 0);

    let start_data = VmStartData::from(&data);
    let session = match start_vm(StartVmOptions {
        data: start_data,
        wasm: data.wasm,
        max_stack_size: None,
        interrupt_budget: None,
        tz: data.tz,
        post,
        should_interrupt,
    }) {
        Ok(session) => session,
        Err(err) => {
            let _ = outgoing.send(WorkerToHostMessage::Crash {
                message: err.message().to_string(),
            });
            return;
        }
    };

    while interrupt.load(Ordering::Acquire) == 0 {
        let Ok(msg) = incoming.recv() else {
            break;
        };
        if interrupt.load(Ordering::Acquire) != 0 {
            break;
        }
        session.deliver(msg);
    }
}

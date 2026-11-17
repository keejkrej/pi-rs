//! Port of packages/coding-agent/src/core/output-guard.ts

use std::sync::{Arc, LazyLock, Mutex};

use pi_js::Result;

struct StdoutTakeoverState {
    taken_over: bool,
}

static STATE: LazyLock<Mutex<StdoutTakeoverState>> =
    LazyLock::new(|| Mutex::new(StdoutTakeoverState { taken_over: false }));
static WRITE_SERIALIZER: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
static PENDING_WRITES: LazyLock<Arc<Mutex<Vec<String>>>> = LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

pub fn take_over_stdout() {
    if let Ok(mut guard) = STATE.lock() {
        guard.taken_over = true;
    }
    pi_js::console::set_stdout_redirect_to_stderr(true);
}

pub fn restore_stdout() {
    if let Ok(mut guard) = STATE.lock() {
        guard.taken_over = false;
    }
    pi_js::console::set_stdout_redirect_to_stderr(false);
}

pub fn is_stdout_taken_over() -> bool {
    STATE.lock().map(|g| g.taken_over).unwrap_or(false)
}

pub fn write_raw_stdout(text: &str) {
    if text.is_empty() {
        return;
    }
    if let Ok(mut q) = PENDING_WRITES.lock() {
        q.push(text.to_owned());
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        let text_owned = text.to_owned();
        tokio::spawn(async move {
            let _guard = WRITE_SERIALIZER.lock().await;
            pi_js::console::raw_stdout_write(&text_owned);
        });
    } else {
        pi_js::console::raw_stdout_write(text);
    }
}

pub async fn wait_for_raw_stdout_backpressure() {
    let _guard = WRITE_SERIALIZER.lock().await;
    if let Ok(mut q) = PENDING_WRITES.lock() {
        q.clear();
    }
}

pub async fn flush_raw_stdout() -> Result<()> {
    wait_for_raw_stdout_backpressure().await;
    Ok(())
}

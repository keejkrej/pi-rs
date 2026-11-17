//! Port of packages/coding-agent/src/modes/interactive/components/countdown-timer.ts

use std::sync::{Arc, Mutex};

use pi_js::time::{Interval, set_interval};
use pi_tui::TUI;

struct CountdownTimerState {
    interval: Option<Interval>,
    remaining_seconds: i64,
}

struct CountdownTimerInner {
    state: Mutex<CountdownTimerState>,
}

impl Drop for CountdownTimerInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.state.lock()
            && let Some(interval) = guard.interval.take()
        {
            interval.clear();
        }
    }
}

/// Reusable countdown timer for dialog components.
#[derive(Clone)]
pub struct CountdownTimer {
    inner: Arc<CountdownTimerInner>,
}

impl CountdownTimer {
    pub fn new(
        timeout_ms: i64,
        tui: Option<Arc<dyn TUI + Send + Sync>>,
        on_tick: Arc<dyn Fn(i64) + Send + Sync>,
        on_expire: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let remaining_seconds = ((timeout_ms as f64) / 1000.0).ceil() as i64;
        on_tick(remaining_seconds);

        let inner = Arc::new(CountdownTimerInner {
            state: Mutex::new(CountdownTimerState {
                interval: None,
                remaining_seconds,
            }),
        });

        let weak = Arc::downgrade(&inner);
        let interval = set_interval(1000, move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let (remaining, expired_interval) = {
                let mut state = inner.state.lock().unwrap();
                state.remaining_seconds -= 1;
                let remaining = state.remaining_seconds;
                let expired = if remaining <= 0 { state.interval.take() } else { None };
                (remaining, expired)
            };
            if let Some(interval) = expired_interval {
                interval.clear();
            }
            on_tick(remaining);
            if let Some(ref ui) = tui {
                ui.request_render(None);
            }
            if remaining <= 0 {
                on_expire();
            }
        });

        inner.state.lock().unwrap().interval = Some(interval);
        Self { inner }
    }

    pub fn dispose(&self) {
        let interval = self.inner.state.lock().unwrap().interval.take();
        if let Some(interval) = interval {
            interval.clear();
        }
    }
}

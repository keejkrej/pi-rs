//! Port of packages/ai/src/utils/abort-signals.ts

use pi_js::abort::AbortSignal;

#[derive(Clone, Debug, Default)]
pub struct CombinedAbortSignal {
    pub signal: Option<AbortSignal>,
}

impl CombinedAbortSignal {
    /// Mirror TS `CombinedAbortSignal.cleanup()` (no-op in Rust since `AbortSignal::any` holds weak dependents).
    pub fn cleanup(&self) {}
}

pub fn combine_abort_signals<'a>(signals: impl IntoIterator<Item = Option<&'a AbortSignal>>) -> CombinedAbortSignal {
    let active: Vec<AbortSignal> = signals.into_iter().flatten().cloned().collect();
    match active.len() {
        0 => CombinedAbortSignal { signal: None },
        1 => CombinedAbortSignal {
            signal: active.into_iter().next(),
        },
        _ => CombinedAbortSignal {
            signal: Some(AbortSignal::any(&active)),
        },
    }
}

//! Port of packages/ai/src/utils/sleep.ts

use pi_js::Result;
use pi_js::abort::AbortSignal;

pub async fn sleep(ms: u64, signal: &AbortSignal) -> Result<()> {
    pi_js::time::sleep(ms, Some(signal)).await
}

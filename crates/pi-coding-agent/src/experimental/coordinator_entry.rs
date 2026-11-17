//! Port of packages/coding-agent/src/experimental/coordinator-entry.ts

use crate::experimental::coordinator::run_coordinator_process;
use crate::experimental::process::{InternalProcessRole, consume_internal_process_role};

pub async fn run_coordinator_entry(args: &[String]) -> pi_js::Result<()> {
    let role = consume_internal_process_role()?;
    if role != Some(InternalProcessRole::Coordinator) {
        return Err(pi_js::Error::msg(
            "Coordinator entrypoint requires an internal coordinator invocation",
        ));
    }
    run_coordinator_process(args).await
}

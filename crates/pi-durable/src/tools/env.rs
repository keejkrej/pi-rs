//! Port of packages/durable/src/tools/env.ts

use std::sync::Arc;

use crate::env::ExecutionEnv;
use crate::harness::types::ToolExecutionApi;

/// The call's execution environment; a tool without one fails with an ordinary error result.
pub fn require_env(api: &ToolExecutionApi) -> pi_js::Result<Arc<dyn ExecutionEnv>> {
    api.env()
        .ok_or_else(|| pi_js::Error::msg("No execution environment is configured"))
}

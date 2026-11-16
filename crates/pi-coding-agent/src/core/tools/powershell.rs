//! Port of packages/coding-agent/src/core/tools/powershell.ts

use std::sync::Arc;

use super::bash::{BashOperations, BashSpawnContext, BashSpawnHook, BashToolDetails, BashToolInput};

pub type PowerShellOperations = dyn BashOperations;
pub type PowerShellSpawnContext = BashSpawnContext;
pub type PowerShellSpawnHook = BashSpawnHook;
pub type PowerShellToolDetails = BashToolDetails;
pub type PowerShellToolInput = BashToolInput;

#[derive(Clone, Default)]
pub struct PowerShellToolOptions {
    pub operations: Option<Arc<dyn BashOperations>>,
    pub expose_session_environment: Option<bool>,
    pub spawn_hook: Option<BashSpawnHook>,
}

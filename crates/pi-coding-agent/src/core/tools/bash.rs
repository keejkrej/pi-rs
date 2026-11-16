//! Port of packages/coding-agent/src/core/tools/bash.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;
use serde::{Deserialize, Serialize};

use super::truncate::TruncationResult;

pub const DEFAULT_SHELL_MAX_OUTPUT_BYTES: usize = 50 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolInput {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BashToolOutput {
    pub output: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    pub exit_code: i64,
    pub wall_time_seconds: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

pub type BashChunkCallback = Arc<dyn Fn(&[u8]) + Send + Sync>;

#[derive(Clone, Default)]
pub struct BashExecOptions {
    pub on_data: Option<BashChunkCallback>,
    pub signal: Option<AbortSignal>,
    pub timeout: Option<f64>,
    pub env: Option<IndexMap<String, String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BashExecResult {
    pub exit_code: Option<i64>,
}

/// Pluggable operations for the bash tool.
/// Override these to delegate command execution to remote systems (for example SSH).
pub trait BashOperations: Send + Sync {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions,
    ) -> BoxFuture<pi_js::Result<BashExecResult>>;
}

#[derive(Clone, Debug, Default)]
pub struct BashSpawnContext {
    pub command: String,
    pub cwd: String,
    pub env: IndexMap<String, String>,
}

pub type BashSpawnHook = Arc<dyn Fn(BashSpawnContext) -> BashSpawnContext + Send + Sync>;

#[derive(Clone, Default)]
pub struct BashToolOptions {
    pub operations: Option<Arc<dyn BashOperations>>,
    pub command_prefix: Option<String>,
    pub spawn_hook: Option<BashSpawnHook>,
    pub expose_session_environment: Option<bool>,
}

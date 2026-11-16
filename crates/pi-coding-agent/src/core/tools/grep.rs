//! Port of packages/coding-agent/src/core/tools/grep.ts

use std::sync::Arc;

use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

use super::truncate::TruncationResult;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolInput {
    pub pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore_case: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub literal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_limit_reached: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_truncated: Option<bool>,
}

/// Pluggable operations for the grep tool.
pub trait GrepOperations: Send + Sync {
    fn is_directory(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<bool>>;
    fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<String>>;
}

#[derive(Clone, Default)]
pub struct GrepToolOptions {
    pub operations: Option<Arc<dyn GrepOperations>>,
}

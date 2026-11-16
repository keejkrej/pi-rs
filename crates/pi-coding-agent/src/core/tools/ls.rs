//! Port of packages/coding-agent/src/core/tools/ls.ts

use std::sync::Arc;

use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

use super::truncate::TruncationResult;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_limit_reached: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LsStat {
    pub is_directory: bool,
}

/// Pluggable operations for the ls tool.
pub trait LsOperations: Send + Sync {
    fn exists(&self, absolute_path: &str) -> BoxFuture<bool>;
    fn stat(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<LsStat>>;
    fn readdir(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<String>>>;
}

#[derive(Clone, Default)]
pub struct LsToolOptions {
    pub operations: Option<Arc<dyn LsOperations>>,
}

//! Port of packages/coding-agent/src/core/tools/find.ts

use std::sync::Arc;

use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

use super::truncate::TruncationResult;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolInput {
    pub pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_limit_reached: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct FindGlobOptions {
    pub ignore: Vec<String>,
    pub limit: usize,
}

/// Pluggable operations for the find tool.
pub trait FindOperations: Send + Sync {
    fn exists(&self, absolute_path: &str) -> BoxFuture<bool>;
    fn glob(&self, pattern: &str, cwd: &str, options: FindGlobOptions) -> BoxFuture<pi_js::Result<Vec<String>>>;
}

#[derive(Clone, Default)]
pub struct FindToolOptions {
    pub operations: Option<Arc<dyn FindOperations>>,
}

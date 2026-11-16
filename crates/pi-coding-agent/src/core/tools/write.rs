//! Port of packages/coding-agent/src/core/tools/write.ts

use std::sync::Arc;

use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteToolInput {
    pub path: String,
    pub content: String,
}

/// Pluggable operations for the write tool.
pub trait WriteOperations: Send + Sync {
    fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>>;
    fn mkdir(&self, dir: &str) -> BoxFuture<pi_js::Result<()>>;
}

#[derive(Clone, Default)]
pub struct WriteToolOptions {
    pub operations: Option<Arc<dyn WriteOperations>>,
}

//! Port of packages/coding-agent/src/core/tools/edit.ts

use std::sync::Arc;

use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditReplacement {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolInput {
    pub path: String,
    pub edits: Vec<EditReplacement>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolDetails {
    /// Display-oriented diff of the changes made
    pub diff: String,
    /// Standard unified patch of the changes made
    pub patch: String,
    /// Line number of the first change in the new file (for editor navigation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<i64>,
}

/// Pluggable operations for the edit tool.
pub trait EditOperations: Send + Sync {
    fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<u8>>>;
    fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>>;
    fn access(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<()>>;
}

#[derive(Clone, Default)]
pub struct EditToolOptions {
    pub operations: Option<Arc<dyn EditOperations>>,
}

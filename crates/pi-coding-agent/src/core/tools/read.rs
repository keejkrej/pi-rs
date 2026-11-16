//! Port of packages/coding-agent/src/core/tools/read.ts

use std::sync::Arc;

use pi_ai::ModelImageResizeOptions;
use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

use super::truncate::TruncationResult;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolInput {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
}

/// Pluggable operations for the read tool.
pub trait ReadOperations: Send + Sync {
    fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<u8>>>;
    fn access(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<()>>;
    fn detect_image_mime_type(&self, _absolute_path: &str) -> BoxFuture<pi_js::Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Clone, Default)]
pub struct ReadToolOptions {
    pub auto_resize_images: Option<bool>,
    pub resize_options: Option<ModelImageResizeOptions>,
    pub operations: Option<Arc<dyn ReadOperations>>,
}

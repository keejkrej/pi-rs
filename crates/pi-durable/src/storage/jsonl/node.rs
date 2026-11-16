//! Port of packages/durable/src/storage/jsonl/node.ts

use std::sync::Arc;

use pi_chord::Context;

use crate::env::node::{NodeExecutionEnv, NodeExecutionEnvOptions};

pub use crate::storage::jsonl::storage::{
    JsonlCorruptionError, JsonlStorage, JsonlStorageOptions, JsonlStoragePoisonedError,
};

/// Open or create a JSONL storage directory using the local filesystem.
pub async fn open_node_jsonl_storage(
    directory: &str,
    context: Context,
    options: JsonlStorageOptions,
) -> pi_js::Result<JsonlStorage> {
    let env = Arc::new(NodeExecutionEnv::new(NodeExecutionEnvOptions {
        cwd: pi_js::env::cwd(),
        ..Default::default()
    }));
    JsonlStorage::open(directory, env, context, options).await
}

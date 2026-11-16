//! Port of packages/coding-agent/src/core/tools/file-mutation-queue.ts

use std::future::Future;

pub async fn with_file_mutation_queue<T, F, Fut>(_file_path: &str, fn_: F) -> pi_js::Result<T>
where
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = pi_js::Result<T>> + Send,
{
    fn_().await
}

//! Port of packages/coding-agent/test/grep-operations.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_ai::{TextContent, UserContentBlock};
use pi_coding_agent::core::tools::grep::{GrepOperations, GrepToolOptions, create_grep_tool};
use pi_js::BoxFuture;
use serde_json::json;
use tempfile::TempDir;

struct LegacyReadFileOps {
    reads: Arc<AtomicUsize>,
}

impl GrepOperations for LegacyReadFileOps {
    fn is_directory(&self, _absolute_path: &str) -> BoxFuture<pi_js::Result<bool>> {
        Box::pin(async { Ok(true) })
    }

    fn read_file(&self, _absolute_path: &str) -> BoxFuture<pi_js::Result<String>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok("custom context".to_owned()) })
    }
}

struct NoReadFileOps;

impl GrepOperations for NoReadFileOps {
    fn is_directory(&self, _absolute_path: &str) -> BoxFuture<pi_js::Result<bool>> {
        Box::pin(async { Ok(true) })
    }
}

#[tokio::test]
async fn accepts_deprecated_read_file_hook_without_calling_it_9276() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    std::fs::write(tmp.path().join("file.txt"), "before\nneedle\nafter\n").unwrap();

    let reads = Arc::new(AtomicUsize::new(0));
    let tool = create_grep_tool(
        &cwd,
        Some(GrepToolOptions {
            operations: Some(Arc::new(LegacyReadFileOps {
                reads: Arc::clone(&reads),
            })),
        }),
    );

    let result = (tool.execute)(
        "grep".to_owned(),
        json!({ "pattern": "needle", "context": 1 }),
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        result.content,
        vec![UserContentBlock::Text(TextContent::new(
            "file.txt-1- before\nfile.txt:2: needle\nfile.txt-3- after"
        ))]
    );
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn allows_custom_operations_without_a_read_file_hook() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    std::fs::write(tmp.path().join("file.txt"), "before\nneedle\nafter\n").unwrap();

    let tool = create_grep_tool(
        &cwd,
        Some(GrepToolOptions {
            operations: Some(Arc::new(NoReadFileOps)),
        }),
    );

    let result = (tool.execute)(
        "grep".to_owned(),
        json!({ "pattern": "needle", "context": 1 }),
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        result.content,
        vec![UserContentBlock::Text(TextContent::new(
            "file.txt-1- before\nfile.txt:2: needle\nfile.txt-3- after"
        ))]
    );
}

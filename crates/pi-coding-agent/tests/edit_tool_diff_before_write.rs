//! Port of packages/coding-agent/test/edit-tool-diff-before-write.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_coding_agent::core::tools::edit::{EditOperations, EditToolOptions, create_edit_tool, testing};
use pi_js::BoxFuture;
use serde_json::json;
use serial_test::serial;

struct MockEditOps {
    write_calls: Arc<AtomicUsize>,
}

impl EditOperations for MockEditOps {
    fn access(&self, _: &str) -> BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn read_file(&self, _: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
        Box::pin(async { Ok(b"a\nb\n".to_vec()) })
    }

    fn write_file(&self, _: &str, _: &str) -> BoxFuture<pi_js::Result<()>> {
        self.write_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
#[serial]
async fn computes_diff_before_writing_so_diff_failure_leaves_file_unchanged() {
    let _guard = testing::override_diff_error(|| Some("diff failed".to_string()));
    let write_calls = Arc::new(AtomicUsize::new(0));
    let tool = create_edit_tool(
        ".",
        Some(EditToolOptions {
            operations: Some(Arc::new(MockEditOps {
                write_calls: Arc::clone(&write_calls),
            })),
        }),
    );

    let err = (tool.execute)(
        "test-diff-before-write".into(),
        json!({
            "path": "f.txt",
            "edits": [{ "oldText": "b", "newText": "B" }]
        }),
        None,
        None,
    )
    .await
    .expect_err("expected diff error");
    assert!(err.to_string().contains("diff failed"), "err: {err}");
    assert_eq!(write_calls.load(Ordering::SeqCst), 0);
}

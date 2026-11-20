//! Port of packages/coding-agent/test/find-abort-before-spawn.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_coding_agent::core::tools::find::{FindGlobOptions, FindOperations, FindToolOptions, create_find_tool};
use pi_js::BoxFuture;
use pi_js::abort::AbortController;
use serde_json::json;

struct AbortingExistsOps {
    controller: Arc<AbortController>,
    glob_calls: Arc<AtomicUsize>,
}

impl FindOperations for AbortingExistsOps {
    fn exists(&self, _: &str) -> BoxFuture<bool> {
        self.controller.abort(None);
        Box::pin(async { true })
    }

    fn glob(&self, _: &str, _: &str, _: FindGlobOptions) -> BoxFuture<pi_js::Result<Vec<String>>> {
        self.glob_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[tokio::test]
async fn does_not_start_glob_after_abort_during_exists_check() {
    let temp = tempfile::tempdir().expect("tempdir");
    let controller = Arc::new(AbortController::new());
    let glob_calls = Arc::new(AtomicUsize::new(0));
    let tool = create_find_tool(
        temp.path().to_str().expect("utf8"),
        Some(FindToolOptions {
            operations: Some(Arc::new(AbortingExistsOps {
                controller: Arc::clone(&controller),
                glob_calls: Arc::clone(&glob_calls),
            })),
        }),
    );

    let err = (tool.execute)(
        "call-1".into(),
        json!({ "pattern": "*.ts" }),
        Some(controller.signal()),
        None,
    )
    .await
    .expect_err("expected abort error");
    assert!(err.to_string().contains("Operation aborted"), "err: {err}");
    assert_eq!(glob_calls.load(Ordering::SeqCst), 0);
}

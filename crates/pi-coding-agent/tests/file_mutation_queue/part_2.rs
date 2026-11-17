//! Port of packages/coding-agent/test/file-mutation-queue.test.ts#L101-274

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pi_coding_agent::core::tools::edit::{EditOperations, EditToolOptions, create_edit_tool};
use pi_coding_agent::core::tools::write::{WriteOperations, WriteToolOptions, create_write_tool};
use pi_js::BoxFuture;
use pi_js::abort::AbortController;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Notify;

#[tokio::test]
async fn preserves_both_parallel_edits_on_same_file() {
    struct DelayedEditOps;
    impl EditOperations for DelayedEditOps {
        fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
            let p = absolute_path.to_string();
            Box::pin(async move {
                let bytes = tokio::fs::read(&p)
                    .await
                    .map_err(|e| pi_js::Error::msg(e.to_string()))?;
                tokio::time::sleep(Duration::from_millis(30)).await;
                Ok(bytes)
            })
        }
        fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>> {
            let p = absolute_path.to_string();
            let c = content.as_bytes().to_vec();
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                tokio::fs::write(&p, &c)
                    .await
                    .map_err(|e| pi_js::Error::msg(e.to_string()))
            })
        }
        fn access(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<()>> {
            let p = absolute_path.to_string();
            Box::pin(async move {
                pi_js::fs::promises::access(&p, pi_js::fs::constants::R_OK | pi_js::fs::constants::W_OK).await
            })
        }
    }

    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("parallel-edit.txt");
    std::fs::write(&file_path, "alpha\nbeta\ngamma\n").unwrap();

    let cwd = dir.path().to_string_lossy().into_owned();
    let fstr = file_path.to_string_lossy().into_owned();
    let edit_tool = create_edit_tool(
        &cwd,
        Some(EditToolOptions {
            operations: Some(Arc::new(DelayedEditOps)),
        }),
    );

    let r1 = (edit_tool.execute)(
        "call-1".into(),
        json!({ "path": fstr, "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
        None,
        None,
    );
    let r2 = (edit_tool.execute)(
        "call-2".into(),
        json!({ "path": fstr, "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
        None,
        None,
    );

    let (res1, res2) = tokio::join!(r1, r2);
    res1.unwrap();
    res2.unwrap();
    assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "ALPHA\nBETA\ngamma\n");
}

#[tokio::test]
async fn shares_queue_between_edit_and_write() {
    struct SlowEditOps;
    impl EditOperations for SlowEditOps {
        fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
            let p = absolute_path.to_string();
            Box::pin(async move {
                let b = tokio::fs::read(&p)
                    .await
                    .map_err(|e| pi_js::Error::msg(e.to_string()))?;
                tokio::time::sleep(Duration::from_millis(30)).await;
                Ok(b)
            })
        }
        fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>> {
            let p = absolute_path.to_string();
            let c = content.as_bytes().to_vec();
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                tokio::fs::write(&p, &c)
                    .await
                    .map_err(|e| pi_js::Error::msg(e.to_string()))
            })
        }
        fn access(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<()>> {
            let p = absolute_path.to_string();
            Box::pin(async move {
                pi_js::fs::promises::access(&p, pi_js::fs::constants::R_OK | pi_js::fs::constants::W_OK).await
            })
        }
    }

    struct FastWriteOps;
    impl WriteOperations for FastWriteOps {
        fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>> {
            let p = absolute_path.to_string();
            let c = content.as_bytes().to_vec();
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                tokio::fs::write(&p, &c)
                    .await
                    .map_err(|e| pi_js::Error::msg(e.to_string()))
            })
        }
        fn mkdir(&self, _dir: &str) -> BoxFuture<pi_js::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("mixed.txt");
    std::fs::write(&file_path, "original\n").unwrap();

    let cwd = dir.path().to_string_lossy().into_owned();
    let fstr = file_path.to_string_lossy().into_owned();
    let edit_tool = create_edit_tool(
        &cwd,
        Some(EditToolOptions {
            operations: Some(Arc::new(SlowEditOps)),
        }),
    );
    let write_tool = create_write_tool(
        &cwd,
        Some(WriteToolOptions {
            operations: Some(Arc::new(FastWriteOps)),
        }),
    );

    let edit_fut = (edit_tool.execute)(
        "call-1".into(),
        json!({ "path": fstr, "edits": [{ "oldText": "original", "newText": "edited" }] }),
        None,
        None,
    );
    let write_fut = async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        (write_tool.execute)(
            "call-2".into(),
            json!({ "path": fstr, "content": "replacement\n" }),
            None,
            None,
        )
        .await
    };

    let (r1, r2) = tokio::join!(edit_fut, write_fut);
    r1.unwrap();
    r2.unwrap();
    assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "replacement\n");
}

#[tokio::test]
async fn keeps_write_queue_locked_while_aborted_write_in_flight() {
    struct CoordinatedWriteOps {
        first_started: Arc<Notify>,
        finish_first: Arc<Notify>,
        second_started: Arc<AtomicBool>,
        first_settled: Arc<AtomicBool>,
    }

    impl WriteOperations for CoordinatedWriteOps {
        fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>> {
            let p = absolute_path.to_string();
            let content = content.to_string();
            let first_started = Arc::clone(&self.first_started);
            let finish_first = Arc::clone(&self.finish_first);
            let second_started = Arc::clone(&self.second_started);
            let first_settled = Arc::clone(&self.first_settled);
            Box::pin(async move {
                if content == "first\n" {
                    first_started.notify_one();
                    finish_first.notified().await;
                    tokio::fs::write(&p, content.as_bytes())
                        .await
                        .map_err(|e| pi_js::Error::msg(e.to_string()))?;
                    first_settled.store(true, Ordering::SeqCst);
                    return Ok(());
                }
                if content == "second\n" {
                    assert!(first_settled.load(Ordering::SeqCst));
                    second_started.store(true, Ordering::SeqCst);
                }
                tokio::fs::write(&p, content.as_bytes())
                    .await
                    .map_err(|e| pi_js::Error::msg(e.to_string()))
            })
        }
        fn mkdir(&self, _dir: &str) -> BoxFuture<pi_js::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("abort-write.txt");
    let first_started = Arc::new(Notify::new());
    let finish_first = Arc::new(Notify::new());
    let second_started = Arc::new(AtomicBool::new(false));
    let first_settled = Arc::new(AtomicBool::new(false));

    let write_tool = create_write_tool(
        &dir.path().to_string_lossy(),
        Some(WriteToolOptions {
            operations: Some(Arc::new(CoordinatedWriteOps {
                first_started: Arc::clone(&first_started),
                finish_first: Arc::clone(&finish_first),
                second_started: Arc::clone(&second_started),
                first_settled: Arc::clone(&first_settled),
            })),
        }),
    );

    let controller = AbortController::new();
    let fstr = file_path.to_string_lossy().into_owned();
    let first_handle = tokio::spawn({
        let exec = Arc::clone(&write_tool.execute);
        let sig = controller.signal();
        let fstr = fstr.clone();
        async move {
            exec(
                "call-1".into(),
                json!({ "path": fstr, "content": "first\n" }),
                Some(sig),
                None,
            )
            .await
        }
    });

    first_started.notified().await;
    controller.abort(None);

    let second_handle = tokio::spawn({
        let exec = Arc::clone(&write_tool.execute);
        async move {
            exec(
                "call-2".into(),
                json!({ "path": fstr, "content": "second\n" }),
                None,
                None,
            )
            .await
        }
    });

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!second_started.load(Ordering::SeqCst));

    finish_first.notify_one();
    let first_err = first_handle.await.unwrap().unwrap_err();
    assert!(first_err.to_string().contains("Operation aborted"));
    second_handle.await.unwrap().unwrap();

    assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "second\n");
}

#[tokio::test]
async fn keeps_edit_queue_locked_while_aborted_edit_in_flight() {
    struct CoordinatedEditOps {
        first_started: Arc<Notify>,
        finish_first: Arc<Notify>,
        second_started: Arc<AtomicBool>,
        first_settled: Arc<AtomicBool>,
    }

    impl EditOperations for CoordinatedEditOps {
        fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
            let p = absolute_path.to_string();
            Box::pin(async move { tokio::fs::read(&p).await.map_err(|e| pi_js::Error::msg(e.to_string())) })
        }
        fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>> {
            let p = absolute_path.to_string();
            let content = content.to_string();
            let first_started = Arc::clone(&self.first_started);
            let finish_first = Arc::clone(&self.finish_first);
            let second_started = Arc::clone(&self.second_started);
            let first_settled = Arc::clone(&self.first_settled);
            Box::pin(async move {
                if content == "ALPHA\nbeta\n" {
                    first_started.notify_one();
                    finish_first.notified().await;
                    tokio::fs::write(&p, content.as_bytes())
                        .await
                        .map_err(|e| pi_js::Error::msg(e.to_string()))?;
                    first_settled.store(true, Ordering::SeqCst);
                    return Ok(());
                }
                if content == "ALPHA\nBETA\n" || content == "alpha\nBETA\n" {
                    assert!(first_settled.load(Ordering::SeqCst));
                    second_started.store(true, Ordering::SeqCst);
                }
                tokio::fs::write(&p, content.as_bytes())
                    .await
                    .map_err(|e| pi_js::Error::msg(e.to_string()))
            })
        }
        fn access(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<()>> {
            let p = absolute_path.to_string();
            Box::pin(async move {
                pi_js::fs::promises::access(&p, pi_js::fs::constants::R_OK | pi_js::fs::constants::W_OK).await
            })
        }
    }

    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("abort-edit.txt");
    std::fs::write(&file_path, "alpha\nbeta\n").unwrap();
    let first_started = Arc::new(Notify::new());
    let finish_first = Arc::new(Notify::new());
    let second_started = Arc::new(AtomicBool::new(false));
    let first_settled = Arc::new(AtomicBool::new(false));

    let edit_tool = create_edit_tool(
        &dir.path().to_string_lossy(),
        Some(EditToolOptions {
            operations: Some(Arc::new(CoordinatedEditOps {
                first_started: Arc::clone(&first_started),
                finish_first: Arc::clone(&finish_first),
                second_started: Arc::clone(&second_started),
                first_settled: Arc::clone(&first_settled),
            })),
        }),
    );

    let controller = AbortController::new();
    let fstr = file_path.to_string_lossy().into_owned();
    let first_handle = tokio::spawn({
        let exec = Arc::clone(&edit_tool.execute);
        let sig = controller.signal();
        let fstr = fstr.clone();
        async move {
            exec(
                "call-1".into(),
                json!({ "path": fstr, "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
                Some(sig),
                None,
            )
            .await
        }
    });

    first_started.notified().await;
    controller.abort(None);

    let second_handle = tokio::spawn({
        let exec = Arc::clone(&edit_tool.execute);
        async move {
            exec(
                "call-2".into(),
                json!({ "path": fstr, "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
                None,
                None,
            )
            .await
        }
    });

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!second_started.load(Ordering::SeqCst));

    finish_first.notify_one();
    let first_err = first_handle.await.unwrap().unwrap_err();
    assert!(first_err.to_string().contains("Operation aborted"));
    second_handle.await.unwrap().unwrap();

    assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "ALPHA\nBETA\n");
}

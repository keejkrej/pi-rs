//! Port of packages/coding-agent/test/bash-executor-truncation.test.ts

use std::sync::Arc;

use pi_coding_agent::core::bash_executor::{
    BashExecOptions, BashExecResult, BashExecutorOptions, BashOperations, execute_bash_with_operations,
};
use pi_coding_agent::core::messages::{BashExecutionMessage, bash_execution_to_text};
use pi_js::BoxFuture;
use pi_js::abort::AbortController;

struct MockBashOps {
    run: Arc<dyn Fn(Arc<dyn Fn(&[u8]) + Send + Sync>) -> pi_js::Result<BashExecResult> + Send + Sync>,
}

impl BashOperations for MockBashOps {
    fn exec(&self, _command: &str, _cwd: &str, options: BashExecOptions) -> BoxFuture<pi_js::Result<BashExecResult>> {
        let on_data = options.on_data.expect("on_data callback");
        let run = Arc::clone(&self.run);
        Box::pin(async move { run(on_data) })
    }
}

// #10164: the retained tail can fit even though earlier output was discarded.
#[tokio::test]
async fn reports_discarded_chunks_whether_cancelled_or_completed() {
    for cancelled in [false, true] {
        let first = format!(
            "error: missing build dependency\n{}",
            "x".repeat(64 * 1024 - "error: missing build dependency\n".len())
        );
        let last = format!(
            "remaining build output\n{}",
            "y".repeat(44 * 1024 - "remaining build output\n".len())
        );
        let first_c = first.clone();
        let last_c = last.clone();
        let controller = Arc::new(AbortController::new());
        let controller_c = Arc::clone(&controller);

        let ops = MockBashOps {
            run: Arc::new(move |on_data| {
                on_data(first_c.as_bytes());
                on_data(last_c.as_bytes());
                if cancelled {
                    controller_c.abort(None);
                    return Err(pi_js::Error::msg("aborted"));
                }
                Ok(BashExecResult { exit_code: Some(0) })
            }),
        };

        let result = execute_bash_with_operations(
            "cat build.log",
            ".",
            &ops,
            Some(BashExecutorOptions {
                on_chunk: None,
                signal: Some(controller.signal()),
            }),
        )
        .await
        .expect("bash execution");

        let full_path = result.full_output_path.clone().expect("full_output_path");
        let full_contents = std::fs::read_to_string(&full_path).expect("read full output");
        let _ = std::fs::remove_file(&full_path);

        assert_eq!(full_contents, format!("{first}{last}"));
        assert_eq!(result.cancelled, cancelled);
        assert_eq!(result.output, last);
        assert!(result.truncated);

        let rendered = bash_execution_to_text(&BashExecutionMessage {
            role: "bashExecution".to_string(),
            command: "cat build.log".to_string(),
            output: result.output,
            exit_code: result.exit_code.map(i64::from),
            cancelled: result.cancelled,
            truncated: result.truncated,
            full_output_path: result.full_output_path,
            timestamp: 0,
            exclude_from_context: None,
        });
        assert!(rendered.contains(&format!("[Output truncated. Full output: {full_path}]")));
    }
}

#[tokio::test]
async fn does_not_mark_short_output_as_truncated() {
    let ops = MockBashOps {
        run: Arc::new(|on_data| {
            on_data(b"done\n");
            Ok(BashExecResult { exit_code: Some(0) })
        }),
    };
    let result = execute_bash_with_operations("echo done", ".", &ops, None)
        .await
        .expect("short bash");
    assert_eq!(result.output, "done\n");
    assert!(!result.truncated);
    assert!(result.full_output_path.is_none());
}

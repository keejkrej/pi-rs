//! Port of packages/coding-agent/test/suite/regressions/5208-late-bash-output.test.ts

use std::sync::Arc;
use std::time::Duration;

use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::tools::bash::{
    BashExecOptions, BashExecResult, BashOperations, BashToolOptions, create_bash_tool,
};
use pi_js::BoxFuture;
use serde_json::json;

struct LateOutputOps;

impl BashOperations for LateOutputOps {
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a str,
        options: BashExecOptions,
    ) -> BoxFuture<pi_js::Result<BashExecResult>> {
        Box::pin(async move {
            if let Some(cb) = &options.on_data {
                cb(b"before\n");
                let cb_clone = Arc::clone(cb);
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    cb_clone(b"late\n");
                });
            }
            Ok(BashExecResult { exit_code: Some(0) })
        })
    }
}

#[tokio::test]
async fn ignores_output_callbacks_after_bash_operations_resolve() {
    let cwd = pi_js::env::cwd();
    let bash = create_bash_tool(
        &cwd,
        Some(BashToolOptions {
            operations: Some(Arc::new(LateOutputOps)),
            ..Default::default()
        }),
    );

    let res = (bash.execute)(
        "test-call-late-output".into(),
        json!({ "command": "late-output" }),
        None,
        None,
    )
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(25)).await;

    let text: String = res
        .content
        .iter()
        .filter_map(|b| match b {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(text.trim(), "before");
}

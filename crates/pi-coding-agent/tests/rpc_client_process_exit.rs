//! Port of packages/coding-agent/test/rpc-client-process-exit.test.ts

use pi_coding_agent::modes::rpc::rpc_client::{RpcClient, RpcClientOptions};
use tempfile::TempDir;

fn write_child_script() -> (TempDir, String) {
    let dir = TempDir::new().expect("create temp dir");
    let dir_str = dir.path().to_str().expect("utf8 temp dir path");
    #[cfg(unix)]
    {
        let path = pi_js::path::join(&[dir_str, "child.sh"]);
        pi_js::fs::write_file(&path, "#!/bin/sh\nread line\nexit 43\n", Some(0o755)).expect("write child.sh");
        pi_js::fs::chmod(&path, 0o755).expect("chmod child.sh");
        (dir, path)
    }
    #[cfg(windows)]
    {
        let path = pi_js::path::join(&[dir_str, "child.cmd"]);
        pi_js::fs::write_file(&path, "@echo off\r\nset /p line=\r\nexit /b 43\r\n", None).expect("write child.cmd");
        (dir, path)
    }
}

mod rpc_client_child_process_failures {
    use super::*;

    #[tokio::test]
    async fn rejects_an_in_flight_request_when_the_child_process_exits() {
        let (_dir, cli_path) = write_child_script();
        let client = RpcClient::new(RpcClientOptions {
            cli_path: Some(cli_path),
            ..Default::default()
        });

        client.start().await.expect("start client");

        let err = client
            .get_commands()
            .await
            .expect_err("expected get_commands to fail on child exit");
        let msg = err.to_string();
        assert!(
            msg.contains("Agent process exited (code=43 signal=null)"),
            "expected 'Agent process exited (code=43 signal=null)' in error, got: {msg}"
        );

        client.stop().await;
    }
}

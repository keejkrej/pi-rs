//! Port of packages/coding-agent/test/bash-close-hang-windows.test.ts

use std::time::Duration;

use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::tools::bash::create_bash_tool;
use pi_js::abort::AbortController;
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn bash_tool_resolves_after_shell_exits_with_inherited_stdio_on_windows() {
    if pi_js::env::platform() != "win32" {
        return;
    }

    let dir = TempDir::new().unwrap();
    let pid_file = dir.path().join("tool-grandchild.pid");
    let pid_str = pid_file.to_string_lossy().replace('\\', "/");
    let command = format!(
        "node -e \"const fs=require('fs');const {{spawn}}=require('child_process');const c=spawn(process.execPath,['-e','setTimeout(()=>{{}},60000)'],{{stdio:'inherit',detached:true}});fs.writeFileSync(process.argv[1],String(c.pid));c.unref();console.log('child-exiting');\" '{}'",
        pid_str
    );

    let controller = AbortController::new();
    let tool = create_bash_tool(&dir.path().to_string_lossy(), None);
    let fut = (tool.execute)(
        "test-call".into(),
        json!({ "command": command }),
        Some(controller.signal()),
        None,
    );

    let res = tokio::time::timeout(Duration::from_secs(3), fut)
        .await
        .expect("timed out waiting for bash tool with inherited stdio");
    controller.abort(None);

    if let Ok(pid_txt) = std::fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_txt.trim().parse::<u32>() {
            pi_coding_agent::core::tools::mcp_wire::kill_process_tree(pid);
        }
    }

    let result = res.unwrap();
    let text: String = result
        .content
        .iter()
        .filter_map(|b| match b {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("child-exiting"), "{text}");
}

// WIRE(@mcp-w3): executeBashWithOperations inherited-stdio test enables once core::bash_executor lands.
#[cfg(any())]
mod pending_bash_executor_test {}

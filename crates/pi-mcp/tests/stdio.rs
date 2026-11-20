use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use pi_mcp::client::ClientState;
use pi_mcp::{McpClient, McpClientOptions, McpTransport, StdioTransport, StdioTransportOptions};
use pretty_assertions::assert_eq;
use serde_json::{from_value, json};

const FIXTURE: &str = env!("CARGO_BIN_EXE_fixture-stdio-server");
#[cfg(unix)]
const STUBBORN: &str = env!("CARGO_BIN_EXE_fixture-stubborn-server");
#[cfg(unix)]
const EXITING_PARENT: &str = env!("CARGO_BIN_EXE_fixture-exiting-parent-server");

#[tokio::test]
async fn connects_to_a_newline_delimited_mcp_server_and_captures_stderr() {
    let stderr = Arc::new(Mutex::new(Vec::<String>::new()));
    let stderr_cb = stderr.clone();
    let transport = Arc::new(StdioTransport::new(StdioTransportOptions {
        command: FIXTURE.to_string(),
        args: None,
        cwd: None,
        env: None,
        inherit_env: None,
        stderr: None,
        on_stderr: Some(Arc::new(move |chunk| {
            stderr_cb.lock().unwrap().push(chunk.to_string());
        })),
        max_message_bytes: None,
        max_stderr_bytes: None,
        close_timeout_ms: None,
    }));
    let client = McpClient::new(McpClientOptions {
        name: "stdio-test".to_string(),
        version: "1.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: None,
        roots: None,
    });
    client.connect(transport.clone()).await.expect("connect");

    let tools = client.list_tools(None).await.expect("list_tools");
    let expected_tools: Vec<pi_mcp::Tool> =
        from_value(json!([{ "name": "echo", "inputSchema": { "type": "object" } }])).unwrap();
    assert_eq!(tools, expected_tools);

    let mut args = IndexMap::new();
    args.insert("text".to_string(), json!("hello"));
    let call_res = client.call_tool("echo", Some(args), None).await.expect("call_tool");
    let expected_call: pi_mcp::CallToolResult =
        from_value(json!({ "content": [{ "type": "text", "text": "hello" }] })).unwrap();
    assert_eq!(call_res, expected_call);

    assert!(transport.pid().is_some());
    tokio::time::sleep(Duration::from_millis(20)).await;
    let joined = stderr.lock().unwrap().join("");
    assert!(joined.contains("stdio fixture ready"), "stderr: {joined}");
    assert!(
        transport.stderr().contains("stdio fixture ready"),
        "transport.stderr: {}",
        transport.stderr()
    );

    client.close().await.expect("close");
    assert_eq!(client.connection_state(), ClientState::Closed);
}

#[cfg(unix)]
#[tokio::test]
async fn kills_a_server_that_ignores_shutdown_including_its_children() {
    let transport = Arc::new(StdioTransport::new(StdioTransportOptions {
        command: STUBBORN.to_string(),
        args: None,
        cwd: None,
        env: None,
        inherit_env: None,
        stderr: None,
        on_stderr: None,
        max_message_bytes: None,
        max_stderr_bytes: None,
        close_timeout_ms: Some(100),
    }));
    let client = McpClient::new(McpClientOptions {
        name: "stdio-test".to_string(),
        version: "1.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: None,
        roots: None,
    });
    client.connect(transport.clone()).await.expect("connect");

    let mut grandchild: Option<u32> = None;
    for _ in 0..100 {
        let stderr_text = transport.stderr();
        if let Some(pos) = stderr_text.find("grandchild ") {
            let rest = &stderr_text[pos + "grandchild ".len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(pid) = digits.parse::<u32>() {
                grandchild = Some(pid);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let grandchild = grandchild.expect("grandchild pid should appear in stderr");

    let started_at = tokio::time::Instant::now();
    client.close().await.expect("close");
    assert!(started_at.elapsed() < Duration::from_secs(5));

    let mut alive = true;
    for _ in 0..100 {
        let status = std::process::Command::new("kill")
            .args(["-0", &grandchild.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => tokio::time::sleep(Duration::from_millis(20)).await,
            _ => {
                alive = false;
                break;
            }
        }
    }
    assert!(!alive, "grandchild process {grandchild} is still alive");
}

#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// #11721
#[cfg(unix)]
#[tokio::test]
async fn kills_children_still_holding_pipes_after_server_process_exited() {
    let transport = Arc::new(StdioTransport::new(StdioTransportOptions {
        command: EXITING_PARENT.to_string(),
        args: None,
        cwd: None,
        env: None,
        inherit_env: None,
        stderr: None,
        on_stderr: None,
        max_message_bytes: None,
        max_stderr_bytes: None,
        close_timeout_ms: Some(100),
    }));
    let close_events = Arc::new(AtomicUsize::new(0));
    let close_cb = close_events.clone();
    let _unsub = transport.on_close(Arc::new(move || {
        close_cb.fetch_add(1, Ordering::SeqCst);
    }));
    transport.start().await.expect("start");
    let server_pid = transport.pid().expect("server pid");

    let mut helper_pid: Option<u32> = None;
    for _ in 0..100 {
        let stderr_text = transport.stderr();
        if let Some(pos) = stderr_text.find("helper ") {
            let rest = &stderr_text[pos + "helper ".len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(pid) = digits.parse::<u32>() {
                helper_pid = Some(pid);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let helper = helper_pid.expect("helper pid in stderr");

    struct KillGuard(u32);
    impl Drop for KillGuard {
        fn drop(&mut self) {
            let _ = std::process::Command::new("kill")
                .args(["-9", &self.0.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    let _guard = KillGuard(helper);

    let mut parent_exited = false;
    for _ in 0..200 {
        if !is_pid_alive(server_pid) {
            parent_exited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(parent_exited, "server parent pid {server_pid} did not exit");
    assert!(is_pid_alive(helper), "helper {helper} should still be running");

    transport.close().await.expect("close");
    let mut helper_killed = false;
    for _ in 0..200 {
        if !is_pid_alive(helper) {
            helper_killed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(helper_killed, "helper {helper} survived transport.close()");
    assert_eq!(close_events.load(Ordering::SeqCst), 1);
}

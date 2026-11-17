//! Port of packages/coding-agent/test/startup-session-name.test.ts

use std::fs;
use std::process::{Command, Stdio};

use pi_coding_agent::config::ENV_AGENT_DIR;
use serde_json::Value;
use tempfile::TempDir;

struct TestDirs {
    _root: TempDir,
    agent_dir: String,
    project_dir: String,
    session_file: String,
}

fn setup() -> TestDirs {
    let root = tempfile::tempdir().expect("create tempdir");
    let agent_dir = root.path().join("agent");
    let project_dir = root.path().join("project");
    let session_file = root.path().join("session.jsonl");
    fs::create_dir_all(&agent_dir).expect("create agent_dir");
    fs::create_dir_all(&project_dir).expect("create project_dir");

    let timestamp = pi_js::time::iso_now();
    let project_dir_str = project_dir.to_string_lossy().into_owned();
    let header = serde_json::json!({
        "type": "session",
        "version": 3,
        "id": "existing-session",
        "timestamp": timestamp,
        "cwd": project_dir_str,
    });
    let msg = serde_json::json!({
        "type": "message",
        "id": "assistant-1",
        "parentId": Value::Null,
        "timestamp": timestamp,
        "message": {
            "role": "assistant",
            "content": [{ "type": "text", "text": "hello" }],
            "api": "anthropic-messages",
            "provider": "anthropic",
            "model": "claude-sonnet-4-5",
            "usage": {
                "input": 0,
                "output": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
                "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
            },
            "stopReason": "stop",
            "timestamp": pi_js::time::now_ms() as i64
        }
    });
    let contents = format!(
        "{}\n{}\n",
        pi_js::json::stringify(&header),
        pi_js::json::stringify(&msg)
    );
    fs::write(&session_file, contents).expect("write session fixture");

    TestDirs {
        _root: root,
        agent_dir: agent_dir.to_string_lossy().into_owned(),
        project_dir: project_dir_str,
        session_file: session_file.to_string_lossy().into_owned(),
    }
}

fn read_session_info_names(session_file: &str) -> Vec<String> {
    let raw = fs::read_to_string(session_file).expect("read session file");
    raw.trim()
        .lines()
        .filter_map(|line| pi_js::json::parse(line).ok())
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("session_info"))
        .map(|v| v.get("name").and_then(Value::as_str).unwrap_or("").to_string())
        .collect()
}

#[test]
fn sets_name_on_the_selected_session_before_runtime_model_validation() {
    let dirs = setup();
    let output = Command::new(env!("CARGO_BIN_EXE_pi"))
        .args([
            "--session",
            &dirs.session_file,
            "--name",
            "  CLI Named Session  ",
            "--model",
            "missing-model",
            "-p",
            "hi",
        ])
        .current_dir(&dirs.project_dir)
        .env(*ENV_AGENT_DIR, &dirs.agent_dir)
        .env("PI_OFFLINE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn pi cli");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        read_session_info_names(&dirs.session_file),
        vec!["CLI Named Session".to_string()]
    );
}

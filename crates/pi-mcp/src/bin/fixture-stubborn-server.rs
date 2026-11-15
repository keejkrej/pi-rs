//! Port of packages/mcp/test/fixtures/stubborn-server.mjs (fixture process entry)

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn install_sigterm_ignore() {
    #[cfg(unix)]
    {
        if let Ok(mut sigterm) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            tokio::spawn(async move { while sigterm.recv().await.is_some() {} });
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    install_sigterm_ignore();

    if std::env::args().nth(1).as_deref() == Some("--grandchild") {
        std::future::pending::<()>().await;
        return;
    }

    let exe = std::env::current_exe().expect("current_exe");
    let grandchild = Command::new(exe)
        .arg("--grandchild")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn grandchild");
    eprintln!("grandchild {}", grandchild.id());
    let _ = std::io::stderr().flush();

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(Value::Object(message)) = pi_js::json::parse(&line) else {
            continue;
        };
        if message.get("method").and_then(Value::as_str) != Some("initialize") {
            continue;
        }
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let mut resp = Map::new();
        resp.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
        resp.insert("id".to_string(), id);
        resp.insert(
            "result".to_string(),
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "serverInfo": { "name": "stubborn-fixture", "version": "1.0.0" }
            }),
        );
        let out = format!("{}\n", pi_js::json::stringify(&Value::Object(resp)));
        let _ = stdout.write_all(out.as_bytes()).await;
        let _ = stdout.flush().await;
    }

    // Ignore stdin EOF and SIGTERM until SIGKILL terminates the process.
    std::future::pending::<()>().await;
}

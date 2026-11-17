//! Port of packages/coding-agent/src/utils/clipboard-command.ts

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

#[derive(Clone, Debug, Default)]
pub struct ClipboardCommandOptions {
    pub input: Option<String>,
    pub timeout_ms: Option<u64>,
    pub max_buffer_bytes: Option<usize>,
}

/// Undefined (`None`) means the command failed; an empty `Vec` is a successful result.
pub async fn run_clipboard_command(command: &str, args: &[&str], options: ClipboardCommandOptions) -> Option<Vec<u8>> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .env_clear()
        .envs(pi_js::env::vars())
        .stdin(Stdio::piped())
        .stdout(if options.input.is_none() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().ok()?;
    let timeout_ms = options.timeout_ms.unwrap_or(3000);
    let max_buffer_bytes = options.max_buffer_bytes.unwrap_or(50 * 1024 * 1024);

    if let Some(mut stdin) = child.stdin.take() {
        if let Some(ref input) = options.input {
            let bytes = input.as_bytes().to_vec();
            tokio::spawn(async move {
                let _ = stdin.write_all(&bytes).await;
                let _ = stdin.shutdown().await;
            });
        } else {
            drop(stdin);
        }
    }

    let mut stdout = child.stdout.take();
    let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(deadline);

    let read_future = async {
        let mut chunks: Vec<u8> = Vec::new();
        if let Some(ref mut out) = stdout {
            let mut buf = [0u8; 8192];
            loop {
                let n = out.read(&mut buf).await.ok()?;
                if n == 0 {
                    break;
                }
                if chunks.len().saturating_add(n) > max_buffer_bytes {
                    return None;
                }
                chunks.extend_from_slice(&buf[..n]);
            }
        }
        let status = child.wait().await.ok()?;
        if status.success() { Some(chunks) } else { None }
    };

    tokio::select! {
        res = read_future => res,
        () = &mut deadline => {
            let _ = child.kill().await;
            None
        }
    }
}

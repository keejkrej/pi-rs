//! Port of packages/mcp/test/fixtures/exiting-parent-server.mjs (fixture process entry)

use std::io::Write;
use std::process::{Command, Stdio};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("--helper") {
        std::future::pending::<()>().await;
        return;
    }

    let exe = std::env::current_exe().expect("current_exe");
    let helper = Command::new(exe)
        .arg("--helper")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn helper");
    eprintln!("helper {}", helper.id());
    let _ = std::io::stderr().flush();
    std::process::exit(0);
}

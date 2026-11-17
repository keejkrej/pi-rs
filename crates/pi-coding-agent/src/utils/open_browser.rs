//! Port of packages/coding-agent/src/utils/open-browser.ts

/// Open a URL or file in the platform browser/default handler.
///
/// This intentionally never invokes a shell. On Windows, do not use
/// `cmd /c start`: cmd.exe re-parses metacharacters (&, |, ^, ...) before
/// `start` runs, which would make attacker-controlled URLs injectable.
pub fn open_browser(target: &str) {
    let platform = pi_js::env::platform();
    let (cmd, args): (&str, Vec<&str>) = if platform == "darwin" {
        ("open", vec![target])
    } else if platform == "win32" {
        ("rundll32", vec!["url.dll,FileProtocolHandler", target])
    } else {
        ("xdg-open", vec![target])
    };

    let mut command = std::process::Command::new(cmd);
    command
        .args(args)
        .env_clear()
        .envs(pi_js::env::vars())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    // Best-effort detached launch; swallow launcher failures (for example, missing xdg-open).
    let _ = command.spawn();
}

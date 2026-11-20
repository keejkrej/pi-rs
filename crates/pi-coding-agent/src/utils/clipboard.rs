//! Port of packages/coding-agent/src/utils/clipboard.ts

use indexmap::IndexMap;
use pi_js::{Error, Result};
use pi_tui::{NativeClipboard as _, get_native_clipboard};

use crate::utils::clipboard_command::{ClipboardCommandOptions, run_clipboard_command};
use crate::utils::wsl::is_wsl;

const MAX_OSC52_ENCODED_LENGTH: usize = 100_000;

fn is_remote_session(env: &IndexMap<String, String>) -> bool {
    let has = |k: &str| env.get(k).is_some_and(|v| !v.is_empty());
    has("SSH_CONNECTION")
        || has("SSH_CLIENT")
        || has("MOSH_CONNECTION")
        || env.get("PI_CLIPBOARD_OSC52").is_some_and(|v| v == "1")
}

fn emit_osc52(text: &str) -> bool {
    let encoded = pi_js::b64::encode(text.as_bytes());
    if encoded.len() > MAX_OSC52_ENCODED_LENGTH {
        return false;
    }
    pi_js::console::stdout_write(&format!("\x1b]52;c;{encoded}\x07"));
    true
}

async fn copy_via_windows_clipboard(text: &str) -> bool {
    let tmp_file = pi_js::path::join(&[
        &pi_js::env::tmp_dir(),
        &format!("pi-wsl-clip-{}.txt", pi_js::crypto::random_uuid()),
    ]);
    let result = async {
        if pi_js::fs::write_file(&tmp_file, text.as_bytes(), Some(0o600)).is_err() {
            return false;
        }
        let Some(win_path_bytes) = run_clipboard_command(
            "wslpath",
            &["-w", &tmp_file],
            ClipboardCommandOptions {
                input: None,
                timeout_ms: Some(1000),
                max_buffer_bytes: None,
            },
        )
        .await
        else {
            return false;
        };
        let win_path = String::from_utf8_lossy(&win_path_bytes).trim().to_owned();
        if win_path.is_empty() {
            return false;
        }
        let escaped = win_path.replace('\'', "''");
        let script =
            format!("Set-Clipboard -Value ([System.IO.File]::ReadAllText('{escaped}', [System.Text.Encoding]::UTF8))");
        run_clipboard_command(
            "powershell.exe",
            &["-NoProfile", "-Command", &script],
            ClipboardCommandOptions {
                input: None,
                timeout_ms: Some(5000),
                max_buffer_bytes: None,
            },
        )
        .await
        .is_some()
    }
    .await;

    let _ = pi_js::fs::unlink(&tmp_file);
    result
}

/// Read plain text from the system clipboard.
pub async fn read_clipboard_text() -> Option<String> {
    let mut commands: Vec<(&str, Vec<&str>)> = Vec::new();
    // Termux reports platform "android", not "linux" (#10391).
    if pi_js::env::var("TERMUX_VERSION").is_some_and(|v| !v.is_empty()) {
        commands.push(("termux-clipboard-get", Vec::new()));
    }
    if pi_js::env::platform() == "linux" {
        if pi_js::env::var("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
            commands.push(("wl-paste", vec!["--no-newline", "--type", "text"]));
        }
        if pi_js::env::var("DISPLAY").is_some_and(|v| !v.is_empty()) {
            commands.push(("xclip", vec!["-selection", "clipboard", "-out"]));
            commands.push(("xsel", vec!["--clipboard", "--output"]));
        }
    }
    for (command, args) in commands {
        let arg_refs: Vec<&str> = args;
        if let Some(bytes) = run_clipboard_command(
            command,
            &arg_refs,
            ClipboardCommandOptions {
                input: None,
                timeout_ms: Some(5000),
                max_buffer_bytes: None,
            },
        )
        .await
        {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            return if text.is_empty() { None } else { Some(text) };
        }
    }

    let clipboard = get_native_clipboard()?;
    match clipboard.get_text().await {
        Ok(Some(Some(text))) if !text.is_empty() => Some(text),
        _ => None,
    }
}

/// Read file paths, such as Finder file copies, from the native clipboard.
pub async fn read_clipboard_file_paths() -> Option<Vec<String>> {
    let clipboard = get_native_clipboard()?;
    match clipboard.get_file_paths().await {
        Ok(Some(Some(paths))) if !paths.is_empty() => Some(paths),
        _ => None,
    }
}

pub async fn copy_to_clipboard(text: &str) -> Result<()> {
    let p = pi_js::env::platform();
    let env = pi_js::env::vars();
    let mut copied = false;

    if p != "linux"
        && let Some(clipboard) = get_native_clipboard()
        && clipboard.has_set_text()
        && clipboard.set_text(text).await.is_ok()
    {
        copied = true;
    }

    if !copied {
        let mut commands: Vec<(&str, Vec<&str>)> = Vec::new();
        if p == "darwin" {
            commands.push(("pbcopy", Vec::new()));
        } else if p == "win32" {
            commands.push(("clip", Vec::new()));
        } else {
            let has = |k: &str| env.get(k).is_some_and(|v| !v.is_empty());
            if has("TERMUX_VERSION") {
                commands.push(("termux-clipboard-set", Vec::new()));
            }
            if has("WAYLAND_DISPLAY") {
                commands.push(("wl-copy", Vec::new()));
            }
            if has("DISPLAY") {
                commands.push(("xclip", vec!["-selection", "clipboard"]));
                commands.push(("xsel", vec!["--clipboard", "--input"]));
            }
        }
        for (command, args) in commands {
            if run_clipboard_command(
                command,
                &args,
                ClipboardCommandOptions {
                    input: Some(text.to_owned()),
                    timeout_ms: Some(5000),
                    max_buffer_bytes: None,
                },
            )
            .await
            .is_some()
            {
                copied = true;
                break;
            }
        }
    }

    let mut osc52_emitted = false;
    if !copied && p == "linux" && is_wsl(Some(&env)) {
        if env.get("WT_SESSION").is_some_and(|v| !v.is_empty()) {
            osc52_emitted = emit_osc52(text);
        }
        copied = osc52_emitted || copy_via_windows_clipboard(text).await;
    }

    let has = |k: &str| env.get(k).is_some_and(|v| !v.is_empty());
    let headless = p == "linux" && !has("DISPLAY") && !has("WAYLAND_DISPLAY") && !has("TERMUX_VERSION");
    let mut oversized = false;
    if !osc52_emitted && (is_remote_session(&env) || (!copied && headless)) {
        if emit_osc52(text) {
            copied = true;
        } else {
            oversized = true;
        }
    }

    if copied {
        return Ok(());
    }
    if oversized {
        return Err(Error::msg("Clipboard unavailable: text exceeds the OSC 52 size limit"));
    }
    if has("TERMUX_VERSION") {
        return Err(Error::msg(
            "Clipboard unavailable: install the Termux:API app and `termux-api` package",
        ));
    }
    if p == "linux" {
        if has("WAYLAND_DISPLAY") {
            return Err(Error::msg(
                "Clipboard unavailable: install `wl-clipboard` (`wl-copy`) or check Wayland access",
            ));
        }
        if has("DISPLAY") {
            return Err(Error::msg(
                "Clipboard unavailable: install `xclip` or `xsel`, or check X11 access",
            ));
        }
    }
    Err(Error::msg("Clipboard unavailable"))
}

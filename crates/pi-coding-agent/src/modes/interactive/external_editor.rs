//! Port of packages/coding-agent/src/modes/interactive/external-editor.ts

use std::process::Stdio;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalEditorOptions {
    pub command: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalEditorResult {
    Complete { content: String },
    Failed { message: String },
}

struct TempDirCleanup(String);

impl Drop for TempDirCleanup {
    fn drop(&mut self) {
        let _ = pi_js::fs::rm(&self.0, true, true);
    }
}

fn strip_bom_local(s: &str) -> &str {
    // WIRE(@tui-w3): use crate::utils::text::strip_bom once @mcp lands Stage 3A-1.
    s.strip_prefix('\u{FEFF}').unwrap_or(s)
}

struct PreparedExternalEditor {
    _cleanup: TempDirCleanup,
    file_path: String,
    command: String,
    args: Vec<String>,
    program: String,
}

fn prepare_external_editor(options: ExternalEditorOptions) -> pi_js::Result<PreparedExternalEditor> {
    let prefix = pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-editor-"]);
    let directory = pi_js::fs::mkdtemp(&prefix)?;
    let cleanup = TempDirCleanup(directory.clone());
    let file_path = pi_js::path::join(&[&directory, "prompt.md"]);

    pi_js::fs::write_file(&file_path, options.content.as_bytes(), None)?;

    let mut parts = options.command.split(' ');
    let editor = parts.next().unwrap_or("");
    let editor_args: Vec<&str> = parts.collect();

    pi_js::console::stdout_write(&format!(
        "Launching external editor: {}\nPi will resume when the editor exits.\n",
        options.command
    ));

    let (program, args) = if pi_js::env::platform() == "win32" {
        let mut line_parts = Vec::with_capacity(editor_args.len() + 2);
        line_parts.push(editor);
        line_parts.extend(editor_args.iter().copied());
        line_parts.push(&file_path);
        ("cmd".to_string(), vec!["/C".to_string(), line_parts.join(" ")])
    } else {
        let mut args: Vec<String> = editor_args.into_iter().map(str::to_string).collect();
        args.push(file_path.clone());
        (editor.to_string(), args)
    };

    Ok(PreparedExternalEditor {
        _cleanup: cleanup,
        file_path,
        command: options.command,
        args,
        program,
    })
}

fn finish_external_editor_status(
    prepared: &PreparedExternalEditor,
    status_res: std::io::Result<std::process::ExitStatus>,
) -> pi_js::Result<ExternalEditorResult> {
    let status = match status_res {
        Ok(s) => s,
        Err(_) => {
            return Ok(ExternalEditorResult::Failed {
                message: format!("Failed to launch external editor: {}", prepared.command),
            });
        }
    };

    if !status.success() {
        #[cfg(unix)]
        if status.code().is_none() {
            use std::os::unix::process::ExitStatusExt;
            if let Some(sig) = status.signal() {
                return Ok(ExternalEditorResult::Failed {
                    message: format!("External editor exited with signal {sig}; prompt unchanged"),
                });
            }
        }
        let exit_desc = status
            .code()
            .map(|c| format!("code {c}"))
            .unwrap_or_else(|| "unknown status".to_string());
        return Ok(ExternalEditorResult::Failed {
            message: format!("External editor exited with {exit_desc}; prompt unchanged"),
        });
    }

    let updated = pi_js::fs::read_to_string(&prepared.file_path)?;
    let stripped = strip_bom_local(&updated);
    let trimmed = stripped.strip_suffix('\n').unwrap_or(stripped);
    Ok(ExternalEditorResult::Complete {
        content: trimmed.to_string(),
    })
}

pub async fn edit_in_external_editor(options: ExternalEditorOptions) -> pi_js::Result<ExternalEditorResult> {
    let prepared = prepare_external_editor(options)?;
    // Do not use synchronous blocking process spawning across async tasks.
    let mut cmd = tokio::process::Command::new(&prepared.program);
    cmd.args(&prepared.args)
        .env_clear()
        .envs(pi_js::env::vars())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status_res = cmd.status().await;
    finish_external_editor_status(&prepared, status_res)
}

pub(crate) fn edit_in_external_editor_blocking(options: ExternalEditorOptions) -> pi_js::Result<ExternalEditorResult> {
    let prepared = prepare_external_editor(options)?;
    let mut cmd = std::process::Command::new(&prepared.program);
    cmd.args(&prepared.args)
        .env_clear()
        .envs(pi_js::env::vars())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status_res = cmd.status();
    finish_external_editor_status(&prepared, status_res)
}

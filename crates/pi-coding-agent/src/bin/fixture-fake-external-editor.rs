//! Port of packages/coding-agent/test/fixtures/fake-external-editor.mjs (fixture process entry)

use std::path::Path;
use std::process::ExitCode;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let Some(capture_path) = argv.get(2) else {
        return ExitCode::from(1);
    };
    let Some(file_path) = argv.last() else {
        return ExitCode::from(1);
    };

    let directory = Path::new(file_path).parent().unwrap_or_else(|| Path::new("."));
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to read {file_path}: {e}");
            return ExitCode::from(1);
        }
    };
    let mut entries: Vec<String> = match std::fs::read_dir(directory) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(e) => {
            eprintln!("failed to read dir {}: {e}", directory.display());
            return ExitCode::from(1);
        }
    };
    entries.sort();

    #[cfg(unix)]
    let directory_mode: u32 = match std::fs::metadata(directory) {
        Ok(m) => m.permissions().mode() & 0o777,
        Err(e) => {
            eprintln!("failed to stat dir {}: {e}", directory.display());
            return ExitCode::from(1);
        }
    };
    #[cfg(not(unix))]
    let directory_mode: u32 = 0o777;

    let payload = serde_json::json!({
        "filePath": file_path,
        "content": content,
        "entries": entries,
        "directoryMode": directory_mode,
    });
    if let Err(e) = std::fs::write(capture_path, pi_js::json::stringify(&payload)) {
        eprintln!("failed to write capture {capture_path}: {e}");
        return ExitCode::from(1);
    }

    if argv.iter().any(|arg| arg == "--fail") {
        return ExitCode::from(1);
    }

    let next_content = if argv.iter().any(|arg| arg == "--empty") {
        ""
    } else {
        "edited\n"
    };
    if let Err(e) = std::fs::write(file_path, next_content) {
        eprintln!("failed to write {file_path}: {e}");
        return ExitCode::from(1);
    }

    ExitCode::SUCCESS
}

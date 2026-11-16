//! Port of packages/coding-agent/test/fixtures/fake-external-editor.mjs (fixture process entry)

use std::fs;
use std::path::Path;

use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        std::process::exit(1);
    }
    let capture_path = &args[1];
    let file_path = match args.last() {
        Some(p) => p,
        None => std::process::exit(1),
    };

    let directory = Path::new(file_path).parent().unwrap_or_else(|| Path::new("."));
    let content = fs::read_to_string(file_path).unwrap();
    let mut entries: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    entries.sort();

    #[cfg(unix)]
    let directory_mode: u32 = {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(directory).unwrap().permissions().mode() & 0o777
    };
    #[cfg(not(unix))]
    let directory_mode: u32 = 0;

    let capture = json!({
        "filePath": file_path,
        "content": content,
        "entries": entries,
        "directoryMode": directory_mode,
    });
    fs::write(capture_path, pi_js::json::stringify(&capture)).unwrap();

    if args.iter().any(|a| a == "--fail") {
        std::process::exit(1);
    }
    let next_content = if args.iter().any(|a| a == "--empty") {
        ""
    } else {
        "edited\n"
    };
    fs::write(file_path, next_content).unwrap();
}

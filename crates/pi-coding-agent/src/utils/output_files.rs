//! Port of packages/coding-agent/src/utils/output-files.ts

use std::fmt::Write as _;

use pi_js::Result;
use pi_js::fs::File;

/// Output can carry private data, so only the user may read the files.
pub const OUTPUT_FILE_MODE: u32 = 0o600;

/// A new, unused path: `<dir>/<prefix>-<random hex><extension>`. `extension` includes the dot.
pub fn create_output_file_path(prefix: &str, extension: &str) -> String {
    let bytes = pi_js::crypto::random_bytes(8);
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    pi_js::path::join(&[&pi_js::env::tmp_dir(), &format!("{prefix}-{hex}{extension}")])
}

/// Write `data` to a new output file (`wx` never follows an existing symlink) and return its path.
pub async fn write_output_file(prefix: &str, extension: &str, data: impl AsRef<[u8]>) -> Result<String> {
    let path = create_output_file_path(prefix, extension);
    pi_js::fs::write_file_with_flag(&path, data, "wx", Some(OUTPUT_FILE_MODE))?;
    Ok(path)
}

/// Open a new output file (`wx`) for streamed output and return `(path, file)`.
pub fn create_output_file_stream(prefix: &str, extension: &str) -> Result<(String, File)> {
    let path = create_output_file_path(prefix, extension);
    let file = pi_js::fs::open(&path, "wx", Some(OUTPUT_FILE_MODE))?;
    Ok((path, file))
}

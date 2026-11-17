//! Port of packages/coding-agent/test/path-utils.test.ts

use pi_coding_agent::core::tools::path_utils::{expand_path, resolve_read_path, resolve_to_cwd};
use tempfile::TempDir;

#[test]
fn expand_path_expands_tilde_to_home() {
    let result = expand_path("~");
    assert!(!result.contains('~'));
}

#[test]
fn expand_path_expands_tilde_slash_to_home() {
    let result = expand_path("~/Documents/file.txt");
    assert!(!result.contains("~/"));
}

#[test]
fn expand_path_keeps_tilde_prefixed_filenames_literal() {
    assert_eq!(expand_path("~draft.md"), "~draft.md");
    assert_eq!(expand_path("@~draft.md"), "~draft.md");
}

#[test]
fn expand_path_normalizes_unicode_spaces() {
    let with_nbsp = "file\u{00A0}name.txt";
    assert_eq!(expand_path(with_nbsp), "file name.txt");
}

#[test]
fn resolve_to_cwd_resolves_absolute_paths_as_is() {
    let tmp = pi_js::env::tmp_dir();
    let abs = pi_js::path::resolve(&[&tmp, "absolute", "path", "file.txt"]);
    let cwd = pi_js::path::resolve(&[&tmp, "some", "cwd"]);
    assert_eq!(resolve_to_cwd(&abs, &cwd), abs);
}

#[test]
fn resolve_to_cwd_resolves_relative_paths_against_cwd() {
    let result = resolve_to_cwd("relative/file.txt", "/some/cwd");
    assert_eq!(result, pi_js::path::resolve(&["/some/cwd", "relative/file.txt"]));
}

#[test]
fn resolve_to_cwd_resolves_tilde_prefixed_filenames_against_cwd() {
    let tmp = pi_js::env::tmp_dir();
    let cwd = pi_js::path::join(&[&tmp, "pi-path-utils-cwd"]);
    assert_eq!(
        resolve_to_cwd("~draft.md", &cwd),
        pi_js::path::resolve(&[&cwd, "~draft.md"])
    );
    assert_eq!(
        resolve_to_cwd("@~draft.md", &cwd),
        pi_js::path::resolve(&[&cwd, "~draft.md"])
    );
}

#[test]
fn resolve_read_path_resolves_existing_file_path() {
    let dir = TempDir::new().unwrap();
    let file_name = "test-file.txt";
    let full = dir.path().join(file_name);
    std::fs::write(&full, "content").unwrap();

    let cwd = dir.path().to_string_lossy();
    let result = resolve_read_path(file_name, &cwd);
    assert_eq!(result, full.to_string_lossy());
}

#[test]
fn resolve_read_path_handles_nfc_vs_nfd() {
    let dir = TempDir::new().unwrap();
    let nfd_name = "file\u{0065}\u{0301}.txt";
    let nfc_name = "file\u{00e9}.txt";
    assert_ne!(nfd_name, nfc_name);
    assert_ne!(nfd_name.as_bytes(), nfc_name.as_bytes());

    std::fs::write(dir.path().join(nfd_name), "content").unwrap();

    let cwd = dir.path().to_string_lossy();
    let result = resolve_read_path(nfc_name, &cwd);
    assert!(result.contains(cwd.as_ref()));
    assert!(result.ends_with(".txt"));
}

#[test]
fn resolve_read_path_handles_curly_quotes_vs_straight_quotes() {
    let dir = TempDir::new().unwrap();
    let curly_name = "Capture d\u{2019}cran.txt";
    let straight_name = "Capture d'cran.txt";
    assert_ne!(curly_name, straight_name);

    let curly_path = dir.path().join(curly_name);
    std::fs::write(&curly_path, "content").unwrap();

    let cwd = dir.path().to_string_lossy();
    let result = resolve_read_path(straight_name, &cwd);
    assert_eq!(result, curly_path.to_string_lossy());
}

#[test]
fn resolve_read_path_handles_combined_nfc_and_curly_quote() {
    let dir = TempDir::new().unwrap();
    let nfc_curly = "Capture d\u{2019}\u{00e9}cran.txt";
    let nfc_straight = "Capture d'\u{00e9}cran.txt";
    assert_ne!(nfc_curly, nfc_straight);

    let full = dir.path().join(nfc_curly);
    std::fs::write(&full, "content").unwrap();

    let cwd = dir.path().to_string_lossy();
    let result = resolve_read_path(nfc_straight, &cwd);
    assert_eq!(result, full.to_string_lossy());
}

#[test]
fn resolve_read_path_handles_macos_am_pm_narrow_no_break_space() {
    let dir = TempDir::new().unwrap();
    let macos_name = "Screenshot 2024-01-01 at 10.00.00\u{202F}AM.png";
    let user_name = "Screenshot 2024-01-01 at 10.00.00 AM.png";

    let full = dir.path().join(macos_name);
    std::fs::write(&full, "content").unwrap();

    let cwd = dir.path().to_string_lossy();
    let result = resolve_read_path(user_name, &cwd);
    assert_eq!(result, full.to_string_lossy());
}

#[test]
fn resolve_read_path_handles_macos_lowercase_am_pm() {
    let dir = TempDir::new().unwrap();
    let macos_name = "Screenshot 2024-01-01 at 10.00.00\u{202F}am.png";
    let user_name = "Screenshot 2024-01-01 at 10.00.00 am.png";

    let full = dir.path().join(macos_name);
    std::fs::write(&full, "content").unwrap();

    let cwd = dir.path().to_string_lossy();
    let result = resolve_read_path(user_name, &cwd);
    assert_eq!(result, full.to_string_lossy());
}

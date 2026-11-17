//! Port of packages/coding-agent/test/external-editor.test.ts

use std::fs;

use pi_coding_agent::modes::interactive::external_editor::{
    ExternalEditorOptions, ExternalEditorResult, edit_in_external_editor,
};
use pretty_assertions::assert_eq;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditorCapture {
    file_path: String,
    content: String,
    entries: Vec<String>,
    directory_mode: u32,
}

async fn run_external_editor(fixture_flag: Option<&str>) -> (ExternalEditorResult, EditorCapture) {
    let test_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[
        &pi_js::env::tmp_dir(),
        "pi-external-editor-test-",
    ]))
    .unwrap();
    let capture_path = pi_js::path::join(&[&test_dir, "capture.json"]);
    let fixture_bin = env!("CARGO_BIN_EXE_fixture-fake-external-editor");
    let command = match fixture_flag {
        Some(flag) => format!("{fixture_bin} {capture_path} {flag}"),
        None => format!("{fixture_bin} {capture_path}"),
    };

    let _capture_guard = pi_js::console::testing::capture();
    let result = edit_in_external_editor(ExternalEditorOptions {
        command,
        content: "original".to_string(),
    })
    .await
    .unwrap();
    let capture_raw = fs::read_to_string(&capture_path).unwrap();
    let capture: EditorCapture = serde_json::from_str(&capture_raw).unwrap();
    let _ = pi_js::fs::rm(&test_dir, true, true);
    (result, capture)
}

#[tokio::test]
async fn edits_a_prompt_inside_a_private_temporary_directory() {
    let (result, capture) = run_external_editor(None).await;
    let directory = pi_js::path::dirname(&capture.file_path);

    assert_eq!(
        result,
        ExternalEditorResult::Complete {
            content: "edited".to_string()
        }
    );
    assert_eq!(pi_js::path::dirname(&directory), pi_js::env::tmp_dir());
    let dir_base = pi_js::path::basename(&directory, None);
    assert!(
        dir_base.starts_with("pi-editor-") && dir_base.len() > "pi-editor-".len(),
        "unexpected dir name: {dir_base}"
    );
    assert_eq!(pi_js::path::basename(&capture.file_path, None), "prompt.md");
    assert_eq!(capture.entries, vec!["prompt.md"]);
    assert_eq!(capture.content, "original");
    if pi_js::env::platform() != "win32" {
        assert_eq!(capture.directory_mode & 0o077, 0);
    }
    assert!(!pi_js::fs::exists(&directory));
}

#[tokio::test]
async fn keeps_the_original_content_when_the_editor_exits_unsuccessfully() {
    let (result, capture) = run_external_editor(Some("--fail")).await;

    assert_eq!(
        result,
        ExternalEditorResult::Failed {
            message: "External editor exited with code 1; prompt unchanged".to_string()
        }
    );
    assert!(!pi_js::fs::exists(&pi_js::path::dirname(&capture.file_path)));
}

#[tokio::test]
async fn returns_empty_content_when_the_editor_clears_the_prompt() {
    let (result, _) = run_external_editor(Some("--empty")).await;
    assert_eq!(result, ExternalEditorResult::Complete { content: String::new() });
}

// #11592: launch failures must be reported, not silently ignored.
#[cfg(not(windows))]
#[tokio::test]
async fn reports_an_editor_that_cannot_be_launched() {
    let _capture_guard = pi_js::console::testing::capture();
    let command = "pi-external-editor-does-not-exist";
    let result = edit_in_external_editor(ExternalEditorOptions {
        command: command.to_string(),
        content: "original".to_string(),
    })
    .await
    .unwrap();
    assert_eq!(
        result,
        ExternalEditorResult::Failed {
            message: format!("Failed to launch external editor: {command}")
        }
    );
}

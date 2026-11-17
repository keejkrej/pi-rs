//! Port of packages/coding-agent/test/block-images.test.ts

use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::tools::read::create_read_tool;
use serde_json::json;
use tempfile::TempDir;

const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

#[tokio::test]
async fn read_tool_always_reads_images() {
    let dir = TempDir::new().unwrap();
    let image_path = dir.path().join("test.png");
    std::fs::write(&image_path, pi_js::b64::decode(TINY_PNG_BASE64)).unwrap();

    let tool = create_read_tool(&dir.path().to_string_lossy(), None);
    let result = (tool.execute)(
        "test-1".into(),
        json!({ "path": image_path.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap();

    assert!(!result.content.is_empty());
    let has_image = result.content.iter().any(|c| matches!(c, UserContentBlock::Image(_)));
    assert!(has_image);
}

#[tokio::test]
async fn read_tool_reads_text_files_normally() {
    let dir = TempDir::new().unwrap();
    let text_path = dir.path().join("test.txt");
    std::fs::write(&text_path, "Hello, world!").unwrap();

    let tool = create_read_tool(&dir.path().to_string_lossy(), None);
    let result = (tool.execute)(
        "test-2".into(),
        json!({ "path": text_path.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.content.len(), 1);
    match &result.content[0] {
        UserContentBlock::Text(t) => assert!(t.text.contains("Hello, world!")),
        other => panic!("expected text content, got {other:?}"),
    }
}

// WIRE(@ca-cli-w3): cli::file_processor::process_file_arguments
// tests with SettingsManager.blockImages will be enabled once CLI file-processor lands.
#[cfg(any())]
mod pending_wire_tests {}

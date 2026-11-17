//! Port of packages/coding-agent/test/block-images.test.ts

use std::sync::{Mutex, OnceLock};

use pi_ai::types::UserContentBlock;
use pi_coding_agent::cli::file_processor::process_file_arguments;
use pi_coding_agent::core::tools::read::create_read_tool;
use serde_json::json;
use tempfile::TempDir;

const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

fn cwd_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn create_tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
    let mut buffer = vec![0u8; 58];
    buffer[0..2].copy_from_slice(b"BM");
    let total_len = buffer.len() as u32;
    buffer[2..6].copy_from_slice(&total_len.to_le_bytes());
    buffer[10..14].copy_from_slice(&54u32.to_le_bytes());
    buffer[14..18].copy_from_slice(&40u32.to_le_bytes());
    buffer[18..22].copy_from_slice(&1i32.to_le_bytes());
    buffer[22..26].copy_from_slice(&1i32.to_le_bytes());
    buffer[26..28].copy_from_slice(&1u16.to_le_bytes());
    buffer[28..30].copy_from_slice(&24u16.to_le_bytes());
    buffer[34..38].copy_from_slice(&4u32.to_le_bytes());
    buffer[56] = 0xff;
    buffer
}

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

#[tokio::test]
async fn process_file_arguments_processes_images_bmp_and_text_files() {
    let _guard = cwd_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = TempDir::new().unwrap();
    let png_path = dir.path().join("test.png");
    std::fs::write(&png_path, pi_js::b64::decode(TINY_PNG_BASE64)).unwrap();
    let png_result = process_file_arguments(&[png_path.to_string_lossy().into_owned()], None)
        .await
        .unwrap();
    assert_eq!(png_result.images.len(), 1);

    let bmp_path = dir.path().join("test.bmp");
    std::fs::write(&bmp_path, create_tiny_bmp_1x1_red_24bpp()).unwrap();
    let bmp_result = process_file_arguments(&[bmp_path.to_string_lossy().into_owned()], None)
        .await
        .unwrap();
    assert_eq!(bmp_result.images.len(), 1);
    assert_eq!(bmp_result.images[0].mime_type, "image/png");
    assert!(
        bmp_result
            .text
            .contains("[Image converted from image/bmp to image/png.]")
    );

    let text_path = dir.path().join("test.txt");
    std::fs::write(&text_path, "Hello, world!").unwrap();
    let text_result = process_file_arguments(&[text_path.to_string_lossy().into_owned()], None)
        .await
        .unwrap();
    assert!(text_result.images.is_empty());
    assert!(text_result.text.contains("Hello, world!"));
}

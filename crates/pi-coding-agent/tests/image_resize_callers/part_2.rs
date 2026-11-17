//! Port of packages/coding-agent/test/image-resize-callers.test.ts (unit coding-agent-cli-args-auth-migrations)

use pi_coding_agent::cli::file_processor::{ProcessFileOptions, process_file_arguments};

const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

/// PNG signature + valid 13-byte IHDR chunk header with truncated/corrupt body so mime sniffing
/// detects `image/png` while in-process image decoding/resizing fails cleanly and returns `None`.
const UNDECODABLE_PNG_BYTES: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
    0x00, 0x00, 0x00, 0x0d, // IHDR length = 13
    0x49, 0x48, 0x44, 0x52, // "IHDR"
    0x00, 0x00, 0x00, 0x01, // width = 1
    0x00, 0x00, 0x00, 0x01, // height = 1
    0x08, 0x02, 0x00, 0x00, 0x00, // bit depth, color type, etc.
    0x90, 0x77, 0x53, 0xde, // IHDR CRC (no IDAT/IEND chunks)
];

#[tokio::test]
async fn file_processor_omits_image_attachments_when_auto_resize_cannot_produce_a_safe_image() {
    let dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "image-resize-callers-"])).unwrap();
    let image_path = pi_js::path::join(&[&dir, "corrupt.png"]);
    pi_js::fs::write_file(&image_path, UNDECODABLE_PNG_BYTES, None).unwrap();

    let result = process_file_arguments(&[image_path], None).await.unwrap();
    let _ = pi_js::fs::rm(&dir, true, true);

    assert!(result.images.is_empty());
    assert!(result.text.contains("Image omitted"));
}

#[tokio::test]
async fn can_defer_resizing_file_attachments_until_prompt_dispatch() {
    let dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "image-resize-callers-"])).unwrap();
    let image_path = pi_js::path::join(&[&dir, "test.png"]);
    let bytes = pi_js::b64::decode(TINY_PNG_BASE64);
    pi_js::fs::write_file(&image_path, &bytes, None).unwrap();

    // Even when auto-resize would otherwise run, disabling auto_resize_images skips resizing and keeps the raw image.
    let result = process_file_arguments(
        &[image_path.clone()],
        Some(ProcessFileOptions {
            auto_resize_images: Some(false),
        }),
    )
    .await
    .unwrap();
    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].mime_type, "image/png");

    // Also verify skipping resize even on an image that fails resizing preserves the raw attachment.
    let raw_unresized = pi_js::path::join(&[&dir, "deferred.png"]);
    pi_js::fs::write_file(&raw_unresized, UNDECODABLE_PNG_BYTES, None).unwrap();
    let deferred = process_file_arguments(
        &[raw_unresized],
        Some(ProcessFileOptions {
            auto_resize_images: Some(false),
        }),
    )
    .await
    .unwrap();
    let _ = pi_js::fs::rm(&dir, true, true);

    assert_eq!(deferred.images.len(), 1);
}

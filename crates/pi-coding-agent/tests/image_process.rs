//! Port of packages/coding-agent/test/image-process.test.ts

use pi_coding_agent::utils::image_process::{ProcessImageOptions, ProcessImageResult, process_image};
use pi_coding_agent::utils::mime::detect_supported_image_mime_type;

fn create_tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
    let mut buffer = vec![0u8; 58];
    buffer[0..2].copy_from_slice(b"BM");
    let len_bytes = (buffer.len() as u32).to_le_bytes();
    buffer[2..6].copy_from_slice(&len_bytes);
    buffer[10..14].copy_from_slice(&54u32.to_le_bytes());
    buffer[14..18].copy_from_slice(&40u32.to_le_bytes());
    buffer[18..22].copy_from_slice(&1i32.to_le_bytes());
    buffer[22..26].copy_from_slice(&1i32.to_le_bytes());
    buffer[26..28].copy_from_slice(&1u16.to_le_bytes());
    buffer[28..30].copy_from_slice(&24u16.to_le_bytes());
    buffer[30..34].copy_from_slice(&0u32.to_le_bytes());
    buffer[34..38].copy_from_slice(&4u32.to_le_bytes());
    buffer[56] = 0xff;
    buffer
}

fn expect_png_magic(base64_data: &str) {
    let buffer = pi_js::b64::decode(base64_data);
    assert_eq!(buffer[0], 0x89);
    assert_eq!(buffer[1], 0x50);
    assert_eq!(buffer[2], 0x4e);
    assert_eq!(buffer[3], 0x47);
}

mod image_processing_pipeline {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn detects_the_complete_gif_signature() {
        for signature in ["GIF87a", "GIF89a"] {
            assert_eq!(
                detect_supported_image_mime_type(signature.as_bytes()),
                Some("image/gif"),
                "signature {signature}"
            );
        }
    }

    #[test]
    fn detects_bmp_files_from_magic_bytes() {
        assert_eq!(
            detect_supported_image_mime_type(&create_tiny_bmp_1x1_red_24bpp()),
            Some("image/bmp")
        );
    }

    #[tokio::test]
    async fn converts_bmp_files_to_png_attachments_when_auto_resize_is_disabled() {
        let result = process_image(
            &create_tiny_bmp_1x1_red_24bpp(),
            "image/bmp",
            Some(ProcessImageOptions {
                auto_resize_images: Some(false),
                resize_options: None,
            }),
        )
        .await;

        match result {
            ProcessImageResult::Ok { data, mime_type, hints } => {
                assert_eq!(mime_type, "image/png");
                assert!(hints.contains(&"[Image converted from image/bmp to image/png.]".to_owned()));
                expect_png_magic(&data);
            }
            ProcessImageResult::Err { message } => panic!("unexpected error: {message}"),
        }
    }

    #[tokio::test]
    async fn converts_bmp_files_before_auto_resizing() {
        let result = process_image(&create_tiny_bmp_1x1_red_24bpp(), "image/bmp", None).await;

        match result {
            ProcessImageResult::Ok { data, mime_type, hints } => {
                assert_eq!(mime_type, "image/png");
                assert!(hints.contains(&"[Image converted from image/bmp to image/png.]".to_owned()));
                expect_png_magic(&data);
            }
            ProcessImageResult::Err { message } => panic!("unexpected error: {message}"),
        }
    }
}

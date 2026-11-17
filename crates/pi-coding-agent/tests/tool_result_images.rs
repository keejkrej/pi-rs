//! Port of packages/coding-agent/test/tool-result-images.test.ts

use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, ImageFormat};
use pi_ai::{ImageContent, TextContent, UserContentBlock};
use pi_coding_agent::utils::tool_result_images::{NormalizeToolResultImagesOptions, normalize_tool_result_images};

const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

fn create_png(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height) as usize];
    for row in 0..height {
        let start = (row * width) as usize;
        let end = ((row + 1) * width) as usize;
        pixels[start..end].fill((row % 256) as u8);
    }
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&pixels, width, height, ColorType::L8.into())
        .expect("encode grayscale PNG");
    out
}

fn read_png_dimensions(base64_data: &str) -> (u32, u32) {
    let bytes = pi_js::b64::decode(base64_data);
    let img = image::load_from_memory_with_format(&bytes, ImageFormat::Png).expect("decode PNG");
    (img.width(), img.height())
}

fn create_tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
    let mut buf = vec![0u8; 58];
    buf[0..2].copy_from_slice(b"BM");
    let total_len = buf.len() as u32;
    buf[2..6].copy_from_slice(&total_len.to_le_bytes());
    buf[10..14].copy_from_slice(&54u32.to_le_bytes());
    buf[14..18].copy_from_slice(&40u32.to_le_bytes());
    buf[18..22].copy_from_slice(&1i32.to_le_bytes());
    buf[22..26].copy_from_slice(&1i32.to_le_bytes());
    buf[26..28].copy_from_slice(&1u16.to_le_bytes());
    buf[28..30].copy_from_slice(&24u16.to_le_bytes());
    buf[30..34].copy_from_slice(&0u32.to_le_bytes());
    buf[34..38].copy_from_slice(&4u32.to_le_bytes());
    buf[56] = 0xff;
    buf
}

fn image_block(bytes: Vec<u8>, mime_type: &str) -> UserContentBlock {
    UserContentBlock::Image(ImageContent::new(pi_js::b64::encode(&bytes), mime_type))
}

#[tokio::test]
async fn returns_original_blocks_when_no_images() {
    let content = vec![UserContentBlock::Text(TextContent::new("no images here"))];
    let normalized = normalize_tool_result_images(content.clone(), None).await;
    assert_eq!(normalized, content);
}

#[tokio::test]
async fn returns_original_blocks_when_images_already_within_limits() {
    let content = vec![
        UserContentBlock::Text(TextContent::new("screenshot")),
        UserContentBlock::Image(ImageContent::new(TINY_PNG_BASE64, "image/png")),
    ];
    let normalized = normalize_tool_result_images(content.clone(), None).await;
    assert_eq!(normalized, content);
}

#[tokio::test]
async fn resizes_oversized_images_and_reports_original_dimensions() {
    let content = vec![image_block(create_png(2400, 4800), "image/png")];
    let normalized = normalize_tool_result_images(content, None).await;

    assert_eq!(normalized.len(), 2);
    match &normalized[0] {
        UserContentBlock::Image(img) => {
            let (width, height) = read_png_dimensions(&img.data);
            assert!(width <= 2000, "width={width}");
            assert!(height <= 2000, "height={height}");
        }
        other => panic!("expected image block, got {other:?}"),
    }
    match &normalized[1] {
        UserContentBlock::Text(text) => assert!(text.text.contains("original 2400x4800"), "{}", text.text),
        other => panic!("expected text block, got {other:?}"),
    }
}

#[tokio::test]
async fn leaves_oversized_images_alone_when_auto_resize_disabled() {
    let content = vec![image_block(create_png(2400, 4800), "image/png")];
    let normalized = normalize_tool_result_images(
        content.clone(),
        Some(NormalizeToolResultImagesOptions {
            auto_resize_images: Some(false),
            resize_options: None,
        }),
    )
    .await;
    assert_eq!(normalized, content);
}

#[tokio::test]
async fn converts_unsupported_image_formats_even_when_auto_resize_disabled() {
    let content = vec![image_block(create_tiny_bmp_1x1_red_24bpp(), "image/bmp")];
    let normalized = normalize_tool_result_images(
        content,
        Some(NormalizeToolResultImagesOptions {
            auto_resize_images: Some(false),
            resize_options: None,
        }),
    )
    .await;

    assert_eq!(normalized.len(), 2);
    match &normalized[0] {
        UserContentBlock::Image(img) => assert_eq!(img.mime_type, "image/png"),
        other => panic!("expected image block, got {other:?}"),
    }
    match &normalized[1] {
        UserContentBlock::Text(text) => {
            assert_eq!(text.text, "[Image converted from image/bmp to image/png.]");
        }
        other => panic!("expected text block, got {other:?}"),
    }
}

#[tokio::test]
async fn keeps_undecodable_images_instead_of_dropping_output() {
    let content = vec![UserContentBlock::Image(ImageContent::new(
        "bm90LWFuLWltYWdl",
        "image/png",
    ))];
    let normalized = normalize_tool_result_images(content.clone(), None).await;
    assert_eq!(normalized, content);
}

#[tokio::test]
async fn preserves_surrounding_text_blocks_and_their_order() {
    let content = vec![
        UserContentBlock::Text(TextContent::new("before")),
        image_block(create_png(2400, 100), "image/png"),
        UserContentBlock::Text(TextContent::new("after")),
    ];
    let normalized = normalize_tool_result_images(content, None).await;

    assert_eq!(normalized.len(), 4);
    assert!(matches!(&normalized[0], UserContentBlock::Text(t) if t.text == "before"));
    assert!(matches!(&normalized[1], UserContentBlock::Image(_)));
    assert!(matches!(&normalized[2], UserContentBlock::Text(_)));
    assert!(matches!(&normalized[3], UserContentBlock::Text(t) if t.text == "after"));
}

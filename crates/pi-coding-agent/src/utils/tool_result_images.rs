//! Port of packages/coding-agent/src/utils/tool-result-images.ts

use pi_ai::{ImageContent, ModelImageResizeOptions, TextContent, UserContentBlock};

use crate::utils::image_process::{ProcessImageOptions, ProcessImageResult, process_image};
use crate::utils::image_resize::ImageResizeOptions;

pub type ToolResultContent = UserContentBlock;

#[derive(Clone, Debug, Default)]
pub struct NormalizeToolResultImagesOptions {
    /// Whether oversized images are resized to inline provider limits. Default: true
    pub auto_resize_images: Option<bool>,
    /// Model-specific resize profile. Uses the conservative built-in defaults when omitted.
    pub resize_options: Option<ModelImageResizeOptions>,
}

/// Normalize image blocks returned by tool results.
pub async fn normalize_tool_result_images(
    content: Vec<ToolResultContent>,
    options: Option<NormalizeToolResultImagesOptions>,
) -> Vec<ToolResultContent> {
    if !content.iter().any(|b| matches!(b, UserContentBlock::Image(_))) {
        return content;
    }

    let opts = options.unwrap_or_default();
    let auto_resize_images = opts.auto_resize_images.unwrap_or(true);
    let resize_options = opts.resize_options.as_ref().map(|r| ImageResizeOptions {
        max_width: r.max_width.map(|v| v.max(1) as u32),
        max_height: r.max_height.map(|v| v.max(1) as u32),
        max_bytes: r.max_bytes.map(|v| v.max(0) as usize),
        jpeg_quality: r.jpeg_quality.map(|v| v.clamp(1.0, 100.0).round() as u8),
    });

    let mut normalized = Vec::with_capacity(content.len());
    let mut changed = false;

    for block in content {
        let UserContentBlock::Image(img) = block else {
            normalized.push(block);
            continue;
        };

        let bytes = pi_js::b64::decode(&img.data);
        let processed = process_image(
            &bytes,
            &img.mime_type,
            Some(ProcessImageOptions {
                auto_resize_images: Some(auto_resize_images),
                resize_options: resize_options.clone(),
            }),
        )
        .await;

        match processed {
            ProcessImageResult::Err { .. } => {
                normalized.push(UserContentBlock::Image(img));
            }
            ProcessImageResult::Ok { data, mime_type, hints } => {
                if data == img.data && mime_type == img.mime_type && hints.is_empty() {
                    normalized.push(UserContentBlock::Image(img));
                    continue;
                }
                normalized.push(UserContentBlock::Image(ImageContent::new(data, mime_type)));
                if !hints.is_empty() {
                    normalized.push(UserContentBlock::Text(TextContent::new(hints.join("\n"))));
                }
                changed = true;
            }
        }
    }

    let _ = changed;
    normalized
}

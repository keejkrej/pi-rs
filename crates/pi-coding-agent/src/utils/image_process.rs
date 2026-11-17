//! Port of packages/coding-agent/src/utils/image-process.ts

use crate::utils::image_convert::convert_image_bytes_to_png;
use crate::utils::image_resize::{ImageResizeOptions, format_dimension_note, resize_image};

#[derive(Clone, Debug, Default)]
pub struct ProcessImageOptions {
    /// Whether to resize images to inline provider limits. Default: true
    pub auto_resize_images: Option<bool>,
    /// Optional resize overrides. Uses resizeImage defaults when omitted.
    pub resize_options: Option<ImageResizeOptions>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessImageResult {
    Ok {
        data: String,
        mime_type: String,
        hints: Vec<String>,
    },
    Err {
        message: String,
    },
}

struct NormalizedImage {
    bytes: Vec<u8>,
    mime_type: String,
    converted_from: Option<String>,
}

fn base_mime_type(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .unwrap_or(mime_type)
        .trim()
        .to_ascii_lowercase()
}

fn normalize_supported_image_mime_type(mime_type: &str) -> Option<&'static str> {
    match base_mime_type(mime_type).as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

async fn normalize_image(bytes: &[u8], mime_type: &str) -> Option<NormalizedImage> {
    if let Some(normalized_mime) = normalize_supported_image_mime_type(mime_type) {
        return Some(NormalizedImage {
            bytes: bytes.to_vec(),
            mime_type: normalized_mime.to_owned(),
            converted_from: None,
        });
    }

    let png_bytes = convert_image_bytes_to_png(bytes).await?;
    Some(NormalizedImage {
        bytes: png_bytes,
        mime_type: "image/png".to_owned(),
        converted_from: Some(base_mime_type(mime_type)),
    })
}

fn conversion_hint(from: Option<&str>, to: &str) -> Option<String> {
    let from = from?;
    if from == to {
        return None;
    }
    Some(format!("[Image converted from {from} to {to}.]"))
}

pub async fn process_image(bytes: &[u8], mime_type: &str, options: Option<ProcessImageOptions>) -> ProcessImageResult {
    let opts = options.unwrap_or_default();
    let auto_resize_images = opts.auto_resize_images.unwrap_or(true);

    let Some(normalized) = normalize_image(bytes, mime_type).await else {
        return ProcessImageResult::Err {
            message: "[Image omitted: could not be converted to a supported inline image format.]".to_owned(),
        };
    };

    if auto_resize_images {
        let Some(resized) = resize_image(&normalized.bytes, &normalized.mime_type, opts.resize_options).await else {
            return ProcessImageResult::Err {
                message: "[Image omitted: could not be resized below the inline image size limit.]".to_owned(),
            };
        };

        let mut hints = Vec::new();
        if let Some(converted_hint) = conversion_hint(normalized.converted_from.as_deref(), &resized.mime_type) {
            hints.push(converted_hint);
        }
        if let Some(dimension_note) = format_dimension_note(&resized) {
            hints.push(dimension_note);
        }

        return ProcessImageResult::Ok {
            data: resized.data,
            mime_type: resized.mime_type,
            hints,
        };
    }

    let mut hints = Vec::new();
    if let Some(converted_hint) = conversion_hint(normalized.converted_from.as_deref(), &normalized.mime_type) {
        hints.push(converted_hint);
    }

    ProcessImageResult::Ok {
        data: pi_js::b64::encode(&normalized.bytes),
        mime_type: normalized.mime_type,
        hints,
    }
}

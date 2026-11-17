//! Port of packages/coding-agent/src/utils/image-resize.ts (PORTING.md §2.6: tokio::task::spawn_blocking).

pub use crate::utils::image_resize_core::{
    ImageResizeOptions, ResizedImage, resize_image_in_process, resize_image_in_process_sync,
};

/// Resize an image to fit within the specified max dimensions and encoded file size.
/// Runs on Tokio's blocking pool so CPU-bound decoding/resizing does not block the async runtime.
pub async fn resize_image(
    input_bytes: &[u8],
    mime_type: &str,
    options: Option<ImageResizeOptions>,
) -> Option<ResizedImage> {
    let owned_bytes = input_bytes.to_vec();
    let owned_mime = mime_type.to_owned();
    let fallback_options = options.clone();
    match tokio::task::spawn_blocking(move || resize_image_in_process_sync(&owned_bytes, &owned_mime, options)).await {
        Ok(result) => result,
        Err(_) => resize_image_in_process(input_bytes, mime_type, fallback_options).await,
    }
}

/// Format a dimension note for resized images.
/// This helps the model understand the coordinate mapping.
pub fn format_dimension_note(result: &ResizedImage) -> Option<String> {
    if !result.was_resized {
        return None;
    }

    let scale = f64::from(result.original_width) / f64::from(result.width);
    Some(format!(
        "[Image: original {}x{}, displayed at {}x{}. Multiply coordinates by {} to map to original image.]",
        result.original_width,
        result.original_height,
        result.width,
        result.height,
        pi_js::num::to_fixed(scale, 2),
    ))
}

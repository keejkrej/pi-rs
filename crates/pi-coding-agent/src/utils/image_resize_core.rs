//! Port of packages/coding-agent/src/utils/image-resize-core.ts

use crate::utils::exif_orientation::apply_exif_orientation;
use crate::utils::photon::{PhotonBackend, PhotonImage, SamplingFilter, load_photon, resize};

// 4.5MB of base64 payload. Provides headroom below Anthropic's 5MB limit.
const DEFAULT_MAX_BYTES: usize = 4_718_592; // 4.5 * 1024 * 1024

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImageResizeOptions {
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub max_bytes: Option<usize>,
    pub jpeg_quality: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResizedImage {
    pub data: String,
    pub mime_type: String,
    pub original_width: u32,
    pub original_height: u32,
    pub width: u32,
    pub height: u32,
    pub was_resized: bool,
}

struct EncodedCandidate {
    data: String,
    encoded_size: usize,
    mime_type: &'static str,
}

fn encode_candidate(buffer: &[u8], mime_type: &'static str) -> EncodedCandidate {
    let data = pi_js::b64::encode(buffer);
    let encoded_size = data.len();
    EncodedCandidate {
        data,
        encoded_size,
        mime_type,
    }
}

/// Synchronously resize an image to fit within the specified max dimensions and encoded file size.
pub fn resize_image_in_process_sync(
    input_bytes: &[u8],
    mime_type: &str,
    options: Option<ImageResizeOptions>,
) -> Option<ResizedImage> {
    let opts = options.unwrap_or_default();
    let max_width = opts.max_width.unwrap_or(2000);
    let max_height = opts.max_height.unwrap_or(2000);
    let max_bytes = opts.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let jpeg_quality = opts.jpeg_quality.unwrap_or(80);

    let input_base64_size = input_bytes.len().div_ceil(3) * 4;
    let photon = PhotonBackend;

    let raw_image = PhotonImage::new_from_byteslice(input_bytes).ok()?;
    let image = apply_exif_orientation(&photon, raw_image, input_bytes);
    let original_width = image.get_width();
    let original_height = image.get_height();
    let format = mime_type.split('/').nth(1).unwrap_or("png");

    if original_width <= max_width && original_height <= max_height && input_base64_size < max_bytes {
        let out_mime = if mime_type.is_empty() {
            format!("image/{format}")
        } else {
            mime_type.to_owned()
        };
        return Some(ResizedImage {
            data: pi_js::b64::encode(input_bytes),
            mime_type: out_mime,
            original_width,
            original_height,
            width: original_width,
            height: original_height,
            was_resized: false,
        });
    }

    let mut target_width = original_width;
    let mut target_height = original_height;

    if target_width > max_width && target_width > 0 {
        target_height = (f64::from(target_height) * f64::from(max_width) / f64::from(target_width)).round() as u32;
        target_width = max_width;
    }
    if target_height > max_height && target_height > 0 {
        target_width = (f64::from(target_width) * f64::from(max_height) / f64::from(target_height)).round() as u32;
        target_height = max_height;
    }
    target_width = target_width.max(1);
    target_height = target_height.max(1);

    let mut quality_steps = Vec::new();
    for q in [jpeg_quality, 85, 70, 55, 40] {
        if !quality_steps.contains(&q) {
            quality_steps.push(q);
        }
    }

    let mut current_width = target_width;
    let mut current_height = target_height;

    loop {
        let resized = resize(&image, current_width, current_height, SamplingFilter::Lanczos3);
        let png = resized.get_bytes().ok().map(|b| encode_candidate(&b, "image/png"));
        let mut jpegs = Vec::new();
        for &q in &quality_steps {
            if let Ok(jpg) = resized.get_bytes_jpeg(q) {
                jpegs.push(encode_candidate(&jpg, "image/jpeg"));
            }
        }
        let mut ordered = Vec::with_capacity(1 + jpegs.len());
        let mut jpeg_iter = jpegs.into_iter();
        let first_jpeg = jpeg_iter.next();
        match (png, first_jpeg) {
            (Some(p), Some(j)) => {
                // Prefer the smaller of PNG and JPEG at the configured quality; lower JPEG quality only if neither fits.
                if j.encoded_size < p.encoded_size {
                    ordered.push(j);
                    ordered.push(p);
                } else {
                    ordered.push(p);
                    ordered.push(j);
                }
            }
            (Some(p), None) => ordered.push(p),
            (None, Some(j)) => ordered.push(j),
            (None, None) => {}
        }
        ordered.extend(jpeg_iter);
        for candidate in ordered {
            if candidate.encoded_size < max_bytes {
                return Some(ResizedImage {
                    data: candidate.data,
                    mime_type: candidate.mime_type.to_owned(),
                    original_width,
                    original_height,
                    width: current_width,
                    height: current_height,
                    was_resized: true,
                });
            }
        }

        if current_width == 1 && current_height == 1 {
            break;
        }

        let next_width = if current_width == 1 {
            1
        } else {
            ((f64::from(current_width) * 0.75).floor() as u32).max(1)
        };
        let next_height = if current_height == 1 {
            1
        } else {
            ((f64::from(current_height) * 0.75).floor() as u32).max(1)
        };
        if next_width == current_width && next_height == current_height {
            break;
        }

        current_width = next_width;
        current_height = next_height;
    }

    None
}

/// Resize an image to fit within the specified max dimensions and encoded file size.
pub async fn resize_image_in_process(
    input_bytes: &[u8],
    mime_type: &str,
    options: Option<ImageResizeOptions>,
) -> Option<ResizedImage> {
    let _photon = load_photon().await?;
    resize_image_in_process_sync(input_bytes, mime_type, options)
}

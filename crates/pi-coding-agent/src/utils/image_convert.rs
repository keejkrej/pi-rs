//! Port of packages/coding-agent/src/utils/image-convert.ts

use crate::utils::exif_orientation::apply_exif_orientation;
use crate::utils::photon::{PhotonImage, load_photon};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConvertedPng {
    pub data: String,
    pub mime_type: String,
}

pub async fn convert_image_bytes_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let photon = load_photon().await?;
    let raw_image = PhotonImage::new_from_byteslice(bytes).ok()?;
    let image = apply_exif_orientation(&photon, raw_image, bytes);
    image.get_bytes().ok()
}

/// Convert image to PNG format for terminal display.
/// Kitty graphics protocol requires PNG format (f=100).
pub async fn convert_to_png(base64_data: &str, mime_type: &str) -> Option<ConvertedPng> {
    if mime_type == "image/png" {
        return Some(ConvertedPng {
            data: base64_data.to_owned(),
            mime_type: mime_type.to_owned(),
        });
    }

    let bytes = pi_js::b64::decode(base64_data);
    let png_bytes = convert_image_bytes_to_png(&bytes).await?;
    Some(ConvertedPng {
        data: pi_js::b64::encode(&png_bytes),
        mime_type: "image/png".to_owned(),
    })
}

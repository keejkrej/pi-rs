//! Port of packages/coding-agent/src/utils/image-convert.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pi_tui::terminal_image::{ImageProtocol, get_capabilities};
use pi_tui::{ImageTranscoder, set_image_transcoder};

use crate::utils::exif_orientation::apply_exif_orientation;
use crate::utils::photon::{PhotonBackend, PhotonImage, load_photon};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConvertedPng {
    pub data: String,
    pub mime_type: String,
}

fn encode_png(photon: &PhotonBackend, bytes: &[u8]) -> Option<Vec<u8>> {
    let raw_image = PhotonImage::new_from_byteslice(bytes).ok()?;
    let image = apply_exif_orientation(photon, raw_image, bytes);
    image.get_bytes().ok()
}

pub async fn convert_image_bytes_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let photon = load_photon().await?;
    encode_png(&photon, bytes)
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

/// Load photon and return a synchronous PNG transcoder for `pi_tui`'s Kitty image rendering.
pub async fn load_png_transcoder() -> Option<ImageTranscoder> {
    let photon = load_photon().await?;
    Some(Arc::new(move |base64_data: &str, _mime_type: &str| {
        let bytes = pi_js::b64::decode(base64_data);
        let png_bytes = encode_png(&photon, &bytes)?;
        Some(pi_js::b64::encode(&png_bytes))
    }))
}

static PNG_TRANSCODER_REGISTERED: AtomicBool = AtomicBool::new(false);

/// On Kitty-protocol terminals, register photon as `pi_tui`'s image transcoder so non-PNG images render.
pub fn ensure_png_transcoder<F: FnOnce() + Send + 'static>(on_registered: F) {
    if PNG_TRANSCODER_REGISTERED.load(Ordering::Acquire) || get_capabilities().images != Some(ImageProtocol::Kitty) {
        return;
    }
    let transcoder: ImageTranscoder = Arc::new(|base64_data: &str, _mime_type: &str| {
        let bytes = pi_js::b64::decode(base64_data);
        let png_bytes = encode_png(&PhotonBackend, &bytes)?;
        Some(pi_js::b64::encode(&png_bytes))
    });
    set_image_transcoder(Some(transcoder));
    if !PNG_TRANSCODER_REGISTERED.swap(true, Ordering::AcqRel) {
        on_registered();
    }
}

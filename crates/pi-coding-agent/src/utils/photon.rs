//! Port of packages/coding-agent/src/utils/photon.ts (PORTING.md §2.6: image crate backend).

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageBuffer, ImageEncoder, ImageFormat, Rgba};
use pi_js::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamplingFilter {
    Nearest = 1,
    Triangle = 2,
    CatmullRom = 3,
    Gaussian = 4,
    Lanczos3 = 5,
}

impl From<SamplingFilter> for FilterType {
    fn from(value: SamplingFilter) -> Self {
        match value {
            SamplingFilter::Nearest => FilterType::Nearest,
            SamplingFilter::Triangle => FilterType::Triangle,
            SamplingFilter::CatmullRom => FilterType::CatmullRom,
            SamplingFilter::Gaussian => FilterType::Gaussian,
            SamplingFilter::Lanczos3 => FilterType::Lanczos3,
        }
    }
}

#[derive(Clone)]
pub struct PhotonImage {
    img: DynamicImage,
}

impl PhotonImage {
    pub fn new(raw_pixels: Vec<u8>, width: u32, height: u32) -> Result<Self> {
        let buf: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_raw(width, height, raw_pixels)
            .ok_or_else(|| Error::msg("Invalid RGBA buffer dimensions"))?;
        Ok(Self {
            img: DynamicImage::ImageRgba8(buf),
        })
    }

    pub fn new_from_byteslice(bytes: &[u8]) -> Result<Self> {
        let img = image::load_from_memory(bytes).map_err(|err| Error::msg(format!("Failed to decode image: {err}")))?;
        Ok(Self { img })
    }

    pub fn get_width(&self) -> u32 {
        self.img.width()
    }

    pub fn get_height(&self) -> u32 {
        self.img.height()
    }

    pub fn get_raw_pixels(&self) -> Vec<u8> {
        self.img.to_rgba8().into_raw()
    }

    pub fn get_bytes(&self) -> Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        self.img
            .write_to(&mut cursor, ImageFormat::Png)
            .map_err(|err| Error::msg(format!("Failed to encode PNG: {err}")))?;
        Ok(cursor.into_inner())
    }

    pub fn get_bytes_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        let rgb = self.img.to_rgb8();
        let (w, h) = rgb.dimensions();
        let mut buf = Vec::new();
        JpegEncoder::new_with_quality(&mut buf, quality)
            .write_image(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .map_err(|err| Error::msg(format!("Failed to encode JPEG: {err}")))?;
        Ok(buf)
    }

    pub fn fliph(&mut self) {
        self.img = self.img.fliph();
    }

    pub fn flipv(&mut self) {
        self.img = self.img.flipv();
    }

    pub fn resize(&self, width: u32, height: u32, filter: SamplingFilter) -> Self {
        Self {
            img: self.img.resize_exact(width, height, filter.into()),
        }
    }

    /// No-op in Rust (explicit WASM free compatibility helper).
    pub fn free(self) {}
}

pub type PhotonImageType = PhotonImage;

pub fn fliph(image: &mut PhotonImage) {
    image.fliph();
}

pub fn flipv(image: &mut PhotonImage) {
    image.flipv();
}

pub fn resize(image: &PhotonImage, width: u32, height: u32, filter: SamplingFilter) -> PhotonImage {
    image.resize(width, height, filter)
}

#[derive(Clone, Copy, Debug)]
pub struct PhotonBackend;

pub async fn load_photon() -> Option<PhotonBackend> {
    Some(PhotonBackend)
}

impl PhotonBackend {
    pub fn fliph(&self, image: &mut PhotonImage) {
        image.fliph();
    }

    pub fn flipv(&self, image: &mut PhotonImage) {
        image.flipv();
    }

    pub fn resize(&self, image: &PhotonImage, width: u32, height: u32, filter: SamplingFilter) -> PhotonImage {
        image.resize(width, height, filter)
    }
}

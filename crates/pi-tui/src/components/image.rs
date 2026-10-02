//! Port of packages/tui/src/components/image.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use crate::terminal_image::ImageDimensions;
use crate::tui::Component;

pub struct ImageTheme {
    pub fallback_color: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

#[derive(Clone, Debug, Default)]
pub struct ImageOptions {
    pub max_width_cells: Option<i64>,
    pub max_height_cells: Option<i64>,
    pub filename: Option<String>,
    /// Kitty image ID. If provided, reuses this ID (for animations/updates).
    pub image_id: Option<i64>,
}

struct ImageState {
    base64_data: String,
    mime_type: String,
    dimensions: ImageDimensions,
    theme: ImageTheme,
    options: ImageOptions,
    image_id: Option<i64>,
    cached_lines: Option<Vec<String>>,
    cached_width: Option<usize>,
}

struct ImageInner {
    state: Mutex<ImageState>,
}

/// PORT: TS class with identity. Handle so containers can store `Arc<dyn Component>`.
#[derive(Clone)]
pub struct Image {
    inner: Arc<ImageInner>,
}

impl Image {
    pub fn new(
        base64_data: &str,
        mime_type: &str,
        theme: ImageTheme,
        options: Option<ImageOptions>,
        dimensions: Option<ImageDimensions>,
    ) -> Self {
        todo!("port: Image::new")
    }

    /// Get the Kitty image ID used by this image (if any).
    pub fn get_image_id(&self) -> Option<i64> {
        self.inner.state.lock().unwrap().image_id
    }

    pub fn invalidate(&self) {
        todo!("port: Image::invalidate")
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: Image::render")
    }
}

impl Component for Image {
    fn render(&self, width: usize) -> Vec<String> {
        Image::render(self, width)
    }

    fn invalidate(&self) {
        Image::invalidate(self)
    }
}

//! Port of packages/tui/src/components/image.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;

use crate::terminal_image::{
    ImageDimensions, ImageProtocol, ImageRenderOptions, allocate_image_id, get_capabilities, get_cell_dimensions,
    get_image_dimensions, get_png_dimensions, image_fallback, render_image,
};
use crate::tui::Component;
use crate::utils::truncate_to_width;

pub type ImageTranscoder = Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;

static IMAGE_TRANSCODER: LazyLock<Mutex<Option<ImageTranscoder>>> = LazyLock::new(|| Mutex::new(None));
static PNG_CACHE: LazyLock<Mutex<IndexMap<String, Option<String>>>> = LazyLock::new(|| Mutex::new(IndexMap::new()));

/// Register the converter used for non-PNG images on Kitty-protocol terminals, which only accept PNG.
/// Without one, such images render as text fallbacks.
pub fn set_image_transcoder(transcoder: Option<ImageTranscoder>) {
    *IMAGE_TRANSCODER.lock().unwrap() = transcoder;
    PNG_CACHE.lock().unwrap().clear();
}

fn to_png(base64_data: &str, mime_type: &str) -> Option<String> {
    let transcoder = IMAGE_TRANSCODER.lock().unwrap().clone()?;
    {
        let mut cache = PNG_CACHE.lock().unwrap();
        if let Some(cached) = cache.shift_remove(base64_data) {
            cache.insert(base64_data.to_string(), cached.clone());
            return cached;
        }
    }
    let png = transcoder(base64_data, mime_type);
    let mut cache = PNG_CACHE.lock().unwrap();
    cache.shift_remove(base64_data);
    cache.insert(base64_data.to_string(), png.clone());
    if cache.len() > 32 {
        cache.shift_remove_index(0);
    }
    png
}

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
    png_data: Option<String>,
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
        let opts = options.unwrap_or_default();
        let resolved_dimensions = dimensions
            .or_else(|| get_image_dimensions(base64_data, mime_type))
            .unwrap_or(ImageDimensions {
                width_px: 800,
                height_px: 600,
            });
        let image_id = opts.image_id;
        Self {
            inner: Arc::new(ImageInner {
                state: Mutex::new(ImageState {
                    base64_data: base64_data.to_string(),
                    mime_type: mime_type.to_string(),
                    dimensions: resolved_dimensions,
                    theme,
                    options: opts,
                    image_id,
                    png_data: None,
                    cached_lines: None,
                    cached_width: None,
                }),
            }),
        }
    }

    /// Get the Kitty image ID used by this image (if any).
    pub fn get_image_id(&self) -> Option<i64> {
        self.inner.state.lock().unwrap().image_id
    }

    pub fn invalidate(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.cached_lines = None;
        state.cached_width = None;
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let (base64_data, mime_type, mut dimensions, fallback_color, options, mut image_id, mut png_data) = {
            let state = self.inner.state.lock().unwrap();
            if let Some(cached) = &state.cached_lines
                && state.cached_width == Some(width)
            {
                return cached.clone();
            }
            (
                state.base64_data.clone(),
                state.mime_type.clone(),
                state.dimensions,
                Arc::clone(&state.theme.fallback_color),
                state.options.clone(),
                state.image_id,
                state.png_data.clone(),
            )
        };

        let max_width = ((width as i64 - 2).min(options.max_width_cells.unwrap_or(60))).max(1);
        let cell_dimensions = get_cell_dimensions();
        let default_max_height = (((max_width as f64) * (cell_dimensions.width_px as f64))
            / (cell_dimensions.height_px as f64))
            .ceil()
            .max(1.0) as i64;
        let max_height = options.max_height_cells.unwrap_or(default_max_height);

        let caps = get_capabilities();
        let mut data: Option<String> = Some(base64_data.clone());
        let orig_dimensions = dimensions;
        if caps.images == Some(ImageProtocol::Kitty) && mime_type != "image/png" {
            if png_data.is_none() {
                png_data = to_png(&base64_data, &mime_type);
            }
            data = png_data.clone();
            if let Some(ref d) = data
                && let Some(png_dims) = get_png_dimensions(d)
            {
                dimensions = png_dims;
            }
        }

        let lines: Vec<String> = if let (Some(proto), Some(data)) = (caps.images, data) {
            if proto == ImageProtocol::Kitty && image_id.is_none() {
                image_id = Some(allocate_image_id());
            }
            let result = render_image(
                &data,
                dimensions,
                Some(ImageRenderOptions {
                    max_width_cells: Some(max_width),
                    max_height_cells: Some(max_height),
                    preserve_aspect_ratio: None,
                    image_id,
                    move_cursor: Some(false),
                }),
            );
            if let Some(result) = result {
                if let Some(id) = result.image_id
                    && id != 0
                {
                    image_id = Some(id);
                }
                let extra_rows = (result.rows - 1).max(0) as usize;
                if proto == ImageProtocol::Kitty {
                    let mut out = Vec::with_capacity(extra_rows + 1);
                    out.push(result.sequence);
                    for _ in 0..extra_rows {
                        out.push(String::new());
                    }
                    out
                } else {
                    let mut out = Vec::with_capacity(extra_rows + 1);
                    for _ in 0..extra_rows {
                        out.push(String::new());
                    }
                    let move_up = if extra_rows > 0 {
                        format!("\x1b[{extra_rows}A")
                    } else {
                        String::new()
                    };
                    out.push(format!("{move_up}{}", result.sequence));
                    out
                }
            } else {
                let fallback = image_fallback(&mime_type, Some(orig_dimensions), options.filename.as_deref());
                vec![truncate_to_width(&fallback_color(&fallback), width as i64, None, None)]
            }
        } else {
            let fallback = image_fallback(&mime_type, Some(orig_dimensions), options.filename.as_deref());
            vec![truncate_to_width(&fallback_color(&fallback), width as i64, None, None)]
        };

        {
            let mut state = self.inner.state.lock().unwrap();
            state.image_id = image_id;
            if png_data.is_some() {
                state.png_data = png_data;
            }
            state.cached_lines = Some(lines.clone());
            state.cached_width = Some(width);
        }

        lines
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

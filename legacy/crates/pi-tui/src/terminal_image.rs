use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use base64::Engine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    ITerm2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub images: Option<ImageProtocol>,
    pub true_color: bool,
    pub hyperlinks: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRenderOptions {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub preserve_aspect_ratio: bool,
    pub image_id: Option<u32>,
    pub move_cursor: bool,
}

impl Default for ImageRenderOptions {
    fn default() -> Self {
        Self {
            max_width_cells: None,
            max_height_cells: None,
            preserve_aspect_ratio: true,
            image_id: None,
            move_cursor: true,
        }
    }
}

static CELL_DIMENSIONS: OnceLock<Mutex<CellDimensions>> = OnceLock::new();
static CAPABILITIES: OnceLock<Mutex<Option<TerminalCapabilities>>> = OnceLock::new();
static IMAGE_ID: AtomicU32 = AtomicU32::new(1);

pub fn get_cell_dimensions() -> CellDimensions {
    *CELL_DIMENSIONS
        .get_or_init(|| {
            Mutex::new(CellDimensions {
                width_px: 9,
                height_px: 18,
            })
        })
        .lock()
        .expect("cell dimensions mutex poisoned")
}

pub fn set_cell_dimensions(dims: CellDimensions) {
    *CELL_DIMENSIONS
        .get_or_init(|| {
            Mutex::new(CellDimensions {
                width_px: 9,
                height_px: 18,
            })
        })
        .lock()
        .expect("cell dimensions mutex poisoned") = dims;
}

pub fn detect_capabilities() -> TerminalCapabilities {
    let term_program = std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_lowercase();
    let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
    let color_term = std::env::var("COLORTERM")
        .unwrap_or_default()
        .to_lowercase();
    let true_color = color_term == "truecolor" || color_term == "24bit";

    if std::env::var("TMUX").is_ok() || term.starts_with("tmux") || term.starts_with("screen") {
        return TerminalCapabilities {
            images: None,
            true_color,
            hyperlinks: false,
        };
    }
    if std::env::var("KITTY_WINDOW_ID").is_ok()
        || term_program == "kitty"
        || term_program == "ghostty"
        || term.contains("ghostty")
        || std::env::var("GHOSTTY_RESOURCES_DIR").is_ok()
        || std::env::var("WEZTERM_PANE").is_ok()
        || term_program == "wezterm"
    {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }
    if std::env::var("ITERM_SESSION_ID").is_ok() || term_program == "iterm.app" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::ITerm2),
            true_color: true,
            hyperlinks: true,
        };
    }
    if matches!(term_program.as_str(), "vscode" | "alacritty") {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }
    TerminalCapabilities {
        images: None,
        true_color,
        hyperlinks: false,
    }
}

pub fn get_capabilities() -> TerminalCapabilities {
    let mut guard = CAPABILITIES
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("capabilities mutex poisoned");
    if let Some(caps) = *guard {
        caps
    } else {
        let caps = detect_capabilities();
        *guard = Some(caps);
        caps
    }
}

pub fn set_capabilities(caps: TerminalCapabilities) {
    *CAPABILITIES
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("capabilities mutex poisoned") = Some(caps);
}

pub fn reset_capabilities_cache() {
    *CAPABILITIES
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("capabilities mutex poisoned") = None;
}

pub fn is_image_line(line: &str) -> bool {
    line.contains("\u{1b}_G") || line.contains("\u{1b}]1337;File=")
}

pub fn allocate_image_id() -> u32 {
    let id = IMAGE_ID.fetch_add(1, Ordering::Relaxed);
    if id == 0 { 1 } else { id }
}

pub fn encode_kitty(
    base64_data: &str,
    columns: Option<u32>,
    rows: Option<u32>,
    image_id: Option<u32>,
    move_cursor: bool,
) -> String {
    const CHUNK_SIZE: usize = 4096;
    let mut params = vec!["a=T".to_string(), "f=100".to_string(), "q=2".to_string()];
    if !move_cursor {
        params.push("C=1".to_string());
    }
    if let Some(columns) = columns {
        params.push(format!("c={columns}"));
    }
    if let Some(rows) = rows {
        params.push(format!("r={rows}"));
    }
    if let Some(image_id) = image_id {
        params.push(format!("i={image_id}"));
    }

    if base64_data.len() <= CHUNK_SIZE {
        return format!("\u{1b}_G{};{}\u{1b}\\", params.join(","), base64_data);
    }

    let mut chunks = Vec::new();
    let mut offset = 0;
    let mut first = true;
    while offset < base64_data.len() {
        let end = (offset + CHUNK_SIZE).min(base64_data.len());
        let chunk = &base64_data[offset..end];
        let last = end >= base64_data.len();
        if first {
            chunks.push(format!(
                "\u{1b}_G{},m=1;{}\u{1b}\\",
                params.join(","),
                chunk
            ));
            first = false;
        } else if last {
            chunks.push(format!("\u{1b}_Gm=0;{chunk}\u{1b}\\"));
        } else {
            chunks.push(format!("\u{1b}_Gm=1;{chunk}\u{1b}\\"));
        }
        offset = end;
    }
    chunks.join("")
}

pub fn delete_kitty_image(image_id: u32) -> String {
    format!("\u{1b}_Ga=d,d=I,i={image_id},q=2\u{1b}\\")
}

pub fn delete_all_kitty_images() -> String {
    "\u{1b}_Ga=d,d=A,q=2\u{1b}\\".to_string()
}

pub fn encode_iterm2(
    base64_data: &str,
    width: Option<&str>,
    height: Option<&str>,
    name_base64: Option<&str>,
    preserve_aspect_ratio: bool,
    inline: bool,
) -> String {
    let mut params = vec![format!("inline={}", if inline { 1 } else { 0 })];
    if let Some(width) = width {
        params.push(format!("width={width}"));
    }
    if let Some(height) = height {
        params.push(format!("height={height}"));
    }
    if let Some(name) = name_base64 {
        params.push(format!("name={name}"));
    }
    if !preserve_aspect_ratio {
        params.push("preserveAspectRatio=0".to_string());
    }
    format!("\u{1b}]1337;File={}:{}\u{7}", params.join(";"), base64_data)
}

pub fn calculate_image_cell_size(
    dimensions: ImageDimensions,
    max_width_cells: u32,
    max_height_cells: Option<u32>,
    cell: CellDimensions,
) -> (u32, u32) {
    let max_width = max_width_cells.max(1);
    let max_height = max_height_cells.map(|height| height.max(1));
    let image_width = dimensions.width_px.max(1);
    let image_height = dimensions.height_px.max(1);
    let width_scale = (max_width * cell.width_px.max(1)) as f64 / image_width as f64;
    let height_scale = max_height.map_or(width_scale, |height| {
        (height * cell.height_px.max(1)) as f64 / image_height as f64
    });
    let scale = width_scale.min(height_scale);
    let scaled_width_px = image_width as f64 * scale;
    let scaled_height_px = image_height as f64 * scale;
    let columns = (scaled_width_px / cell.width_px.max(1) as f64).ceil() as u32;
    let rows = (scaled_height_px / cell.height_px.max(1) as f64).ceil() as u32;
    (
        columns.clamp(1, max_width),
        max_height.map_or(rows.max(1), |height| rows.clamp(1, height)),
    )
}

pub fn calculate_image_rows(dimensions: ImageDimensions, target_width_cells: u32) -> u32 {
    calculate_image_cell_size(dimensions, target_width_cells, None, get_cell_dimensions()).1
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedImage {
    pub sequence: String,
    pub rows: u32,
    pub image_id: Option<u32>,
}

pub fn render_image(
    base64_data: &str,
    image_dimensions: ImageDimensions,
    options: ImageRenderOptions,
) -> Option<RenderedImage> {
    let caps = get_capabilities();
    let protocol = caps.images?;
    let max_width = options.max_width_cells.unwrap_or(80);
    let (columns, rows) = calculate_image_cell_size(
        image_dimensions,
        max_width,
        options.max_height_cells,
        get_cell_dimensions(),
    );
    match protocol {
        ImageProtocol::Kitty => Some(RenderedImage {
            sequence: encode_kitty(
                base64_data,
                Some(columns),
                Some(rows),
                options.image_id,
                options.move_cursor,
            ),
            rows,
            image_id: options.image_id,
        }),
        ImageProtocol::ITerm2 => Some(RenderedImage {
            sequence: encode_iterm2(
                base64_data,
                Some(&columns.to_string()),
                Some("auto"),
                None,
                options.preserve_aspect_ratio,
                true,
            ),
            rows,
            image_id: None,
        }),
    }
}

pub fn hyperlink(text: &str, url: &str) -> String {
    format!("\u{1b}]8;;{url}\u{1b}\\{text}\u{1b}]8;;\u{1b}\\")
}

pub fn image_fallback(
    mime_type: &str,
    dimensions: Option<ImageDimensions>,
    filename: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(filename) = filename {
        parts.push(filename.to_string());
    }
    parts.push(format!("[{mime_type}]"));
    if let Some(dim) = dimensions {
        parts.push(format!("{}x{}", dim.width_px, dim.height_px));
    }
    format!("[Image: {}]", parts.join(" "))
}

fn decode_base64(base64_data: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .ok()
}

pub fn get_png_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 24 || &buffer[0..4] != b"\x89PNG" {
        return None;
    }
    Some(ImageDimensions {
        width_px: u32::from_be_bytes(buffer[16..20].try_into().ok()?),
        height_px: u32::from_be_bytes(buffer[20..24].try_into().ok()?),
    })
}

pub fn get_jpeg_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 2 || buffer[0] != 0xff || buffer[1] != 0xd8 {
        return None;
    }
    let mut offset = 2;
    while offset + 9 < buffer.len() {
        if buffer[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = buffer[offset + 1];
        if (0xc0..=0xc2).contains(&marker) {
            return Some(ImageDimensions {
                width_px: u16::from_be_bytes([buffer[offset + 7], buffer[offset + 8]]) as u32,
                height_px: u16::from_be_bytes([buffer[offset + 5], buffer[offset + 6]]) as u32,
            });
        }
        if offset + 3 >= buffer.len() {
            return None;
        }
        let length = u16::from_be_bytes([buffer[offset + 2], buffer[offset + 3]]) as usize;
        if length < 2 {
            return None;
        }
        offset += 2 + length;
    }
    None
}

pub fn get_gif_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 10 || (&buffer[0..6] != b"GIF87a" && &buffer[0..6] != b"GIF89a") {
        return None;
    }
    Some(ImageDimensions {
        width_px: u16::from_le_bytes([buffer[6], buffer[7]]) as u32,
        height_px: u16::from_le_bytes([buffer[8], buffer[9]]) as u32,
    })
}

pub fn get_webp_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 30 || &buffer[0..4] != b"RIFF" || &buffer[8..12] != b"WEBP" {
        return None;
    }
    match &buffer[12..16] {
        b"VP8 " => Some(ImageDimensions {
            width_px: (u16::from_le_bytes([buffer[26], buffer[27]]) & 0x3fff) as u32,
            height_px: (u16::from_le_bytes([buffer[28], buffer[29]]) & 0x3fff) as u32,
        }),
        b"VP8L" => {
            if buffer.len() < 25 {
                return None;
            }
            let bits = u32::from_le_bytes([buffer[21], buffer[22], buffer[23], buffer[24]]);
            Some(ImageDimensions {
                width_px: (bits & 0x3fff) + 1,
                height_px: ((bits >> 14) & 0x3fff) + 1,
            })
        }
        b"VP8X" => Some(ImageDimensions {
            width_px: (buffer[24] as u32
                | ((buffer[25] as u32) << 8)
                | ((buffer[26] as u32) << 16))
                + 1,
            height_px: (buffer[27] as u32
                | ((buffer[28] as u32) << 8)
                | ((buffer[29] as u32) << 16))
                + 1,
        }),
        _ => None,
    }
}

pub fn get_image_dimensions(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
    match mime_type {
        "image/png" => get_png_dimensions(base64_data),
        "image/jpeg" => get_jpeg_dimensions(base64_data),
        "image/gif" => get_gif_dimensions(base64_data),
        "image/webp" => get_webp_dimensions(base64_data),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_kitty_delete() {
        assert_eq!(delete_kitty_image(7), "\u{1b}_Ga=d,d=I,i=7,q=2\u{1b}\\");
    }

    #[test]
    fn hyperlink_uses_osc8_st_sequence() {
        assert_eq!(
            hyperlink("text", "https://example.com"),
            "\u{1b}]8;;https://example.com\u{1b}\\text\u{1b}]8;;\u{1b}\\"
        );
    }
}

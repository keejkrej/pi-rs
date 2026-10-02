//! Port of packages/tui/src/terminal-image.ts

#![allow(dead_code, unused_variables)]

use std::sync::{LazyLock, Mutex};

use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};

use crate::colors::TerminalColorMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageProtocol {
    #[serde(rename = "kitty")]
    Kitty,
    #[serde(rename = "iterm2")]
    Iterm2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCapabilities {
    /// PORT: TS `"kitty" | "iterm2" | null`. `None` is `null` (the key is always present in TS literals).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<ImageProtocol>,
    pub true_color: bool,
    pub hyperlinks: bool,
}

/// PORT: TS `Partial<TerminalCapabilities>`. `images: None` is absent, `Some(None)` is `null`,
/// `Some(Some(_))` is `"kitty"` | `"iterm2"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCapabilityOverrides {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "pi_js::json::double_option"
    )]
    pub images: Option<Option<ImageProtocol>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub true_color: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyperlinks: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellDimensions {
    pub width_px: i64,
    pub height_px: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageDimensions {
    pub width_px: i64,
    pub height_px: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRenderOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width_cells: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height_cells: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_aspect_ratio: Option<bool>,
    /// Kitty image ID. If provided, reuses/replaces existing image with this ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<i64>,
    /// Whether Kitty should apply its default cursor movement after placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_cursor: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageCellSize {
    pub columns: i64,
    pub rows: i64,
}

/// Field order is the object literal (`imageId`, `columns`, `rows`, `widthPx`, `heightPx`), not the interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KittyImageMetadata {
    pub image_id: i64,
    pub columns: i64,
    pub rows: i64,
    pub width_px: i64,
    pub height_px: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RegisteredKittyImageMetadata {
    image_id: i64,
    columns: i64,
    rows: i64,
    width_px: i64,
    height_px: i64,
    transmission_generation: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KittyImagePlacement {
    pub image_id: i64,
    pub transmission_generation: i64,
    pub transmission_bytes: i64,
    pub estimated_decoded_bytes: i64,
    pub sequence: String,
    pub replacement_line: String,
}

/// TS inline options of `encodeKitty`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeKittyOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<i64>,
    /// Whether Kitty should apply its default cursor movement after placement. Default: true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_cursor: Option<bool>,
}

/// TS `number | string` (`width` / `height` of [`EncodeITerm2Options`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NumberOrString {
    Number(f64),
    Text(String),
}

/// TS inline options of `encodeITerm2`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeITerm2Options {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<NumberOrString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<NumberOrString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_aspect_ratio: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<bool>,
}

/// `{ sequence, columns, rows, imageId? }` from [`render_image`], or `None` when images are unsupported.
///
/// PORT: TS returns this anonymous object, or `null`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderImageResult {
    pub sequence: String,
    pub columns: i64,
    pub rows: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<i64>,
}

const DEFAULT_CELL_DIMENSIONS: CellDimensions = CellDimensions {
    width_px: 9,
    height_px: 18,
};

static CACHED_CAPABILITIES: LazyLock<Mutex<Option<TerminalCapabilities>>> = LazyLock::new(|| Mutex::new(None));
static CAPABILITY_OVERRIDES: LazyLock<Mutex<TerminalCapabilityOverrides>> =
    LazyLock::new(|| Mutex::new(TerminalCapabilityOverrides::default()));
// Default cell dimensions - updated by TUI when terminal responds to query
static CELL_DIMENSIONS: LazyLock<Mutex<CellDimensions>> = LazyLock::new(|| Mutex::new(DEFAULT_CELL_DIMENSIONS));

const KITTY_PREFIX: &str = "\u{1b}_G";
const ITERM2_PREFIX: &str = "\u{1b}]1337;File=";
const KITTY_CHUNK_SIZE: usize = 4096;

static KITTY_PLACEMENT_CONTROL_KEYS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        "i", "p", "x", "y", "w", "h", "X", "Y", "c", "r", "C", "U", "z", "P", "Q", "H", "V",
    ])
});

static KITTY_IMAGE_METADATA: LazyLock<Mutex<IndexMap<i64, RegisteredKittyImageMetadata>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));
static KITTY_TRANSMISSION_GENERATION: LazyLock<Mutex<i64>> = LazyLock::new(|| Mutex::new(0));

pub fn get_cell_dimensions() -> CellDimensions {
    *CELL_DIMENSIONS.lock().unwrap()
}

pub fn set_cell_dimensions(dims: CellDimensions) {
    *CELL_DIMENSIONS.lock().unwrap() = dims;
}

/// Checks whether the attached tmux client forwards OSC 8 hyperlinks to the
/// outer terminal. tmux only re-emits them when its `client_termfeatures` lists
/// `hyperlinks`, and strips them otherwise. On any error fallbacks `false`.
fn probe_tmux_hyperlinks() -> bool {
    todo!("port: probe_tmux_hyperlinks")
}

fn detect_capabilities_from_environment(tmux_forwards_hyperlink: &dyn Fn() -> bool) -> TerminalCapabilities {
    todo!("port: detect_capabilities_from_environment")
}

fn parse_boolean_capability_override(value: Option<&str>) -> Option<bool> {
    todo!("port: parse_boolean_capability_override")
}

/// PORT: `None` selects `probe_tmux_hyperlinks`, the TS default.
pub fn detect_capabilities(tmux_forwards_hyperlink: Option<&dyn Fn() -> bool>) -> TerminalCapabilities {
    todo!("port: detect_capabilities")
}

pub fn get_capabilities() -> TerminalCapabilities {
    todo!("port: get_capabilities")
}

/// PORT: `None` selects [`get_capabilities`], the TS default.
pub fn get_terminal_color_mode(capabilities: Option<TerminalCapabilities>) -> TerminalColorMode {
    todo!("port: get_terminal_color_mode")
}

pub fn reset_capabilities_cache() {
    *CACHED_CAPABILITIES.lock().unwrap() = None;
}

/// Override selected auto-detected capabilities.
pub fn set_capability_overrides(overrides: TerminalCapabilityOverrides) {
    todo!("port: set_capability_overrides")
}

/// Override the cached capabilities. Useful in tests to exercise both code paths.
pub fn set_capabilities(caps: TerminalCapabilities) {
    *CACHED_CAPABILITIES.lock().unwrap() = Some(caps);
}

pub fn is_image_line(line: &str) -> bool {
    todo!("port: is_image_line")
}

/// Generate a random image ID for Kitty graphics protocol.
/// Uses random IDs to avoid collisions between different module instances
/// (e.g., main app vs extensions).
pub fn allocate_image_id() -> i64 {
    todo!("port: allocate_image_id")
}

pub fn encode_kitty(base64_data: &str, options: Option<EncodeKittyOptions>) -> String {
    todo!("port: encode_kitty")
}

/// Delete a Kitty graphics image by ID.
/// Uses uppercase 'I' to also free the image data.
pub fn delete_kitty_image(image_id: i64) -> String {
    todo!("port: delete_kitty_image")
}

/// Delete all visible Kitty graphics images.
/// Uses uppercase 'A' to also free the image data.
pub fn delete_all_kitty_images() -> String {
    "\u{1b}_Ga=d,d=A,q=2\u{1b}\\".to_string()
}

/// Delete all visible Kitty placements while retaining their uploaded image data.
pub fn delete_all_kitty_placements() -> String {
    "\u{1b}_Ga=d,d=a,q=2\u{1b}\\".to_string()
}

pub fn encode_iterm2(base64_data: &str, options: Option<EncodeITerm2Options>) -> String {
    todo!("port: encode_iterm2")
}

pub fn register_kitty_image_metadata(metadata: KittyImageMetadata) {
    todo!("port: register_kitty_image_metadata")
}

fn get_registered_kitty_image_metadata(line: &str) -> Option<RegisteredKittyImageMetadata> {
    todo!("port: get_registered_kitty_image_metadata")
}

pub fn get_kitty_image_metadata(line: &str) -> Option<KittyImageMetadata> {
    todo!("port: get_kitty_image_metadata")
}

/// Build a placement-only command for an image line emitted by [`render_image`].
pub fn get_kitty_image_placement(line: &str) -> Option<KittyImagePlacement> {
    todo!("port: get_kitty_image_placement")
}

pub fn crop_kitty_image_line(line: &str, hidden_rows: i64, visible_rows: i64) -> String {
    todo!("port: crop_kitty_image_line")
}

fn choose_less_distorted_cell_count(upper_count: i64, ideal_count: f64) -> i64 {
    todo!("port: choose_less_distorted_cell_count")
}

/// PORT: `None` cell dimensions are `{ widthPx: 9, heightPx: 18 }`. `None` `optimize_aspect_ratio` is false.
pub fn calculate_image_cell_size(
    image_dimensions: ImageDimensions,
    max_width_cells: i64,
    max_height_cells: Option<i64>,
    cell_dimensions: Option<CellDimensions>,
    optimize_aspect_ratio: Option<bool>,
) -> ImageCellSize {
    todo!("port: calculate_image_cell_size")
}

pub fn calculate_image_rows(
    image_dimensions: ImageDimensions,
    target_width_cells: i64,
    cell_dimensions: Option<CellDimensions>,
) -> i64 {
    calculate_image_cell_size(image_dimensions, target_width_cells, None, cell_dimensions, None).rows
}

pub fn get_png_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    todo!("port: get_png_dimensions")
}

pub fn get_jpeg_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    todo!("port: get_jpeg_dimensions")
}

pub fn get_gif_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    todo!("port: get_gif_dimensions")
}

pub fn get_webp_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    todo!("port: get_webp_dimensions")
}

pub fn get_image_dimensions(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
    todo!("port: get_image_dimensions")
}

pub fn render_image(
    base64_data: &str,
    image_dimensions: ImageDimensions,
    options: Option<ImageRenderOptions>,
) -> Option<RenderImageResult> {
    todo!("port: render_image")
}

/// Wrap text in an OSC 8 hyperlink sequence.
/// The text is rendered as a clickable hyperlink in terminals that support OSC 8
/// (Ghostty, Kitty, WezTerm, iTerm2, VSCode, and others).
/// In terminals that do not support OSC 8, the escape sequences are ignored
/// and only the plain text is displayed.
///
/// @param text - The visible text to display
/// @param url - The URL to link to
pub fn hyperlink(text: &str, url: &str) -> String {
    todo!("port: hyperlink")
}

/// Shorten home-prefixed absolute paths to ~/... for compact display.
fn shorten_image_path(filename: &str) -> String {
    todo!("port: shorten_image_path")
}

/// Text fallback when the terminal cannot render inline images.
/// Absolute paths are shown shortened (~/...) and, when OSC 8 hyperlinks are
/// available, linked to file:// so the full path remains openable.
pub fn image_fallback(mime_type: &str, dimensions: Option<ImageDimensions>, filename: Option<&str>) -> String {
    todo!("port: image_fallback")
}

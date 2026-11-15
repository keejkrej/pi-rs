//! Port of packages/tui/src/terminal-image.ts

#![allow(dead_code)]

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex, mpsc};
use std::time::Duration;

use indexmap::{IndexMap, IndexSet};
use regex::Regex;
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

static KITTY_APC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b_G([^;]*);").expect("KITTY_APC_RE compiles"));
static KITTY_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|,)i=([0-9]+)(?:,|$)").expect("KITTY_ID_RE compiles"));
static KITTY_MORE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|,)m=1(?:,|$)").expect("KITTY_MORE_RE compiles"));
static KITTY_CROP_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[yhr]=").expect("KITTY_CROP_KEY_RE compiles"));

fn env_non_empty(key: &str) -> bool {
    pi_js::env::var(key).is_some_and(|v| !v.is_empty())
}

/// Checks whether the attached tmux client forwards OSC 8 hyperlinks to the
/// outer terminal. tmux only re-emits them when its `client_termfeatures` lists
/// `hyperlinks`, and strips them otherwise. On any error fallbacks `false`.
fn probe_tmux_hyperlinks() -> bool {
    let Ok(mut child) = Command::new("tmux")
        .args(["display-message", "-p", "#{client_termfeatures}"])
        .env_clear()
        .envs(pi_js::env::vars())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };

    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(1);
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let Ok(buf) = rx.recv_timeout(Duration::from_millis(250)) else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };

    let Ok(status) = child.wait() else {
        return false;
    };
    if !status.success() {
        return false;
    }

    let termfeatures = String::from_utf8_lossy(&buf);
    termfeatures.split(',').any(|feature| feature.trim() == "hyperlinks")
}

fn detect_capabilities_from_environment(tmux_forwards_hyperlink: &dyn Fn() -> bool) -> TerminalCapabilities {
    let term_program = pi_js::env::var("TERM_PROGRAM").unwrap_or_default().to_lowercase();
    let terminal_emulator = pi_js::env::var("TERMINAL_EMULATOR").unwrap_or_default().to_lowercase();
    let term = pi_js::env::var("TERM").unwrap_or_default().to_lowercase();
    let color_term = pi_js::env::var("COLORTERM").unwrap_or_default().to_lowercase();
    let has_true_color_hint = color_term == "truecolor" || color_term == "24bit" || term.ends_with("-direct");
    let is_windows_console = pi_js::env::platform() == "win32";

    // Emit OSC 8 hyperlinks only when tmux confirms it forwards.
    // Image protocols are unreliable under tmux, so leave `images: null`.
    if env_non_empty("TMUX") || term.starts_with("tmux") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: tmux_forwards_hyperlink(),
        };
    }

    // screen does not forward OSC 8 hyperlinks, so keep them off there.
    if term.starts_with("screen") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: false,
        };
    }

    if env_non_empty("KITTY_WINDOW_ID") || term_program == "kitty" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "ghostty" || term.contains("ghostty") || env_non_empty("GHOSTTY_RESOURCES_DIR") {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if env_non_empty("WEZTERM_PANE") || term_program == "wezterm" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    // Warp supports the Kitty graphics protocol and OSC 8 hyperlinks.
    if term_program == "warpterminal" || env_non_empty("WARP_SESSION_ID") || env_non_empty("WARP_TERMINAL_SESSION_UUID")
    {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if env_non_empty("ITERM_SESSION_ID") || term_program == "iterm.app" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Iterm2),
            true_color: true,
            hyperlinks: true,
        };
    }

    if env_non_empty("WT_SESSION") {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "alacritty" || term_program == "vscode" || term_program == "zed" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    if terminal_emulator == "jetbrains-jediterm" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: false,
        };
    }

    // Windows Terminal does not always set WT_SESSION, for example when it hosts
    // a cmd.exe launched directly from Win+R. Modern Windows consoles support
    // truecolor; keep hyperlinks off unless we positively detected support above.
    if is_windows_console {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: false,
        };
    }

    // Unknown terminal: be conservative. OSC 8 is rendered invisibly as "just
    // text" on terminals that swallow it, which means the URL disappears from
    // the rendered output. Default to the legacy `text (url)` behavior unless we
    // have positively identified a hyperlink-capable terminal above.
    TerminalCapabilities {
        images: None,
        true_color: has_true_color_hint,
        hyperlinks: false,
    }
}

fn parse_boolean_capability_override(value: Option<&str>) -> Option<bool> {
    match value {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    }
}

/// PORT: `None` selects `probe_tmux_hyperlinks`, the TS default.
pub fn detect_capabilities(tmux_forwards_hyperlink: Option<&dyn Fn() -> bool>) -> TerminalCapabilities {
    let hyperlinks = parse_boolean_capability_override(pi_js::env::var("PI_HYPERLINKS").as_deref());
    let detected = match hyperlinks {
        Some(h) => detect_capabilities_from_environment(&|| h),
        None => detect_capabilities_from_environment(tmux_forwards_hyperlink.unwrap_or(&probe_tmux_hyperlinks)),
    };
    let image_protocol = pi_js::env::var("PI_IMAGE_PROTOCOL").map(|v| v.to_lowercase());
    let images: Option<Option<ImageProtocol>> = match image_protocol.as_deref() {
        Some("kitty") => Some(Some(ImageProtocol::Kitty)),
        Some("iterm2") => Some(Some(ImageProtocol::Iterm2)),
        Some("none") | Some("0") => Some(None),
        _ => None,
    };
    let true_color = parse_boolean_capability_override(pi_js::env::var("PI_TRUE_COLOR").as_deref());
    TerminalCapabilities {
        images: images.unwrap_or(detected.images),
        true_color: true_color.unwrap_or(detected.true_color),
        hyperlinks: hyperlinks.unwrap_or(detected.hyperlinks),
    }
}

pub fn get_capabilities() -> TerminalCapabilities {
    if let Some(caps) = *CACHED_CAPABILITIES.lock().unwrap() {
        return caps;
    }
    let overrides = *CAPABILITY_OVERRIDES.lock().unwrap();
    let detected = match overrides.hyperlinks {
        Some(h) => detect_capabilities(Some(&|| h)),
        None => detect_capabilities(None),
    };
    let caps = TerminalCapabilities {
        images: overrides.images.unwrap_or(detected.images),
        true_color: overrides.true_color.unwrap_or(detected.true_color),
        hyperlinks: overrides.hyperlinks.unwrap_or(detected.hyperlinks),
    };
    let mut cached = CACHED_CAPABILITIES.lock().unwrap();
    *cached.get_or_insert(caps)
}

/// PORT: `None` selects [`get_capabilities`], the TS default.
pub fn get_terminal_color_mode(capabilities: Option<TerminalCapabilities>) -> TerminalColorMode {
    let caps = capabilities.unwrap_or_else(get_capabilities);
    if caps.true_color {
        TerminalColorMode::Truecolor
    } else {
        TerminalColorMode::N256color
    }
}

pub fn reset_capabilities_cache() {
    *CACHED_CAPABILITIES.lock().unwrap() = None;
}

/// Override selected auto-detected capabilities.
pub fn set_capability_overrides(overrides: TerminalCapabilityOverrides) {
    let mut current = CAPABILITY_OVERRIDES.lock().unwrap();
    if *current == overrides {
        return;
    }
    *current = overrides;
    *CACHED_CAPABILITIES.lock().unwrap() = None;
}

/// Override the cached capabilities. Useful in tests to exercise both code paths.
pub fn set_capabilities(caps: TerminalCapabilities) {
    *CACHED_CAPABILITIES.lock().unwrap() = Some(caps);
}

pub fn is_image_line(line: &str) -> bool {
    // Fast path: sequence at line start (single-row images)
    if line.starts_with(KITTY_PREFIX) || line.starts_with(ITERM2_PREFIX) {
        return true;
    }
    // Slow path: sequence elsewhere (multi-row images have cursor-up prefix)
    line.contains(KITTY_PREFIX) || line.contains(ITERM2_PREFIX)
}

/// Generate a random image ID for Kitty graphics protocol.
/// Uses random IDs to avoid collisions between different module instances
/// (e.g., main app vs extensions).
pub fn allocate_image_id() -> i64 {
    // Use random ID in range [1, 0xffffffff] to avoid collisions
    (pi_js::crypto::math_random() * f64::from(0xffff_fffe_u32)).floor() as i64 + 1
}

pub fn encode_kitty(base64_data: &str, options: Option<EncodeKittyOptions>) -> String {
    let options = options.unwrap_or_default();
    let mut params: Vec<String> = vec!["a=T".to_string(), "f=100".to_string(), "q=2".to_string()];

    if options.move_cursor == Some(false) {
        params.push("C=1".to_string());
    }
    if let Some(columns) = options.columns.filter(|&c| c != 0) {
        params.push(format!("c={columns}"));
    }
    if let Some(rows) = options.rows.filter(|&r| r != 0) {
        params.push(format!("r={rows}"));
    }
    if let Some(image_id) = options.image_id.filter(|&id| id != 0) {
        params.push(format!("i={image_id}"));
    }

    let len = if base64_data.is_ascii() {
        base64_data.len()
    } else {
        pi_js::str16::len(base64_data)
    };

    if len <= KITTY_CHUNK_SIZE {
        return format!("\u{1b}_G{};{}\u{1b}\\", params.join(","), base64_data);
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut offset = 0usize;
    let mut is_first = true;
    let is_ascii = base64_data.is_ascii();

    while offset < len {
        let end = (offset + KITTY_CHUNK_SIZE).min(len);
        let chunk = if is_ascii {
            base64_data[offset..end].to_string()
        } else {
            pi_js::str16::slice(base64_data, offset as i64, Some(end as i64))
        };
        let is_last = offset + KITTY_CHUNK_SIZE >= len;

        if is_first {
            chunks.push(format!("\u{1b}_G{},m=1;{}\u{1b}\\", params.join(","), chunk));
            is_first = false;
        } else if is_last {
            chunks.push(format!("\u{1b}_Gm=0;{}\u{1b}\\", chunk));
        } else {
            chunks.push(format!("\u{1b}_Gm=1;{}\u{1b}\\", chunk));
        }

        offset += KITTY_CHUNK_SIZE;
    }

    chunks.concat()
}

/// Delete a Kitty graphics image by ID.
/// Uses uppercase 'I' to also free the image data.
pub fn delete_kitty_image(image_id: i64) -> String {
    format!("\u{1b}_Ga=d,d=I,i={image_id},q=2\u{1b}\\")
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
    let options = options.unwrap_or_default();
    let mut params: Vec<String> = vec![
        format!("inline={}", if options.inline != Some(false) { 1 } else { 0 }),
        format!("size={}", pi_js::b64::decode(base64_data).len()),
    ];

    if let Some(width) = &options.width {
        match width {
            NumberOrString::Number(n) => params.push(format!("width={}", pi_js::num::to_js_string(*n))),
            NumberOrString::Text(s) => params.push(format!("width={s}")),
        }
    }
    if let Some(height) = &options.height {
        match height {
            NumberOrString::Number(n) => params.push(format!("height={}", pi_js::num::to_js_string(*n))),
            NumberOrString::Text(s) => params.push(format!("height={s}")),
        }
    }
    if let Some(name) = options.name.as_deref().filter(|s| !s.is_empty()) {
        let name_base64 = pi_js::b64::encode(name.as_bytes());
        params.push(format!("name={name_base64}"));
    }
    if options.preserve_aspect_ratio == Some(false) {
        params.push("preserveAspectRatio=0".to_string());
    }

    format!("\u{1b}]1337;File={}:{}\u{7}", params.join(";"), base64_data)
}

pub fn register_kitty_image_metadata(metadata: KittyImageMetadata) {
    let mut generation = KITTY_TRANSMISSION_GENERATION.lock().unwrap();
    *generation += 1;
    let transmission_generation = *generation;
    drop(generation);

    let mut map = KITTY_IMAGE_METADATA.lock().unwrap();
    map.shift_remove(&metadata.image_id);
    map.insert(
        metadata.image_id,
        RegisteredKittyImageMetadata {
            image_id: metadata.image_id,
            columns: metadata.columns,
            rows: metadata.rows,
            width_px: metadata.width_px,
            height_px: metadata.height_px,
            transmission_generation,
        },
    );
    if map.len() > 1000 {
        map.shift_remove_index(0);
    }
}

fn get_registered_kitty_image_metadata(line: &str) -> Option<RegisteredKittyImageMetadata> {
    let controls = KITTY_APC_RE.captures(line)?.get(1)?.as_str();
    if controls.is_empty() {
        return None;
    }
    let image_id_str = KITTY_ID_RE.captures(controls)?.get(1)?.as_str();
    let image_id = pi_js::num::parse_int(image_id_str, Some(10)) as i64;
    KITTY_IMAGE_METADATA.lock().unwrap().get(&image_id).copied()
}

pub fn get_kitty_image_metadata(line: &str) -> Option<KittyImageMetadata> {
    let metadata = get_registered_kitty_image_metadata(line)?;
    Some(KittyImageMetadata {
        image_id: metadata.image_id,
        columns: metadata.columns,
        rows: metadata.rows,
        width_px: metadata.width_px,
        height_px: metadata.height_px,
    })
}

/// Build a placement-only command for an image line emitted by [`render_image`].
pub fn get_kitty_image_placement(line: &str) -> Option<KittyImagePlacement> {
    let caps = KITTY_APC_RE.captures(line)?;
    let full_match = caps.get(0)?;
    let match_index = full_match.start();
    let match_controls = caps.get(1)?.as_str();
    let metadata = get_registered_kitty_image_metadata(line)?;

    let mut command_start = match_index;
    let mut command_controls = match_controls;
    let mut transmission_end: usize;
    loop {
        let search_from = command_start + KITTY_PREFIX.len();
        let rel_term = line.get(search_from..)?.find("\u{1b}\\")?;
        let terminator = search_from + rel_term;
        transmission_end = terminator + 2;
        if !KITTY_MORE_RE.is_match(command_controls) {
            break;
        }
        command_start = transmission_end;
        if !line.get(command_start..)?.starts_with(KITTY_PREFIX) {
            return None;
        }
        let controls_start = command_start + KITTY_PREFIX.len();
        let rel_semi = line.get(controls_start..)?.find(';')?;
        let controls_end = controls_start + rel_semi;
        command_controls = &line[controls_start..controls_end];
    }

    let controls: Vec<&str> = match_controls
        .split(',')
        .filter(|control| {
            let key = control.split_once('=').map_or(*control, |(k, _)| k);
            KITTY_PLACEMENT_CONTROL_KEYS.contains(key)
        })
        .collect();
    let sequence = format!("\u{1b}_Ga=p,q=2,{}\u{1b}\\", controls.join(","));
    Some(KittyImagePlacement {
        image_id: metadata.image_id,
        transmission_generation: metadata.transmission_generation,
        transmission_bytes: (transmission_end - match_index) as i64,
        estimated_decoded_bytes: metadata.width_px * metadata.height_px * 4,
        sequence: sequence.clone(),
        replacement_line: format!("{}{}{}", &line[..match_index], sequence, &line[transmission_end..]),
    })
}

pub fn crop_kitty_image_line(line: &str, hidden_rows: i64, visible_rows: i64) -> String {
    let Some(metadata) = get_kitty_image_metadata(line) else {
        return line.to_string();
    };
    let Some(caps) = KITTY_APC_RE.captures(line) else {
        return line.to_string();
    };
    if hidden_rows < 0 || hidden_rows >= metadata.rows || visible_rows <= 0 {
        return line.to_string();
    }
    let cropped_rows = visible_rows.min(metadata.rows - hidden_rows);
    if hidden_rows == 0 && cropped_rows == metadata.rows {
        return line.to_string();
    }
    let full_match = caps.get(0).unwrap();
    let match_controls = caps.get(1).unwrap().as_str();
    let source_y = ((metadata.height_px as f64 * hidden_rows as f64) / metadata.rows as f64).floor() as i64;
    let source_end =
        ((metadata.height_px as f64 * (hidden_rows + cropped_rows) as f64) / metadata.rows as f64).ceil() as i64;
    let source_height = 1.max(metadata.height_px.min(source_end) - source_y);
    let mut controls: Vec<String> = match_controls
        .split(',')
        .filter(|control| !KITTY_CROP_KEY_RE.is_match(control))
        .map(str::to_string)
        .collect();
    controls.push(format!("y={source_y}"));
    controls.push(format!("h={source_height}"));
    controls.push(format!("r={cropped_rows}"));
    format!(
        "{}\u{1b}_G{};{}",
        &line[..full_match.start()],
        controls.join(","),
        &line[full_match.end()..]
    )
}

fn choose_less_distorted_cell_count(upper_count: i64, ideal_count: f64) -> i64 {
    if upper_count <= 1 {
        return upper_count;
    }

    let lower_count = upper_count - 1;
    let upper_f = upper_count as f64;
    let lower_f = lower_count as f64;
    let upper_distortion = (upper_f / ideal_count).max(ideal_count / upper_f);
    let lower_distortion = (lower_f / ideal_count).max(ideal_count / lower_f);
    if lower_distortion < upper_distortion {
        lower_count
    } else {
        upper_count
    }
}

/// PORT: `None` cell dimensions are `{ widthPx: 9, heightPx: 18 }`. `None` `optimize_aspect_ratio` is false.
pub fn calculate_image_cell_size(
    image_dimensions: ImageDimensions,
    max_width_cells: i64,
    max_height_cells: Option<i64>,
    cell_dimensions: Option<CellDimensions>,
    optimize_aspect_ratio: Option<bool>,
) -> ImageCellSize {
    let cell_dimensions = cell_dimensions.unwrap_or(DEFAULT_CELL_DIMENSIONS);
    let optimize_aspect_ratio = optimize_aspect_ratio.unwrap_or(false);

    let max_width = max_width_cells.max(1);
    let max_height = max_height_cells.map(|v| v.max(1));
    let image_width = image_dimensions.width_px.max(1) as f64;
    let image_height = image_dimensions.height_px.max(1) as f64;

    let width_scale = (max_width as f64 * cell_dimensions.width_px as f64) / image_width;
    let height_scale = match max_height {
        None => width_scale,
        Some(mh) => (mh as f64 * cell_dimensions.height_px as f64) / image_height,
    };
    let scale = width_scale.min(height_scale);

    let scaled_width_px = image_width * scale;
    let scaled_height_px = image_height * scale;
    let mut columns = 1.max(max_width.min((scaled_width_px / cell_dimensions.width_px as f64).ceil() as i64));
    let height_rows = scaled_height_px / cell_dimensions.height_px as f64;
    let mut rows = 1.max(height_rows.ceil() as i64);
    if let Some(mh) = max_height {
        rows = mh.min(rows);
    }

    if !optimize_aspect_ratio {
        return ImageCellSize { columns, rows };
    }

    if width_scale <= height_scale {
        let ideal_rows = (columns as f64 * cell_dimensions.width_px as f64 * image_height)
            / (image_width * cell_dimensions.height_px as f64);
        rows = choose_less_distorted_cell_count(rows, ideal_rows);
    } else {
        let ideal_columns = (rows as f64 * cell_dimensions.height_px as f64 * image_width)
            / (image_height * cell_dimensions.width_px as f64);
        columns = choose_less_distorted_cell_count(columns, ideal_columns);
    }

    ImageCellSize { columns, rows }
}

pub fn calculate_image_rows(
    image_dimensions: ImageDimensions,
    target_width_cells: i64,
    cell_dimensions: Option<CellDimensions>,
) -> i64 {
    calculate_image_cell_size(image_dimensions, target_width_cells, None, cell_dimensions, None).rows
}

pub fn get_png_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = pi_js::b64::decode(base64_data);
    if buffer.len() < 24 {
        return None;
    }
    if buffer[0] != 0x89 || buffer[1] != 0x50 || buffer[2] != 0x4e || buffer[3] != 0x47 {
        return None;
    }
    let width = u32::from_be_bytes([buffer[16], buffer[17], buffer[18], buffer[19]]);
    let height = u32::from_be_bytes([buffer[20], buffer[21], buffer[22], buffer[23]]);
    Some(ImageDimensions {
        width_px: i64::from(width),
        height_px: i64::from(height),
    })
}

pub fn get_jpeg_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = pi_js::b64::decode(base64_data);
    if buffer.len() < 10 {
        return None;
    }
    if buffer[0] != 0xff || buffer[1] != 0xd8 {
        return None;
    }
    let mut offset = 2usize;
    while offset < buffer.len() - 9 {
        if buffer[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = buffer[offset + 1];
        if (0xc0..=0xc2).contains(&marker) {
            let height = u16::from_be_bytes([buffer[offset + 5], buffer[offset + 6]]);
            let width = u16::from_be_bytes([buffer[offset + 7], buffer[offset + 8]]);
            return Some(ImageDimensions {
                width_px: i64::from(width),
                height_px: i64::from(height),
            });
        }
        if offset + 3 >= buffer.len() {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([buffer[offset + 2], buffer[offset + 3]]));
        if length < 2 {
            return None;
        }
        offset += 2 + length;
    }
    None
}

pub fn get_gif_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = pi_js::b64::decode(base64_data);
    if buffer.len() < 10 {
        return None;
    }
    if &buffer[0..6] != b"GIF87a" && &buffer[0..6] != b"GIF89a" {
        return None;
    }
    let width = u16::from_le_bytes([buffer[6], buffer[7]]);
    let height = u16::from_le_bytes([buffer[8], buffer[9]]);
    Some(ImageDimensions {
        width_px: i64::from(width),
        height_px: i64::from(height),
    })
}

pub fn get_webp_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = pi_js::b64::decode(base64_data);
    if buffer.len() < 30 {
        return None;
    }
    if &buffer[0..4] != b"RIFF" || &buffer[8..12] != b"WEBP" {
        return None;
    }
    let chunk = &buffer[12..16];
    if chunk == b"VP8 " {
        let width = u16::from_le_bytes([buffer[26], buffer[27]]) & 0x3fff;
        let height = u16::from_le_bytes([buffer[28], buffer[29]]) & 0x3fff;
        return Some(ImageDimensions {
            width_px: i64::from(width),
            height_px: i64::from(height),
        });
    } else if chunk == b"VP8L" {
        let bits = u32::from_le_bytes([buffer[21], buffer[22], buffer[23], buffer[24]]);
        let width = (bits & 0x3fff) + 1;
        let height = ((bits >> 14) & 0x3fff) + 1;
        return Some(ImageDimensions {
            width_px: i64::from(width),
            height_px: i64::from(height),
        });
    } else if chunk == b"VP8X" {
        let width = (i64::from(buffer[24]) | (i64::from(buffer[25]) << 8) | (i64::from(buffer[26]) << 16)) + 1;
        let height = (i64::from(buffer[27]) | (i64::from(buffer[28]) << 8) | (i64::from(buffer[29]) << 16)) + 1;
        return Some(ImageDimensions {
            width_px: width,
            height_px: height,
        });
    }
    None
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

pub fn render_image(
    base64_data: &str,
    image_dimensions: ImageDimensions,
    options: Option<ImageRenderOptions>,
) -> Option<RenderImageResult> {
    let caps = get_capabilities();
    let protocol = caps.images?;
    let options = options.unwrap_or_default();

    let max_width = options.max_width_cells.unwrap_or(80);
    // Reduce Kitty's cell-aligned distortion without shrinking iTerm2 reservations.
    let size = calculate_image_cell_size(
        image_dimensions,
        max_width,
        options.max_height_cells,
        Some(get_cell_dimensions()),
        Some(protocol == ImageProtocol::Kitty),
    );

    match protocol {
        ImageProtocol::Kitty => {
            if let Some(image_id) = options.image_id {
                register_kitty_image_metadata(KittyImageMetadata {
                    image_id,
                    columns: size.columns,
                    rows: size.rows,
                    width_px: image_dimensions.width_px,
                    height_px: image_dimensions.height_px,
                });
            }
            let sequence = encode_kitty(
                base64_data,
                Some(EncodeKittyOptions {
                    columns: Some(size.columns),
                    rows: Some(size.rows),
                    image_id: options.image_id,
                    move_cursor: options.move_cursor,
                }),
            );
            Some(RenderImageResult {
                sequence,
                columns: size.columns,
                rows: size.rows,
                image_id: options.image_id,
            })
        }
        ImageProtocol::Iterm2 => {
            let sequence = encode_iterm2(
                base64_data,
                Some(EncodeITerm2Options {
                    width: Some(NumberOrString::Number(size.columns as f64)),
                    height: Some(NumberOrString::Text("auto".to_string())),
                    preserve_aspect_ratio: Some(options.preserve_aspect_ratio.unwrap_or(true)),
                    ..Default::default()
                }),
            );
            Some(RenderImageResult {
                sequence,
                columns: size.columns,
                rows: size.rows,
                image_id: None,
            })
        }
    }
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
    format!("\u{1b}]8;;{url}\u{1b}\\{text}\u{1b}]8;;\u{1b}\\")
}

/// Shorten home-prefixed absolute paths to ~/... for compact display.
fn shorten_image_path(filename: &str) -> String {
    let home = pi_js::env::home_dir();
    if !home.is_empty() {
        if let Some(rest) = filename.strip_prefix(&home) {
            if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') {
                return format!("~{rest}");
            }
        }
    }
    filename.to_string()
}

fn path_to_file_url_href(filename: &str) -> String {
    if let Ok(url) = url::Url::from_file_path(filename) {
        return url.into();
    }
    let normalized = filename.replace('\\', "/");
    if normalized.starts_with('/') {
        format!("file://{}", pi_js::uri::encode_uri(&normalized))
    } else {
        format!("file:///{}", pi_js::uri::encode_uri(&normalized))
    }
}

/// Text fallback when the terminal cannot render inline images.
/// Absolute paths are shown shortened (~/...) and, when OSC 8 hyperlinks are
/// available, linked to file:// so the full path remains openable.
pub fn image_fallback(mime_type: &str, dimensions: Option<ImageDimensions>, filename: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(filename) = filename.filter(|s| !s.is_empty()) {
        let display = shorten_image_path(filename);
        if get_capabilities().hyperlinks && pi_js::path::is_absolute(filename) {
            parts.push(hyperlink(&display, &path_to_file_url_href(filename)));
        } else {
            parts.push(display);
        }
    }
    parts.push(format!("[{mime_type}]"));
    if let Some(dimensions) = dimensions {
        parts.push(format!("{}x{}", dimensions.width_px, dimensions.height_px));
    }
    format!("[Image: {}]", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn parses_png_jpeg_gif_and_webp_dimensions_and_allocates_ids() {
        let mut png = vec![0u8; 24];
        png[0..8].copy_from_slice(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
        png[16..20].copy_from_slice(&320u32.to_be_bytes());
        png[20..24].copy_from_slice(&240u32.to_be_bytes());
        let png_b64 = pi_js::b64::encode(&png);
        assert_eq!(
            get_image_dimensions(&png_b64, "image/png"),
            Some(ImageDimensions {
                width_px: 320,
                height_px: 240,
            })
        );

        let jpeg = vec![
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x01, 0x90, 0x02, 0x80, 0x03,
            0x00, 0x00,
        ];
        assert_eq!(
            get_image_dimensions(&pi_js::b64::encode(&jpeg), "image/jpeg"),
            Some(ImageDimensions {
                width_px: 640,
                height_px: 400,
            })
        );

        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&128u16.to_le_bytes());
        gif.extend_from_slice(&64u16.to_le_bytes());
        assert_eq!(
            get_image_dimensions(&pi_js::b64::encode(&gif), "image/gif"),
            Some(ImageDimensions {
                width_px: 128,
                height_px: 64,
            })
        );

        let mut vp8 = vec![0u8; 30];
        vp8[0..4].copy_from_slice(b"RIFF");
        vp8[8..16].copy_from_slice(b"WEBPVP8 ");
        vp8[26..28].copy_from_slice(&800u16.to_le_bytes());
        vp8[28..30].copy_from_slice(&600u16.to_le_bytes());
        assert_eq!(
            get_webp_dimensions(&pi_js::b64::encode(&vp8)),
            Some(ImageDimensions {
                width_px: 800,
                height_px: 600,
            })
        );

        let mut vp8l = vec![0u8; 30];
        vp8l[0..4].copy_from_slice(b"RIFF");
        vp8l[8..16].copy_from_slice(b"WEBPVP8L");
        let bits = 99u32 | (49u32 << 14);
        vp8l[21..25].copy_from_slice(&bits.to_le_bytes());
        assert_eq!(
            get_webp_dimensions(&pi_js::b64::encode(&vp8l)),
            Some(ImageDimensions {
                width_px: 100,
                height_px: 50,
            })
        );

        let mut vp8x = vec![0u8; 30];
        vp8x[0..4].copy_from_slice(b"RIFF");
        vp8x[8..16].copy_from_slice(b"WEBPVP8X");
        let w = 1023u32.to_le_bytes();
        let h = 511u32.to_le_bytes();
        vp8x[24..27].copy_from_slice(&w[..3]);
        vp8x[27..30].copy_from_slice(&h[..3]);
        assert_eq!(
            get_image_dimensions(&pi_js::b64::encode(&vp8x), "image/webp"),
            Some(ImageDimensions {
                width_px: 1024,
                height_px: 512,
            })
        );
        assert_eq!(get_image_dimensions(&png_b64, "image/svg+xml"), None);

        let id1 = allocate_image_id();
        let id2 = allocate_image_id();
        assert!((1..=i64::from(u32::MAX)).contains(&id1));
        assert!((1..=i64::from(u32::MAX)).contains(&id2));
    }
}

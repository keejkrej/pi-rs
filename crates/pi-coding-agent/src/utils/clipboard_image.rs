//! Port of packages/coding-agent/src/utils/clipboard-image.ts

use indexmap::IndexMap;
use pi_tui::{NativeClipboard as _, get_native_clipboard};

use crate::utils::clipboard_command::{ClipboardCommandOptions, run_clipboard_command};
use crate::utils::mime::detect_supported_image_mime_type;
use crate::utils::photon::{PhotonImage, load_photon};
use crate::utils::wsl::is_wsl;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

#[derive(Clone, Debug, Default)]
pub struct ReadClipboardImageOptions {
    pub env: Option<IndexMap<String, String>>,
    pub platform: Option<String>,
}

const SUPPORTED_IMAGE_MIME_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];
const DEFAULT_LIST_TIMEOUT_MS: u64 = 1000;
const DEFAULT_POWERSHELL_TIMEOUT_MS: u64 = 5000;

pub fn is_wayland_session(env: Option<&IndexMap<String, String>>) -> bool {
    let get = |k: &str| match env {
        Some(map) => map.get(k).cloned(),
        None => pi_js::env::var(k),
    };
    get("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) || get("XDG_SESSION_TYPE").as_deref() == Some("wayland")
}

fn base_mime_type(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .unwrap_or(mime_type)
        .trim()
        .to_ascii_lowercase()
}

pub fn extension_for_image_mime_type(mime_type: &str) -> Option<&'static str> {
    match base_mime_type(mime_type).as_str() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn select_preferred_image_mime_type(mime_types: &[String]) -> Option<String> {
    let normalized: Vec<(String, String)> = mime_types
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| (t.to_owned(), base_mime_type(t)))
        .collect();

    for &preferred in SUPPORTED_IMAGE_MIME_TYPES {
        if let Some((raw, _)) = normalized.iter().find(|(_, base)| base == preferred) {
            return Some(raw.clone());
        }
    }

    normalized
        .into_iter()
        .find(|(_, base)| base.starts_with("image/"))
        .map(|(raw, _)| raw)
}

fn is_supported_image_mime_type(mime_type: &str) -> bool {
    let base = base_mime_type(mime_type);
    SUPPORTED_IMAGE_MIME_TYPES.contains(&base.as_str())
}

async fn convert_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let _photon = load_photon().await?;
    let image = PhotonImage::new_from_byteslice(bytes).ok()?;
    image.get_bytes().ok()
}

/// Outer `Option`: `None` means backend failed, `Some(None)` means no image.
async fn read_clipboard_image_via_wl_paste() -> Option<Option<ClipboardImage>> {
    let list = run_clipboard_command(
        "wl-paste",
        &["--list-types"],
        ClipboardCommandOptions {
            input: None,
            timeout_ms: Some(DEFAULT_LIST_TIMEOUT_MS),
            max_buffer_bytes: None,
        },
    )
    .await?;

    let types: Vec<String> = String::from_utf8_lossy(&list)
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    let Some(selected_type) = select_preferred_image_mime_type(&types) else {
        return Some(None);
    };

    let data = run_clipboard_command(
        "wl-paste",
        &["--type", &selected_type, "--no-newline"],
        ClipboardCommandOptions::default(),
    )
    .await?;
    if data.is_empty() {
        return Some(None);
    }

    Some(Some(ClipboardImage {
        bytes: data,
        mime_type: base_mime_type(&selected_type),
    }))
}

async fn read_clipboard_image_via_power_shell() -> Option<ClipboardImage> {
    let tmp_file = pi_js::path::join(&[
        &pi_js::env::tmp_dir(),
        &format!("pi-wsl-clip-{}.png", pi_js::crypto::random_uuid()),
    ]);

    let result = async {
        let win_path_bytes = run_clipboard_command(
            "wslpath",
            &["-w", &tmp_file],
            ClipboardCommandOptions {
                input: None,
                timeout_ms: Some(DEFAULT_LIST_TIMEOUT_MS),
                max_buffer_bytes: None,
            },
        )
        .await?;
        let win_path = String::from_utf8_lossy(&win_path_bytes).trim().to_owned();
        if win_path.is_empty() {
            return None;
        }
        let ps_quoted = win_path.replace('\'', "''");
        let ps_script = [
            "Add-Type -AssemblyName System.Windows.Forms".to_owned(),
            "Add-Type -AssemblyName System.Drawing".to_owned(),
            format!("$path = '{ps_quoted}'"),
            "$img = [System.Windows.Forms.Clipboard]::GetImage()".to_owned(),
            "if ($img) { $img.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); Write-Output 'ok' } else { Write-Output 'empty' }".to_owned(),
        ]
        .join("; ");

        let output_bytes = run_clipboard_command(
            "powershell.exe",
            &["-NoProfile", "-Command", &ps_script],
            ClipboardCommandOptions {
                input: None,
                timeout_ms: Some(DEFAULT_POWERSHELL_TIMEOUT_MS),
                max_buffer_bytes: None,
            },
        )
        .await?;

        if String::from_utf8_lossy(&output_bytes).trim() != "ok" {
            return None;
        }

        let bytes = pi_js::fs::read_file(&tmp_file).ok()?;
        if bytes.is_empty() {
            return None;
        }

        Some(ClipboardImage {
            bytes,
            mime_type: "image/png".to_owned(),
        })
    }
    .await;

    let _ = pi_js::fs::unlink(&tmp_file);
    result
}

async fn read_clipboard_image_via_xclip() -> Option<Option<ClipboardImage>> {
    let targets = run_clipboard_command(
        "xclip",
        &["-selection", "clipboard", "-t", "TARGETS", "-o"],
        ClipboardCommandOptions {
            input: None,
            timeout_ms: Some(DEFAULT_LIST_TIMEOUT_MS),
            max_buffer_bytes: None,
        },
    )
    .await?;

    let candidate_types: Vec<String> = String::from_utf8_lossy(&targets)
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    let Some(preferred) = select_preferred_image_mime_type(&candidate_types) else {
        return Some(None);
    };

    let data = run_clipboard_command(
        "xclip",
        &["-selection", "clipboard", "-t", &preferred, "-o"],
        ClipboardCommandOptions::default(),
    )
    .await?;
    if data.is_empty() {
        return Some(None);
    }

    Some(Some(ClipboardImage {
        bytes: data,
        mime_type: base_mime_type(&preferred),
    }))
}

async fn read_clipboard_image_via_native_clipboard() -> Option<Option<ClipboardImage>> {
    let clipboard = get_native_clipboard()?;
    let bytes = match clipboard.get_image().await {
        Ok(Some(Some(b))) => Some(b),
        Ok(Some(None)) => return Some(None),
        Ok(None) | Err(_) => return None,
    }?;
    if bytes.is_empty() {
        return Some(None);
    }
    let mime_type = detect_supported_image_mime_type(&bytes)
        .unwrap_or("application/octet-stream")
        .to_owned();
    Some(Some(ClipboardImage { bytes, mime_type }))
}

pub async fn read_clipboard_image(options: Option<ReadClipboardImageOptions>) -> Option<ClipboardImage> {
    let opts = options.unwrap_or_default();
    let env = opts.env.unwrap_or_else(pi_js::env::vars);
    let platform = opts.platform.unwrap_or_else(|| pi_js::env::platform().to_owned());

    if env.get("TERMUX_VERSION").is_some_and(|v| !v.is_empty()) {
        return None;
    }

    let mut image: Option<Option<ClipboardImage>> = None;

    if platform == "linux" {
        let wsl = is_wsl(Some(&env));
        if is_wayland_session(Some(&env)) || wsl {
            image = read_clipboard_image_via_wl_paste().await;
        }
        if image.is_none() {
            image = read_clipboard_image_via_xclip().await;
        }
        if !matches!(image, Some(Some(_)))
            && wsl
            && let Some(ps_img) = read_clipboard_image_via_power_shell().await
        {
            image = Some(Some(ps_img));
        }
        if image.is_none() {
            image = read_clipboard_image_via_native_clipboard().await;
        }
    } else {
        image = read_clipboard_image_via_native_clipboard().await;
    }

    let img = image.flatten()?;
    if !is_supported_image_mime_type(&img.mime_type) {
        let png_bytes = convert_to_png(&img.bytes).await?;
        return Some(ClipboardImage {
            bytes: png_bytes,
            mime_type: "image/png".to_owned(),
        });
    }

    Some(img)
}

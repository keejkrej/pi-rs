//! Port of packages/coding-agent/src/utils/tools-manager.ts

use std::sync::LazyLock;

use indexmap::IndexMap;
use pi_js::fetch::{Headers, Request};
use pi_js::{Error, Result};

use crate::config::{VERSION, get_bin_dir};
use crate::utils::child_process::{SpawnProcessOptions, spawn_process_sync};
use crate::utils::management_http::{FetchRetryOptions, extract_error_cause_chain, fetch_with_retry};
use crate::utils::pi_user_agent::get_pi_user_agent;

const NETWORK_TIMEOUT_MS: u64 = 10_000;
const DOWNLOAD_TIMEOUT_MS: u64 = 120_000;

fn is_offline_mode_enabled() -> bool {
    let Some(value) = pi_js::env::var("PI_OFFLINE") else {
        return false;
    };
    if value.is_empty() {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    value == "1" || lower == "true" || lower == "yes"
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedTool {
    Fd,
    Rg,
}

impl ManagedTool {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fd => "fd",
            Self::Rg => "rg",
        }
    }
}

struct ToolConfig {
    name: &'static str,
    repo: &'static str,
    binary_name: &'static str,
    system_binary_names: &'static [&'static str],
    tag_prefix: &'static str,
}

fn tool_config(tool: ManagedTool) -> ToolConfig {
    match tool {
        ManagedTool::Fd => ToolConfig {
            name: "fd",
            repo: "sharkdp/fd",
            binary_name: "fd",
            system_binary_names: &["fd", "fdfind"],
            tag_prefix: "v",
        },
        ManagedTool::Rg => ToolConfig {
            name: "ripgrep",
            repo: "BurntSushi/ripgrep",
            binary_name: "rg",
            system_binary_names: &["rg"],
            tag_prefix: "",
        },
    }
}

// Map platform/arch to the release target suffix shared by fd and ripgrep.
// Only x64 and arm64 are mapped (#11209).
fn get_release_target(plat: &str, arch: &str) -> Option<String> {
    let arch_str = match arch {
        "arm64" => "aarch64",
        "x64" => "x86_64",
        _ => return None,
    };
    match plat {
        "darwin" => Some(format!("{arch_str}-apple-darwin.tar.gz")),
        "linux" => Some(format!("{arch_str}-unknown-linux-musl.tar.gz")),
        "win32" => Some(format!("{arch_str}-pc-windows-msvc.zip")),
        _ => None,
    }
}

fn get_asset_name(tool: ManagedTool, version: &str, plat: &str, arch: &str) -> Option<String> {
    let target = get_release_target(plat, arch)?;
    match tool {
        ManagedTool::Fd => Some(format!("fd-v{version}-{target}")),
        ManagedTool::Rg => Some(format!("ripgrep-{version}-{target}")),
    }
}

fn command_exists(cmd: &str) -> bool {
    let res = spawn_process_sync(
        cmd,
        &["--version"],
        SpawnProcessOptions {
            stdout_piped: true,
            stderr_piped: true,
            ..Default::default()
        },
    );
    res.error.is_none()
}

pub fn get_tool_path(tool: ManagedTool) -> Option<String> {
    let config = tool_config(tool);
    let tools_dir = get_bin_dir();
    let ext = if pi_js::env::platform() == "win32" { ".exe" } else { "" };
    let local_path = pi_js::path::join(&[&tools_dir, &format!("{}{ext}", config.binary_name)]);
    if pi_js::fs::exists(&local_path) {
        return Some(local_path);
    }

    for &sys_name in config.system_binary_names {
        if command_exists(sys_name) {
            return Some(sys_name.to_owned());
        }
    }

    None
}

pub async fn get_latest_version(repo: &str) -> Result<String> {
    let url = format!("https://github.com/{repo}/releases/latest");
    let mut headers = Headers::new();
    headers.set("User-Agent", &get_pi_user_agent(*VERSION));
    let req = Request {
        method: "GET".to_owned(),
        url,
        headers,
        body: None,
        signal: None,
    };

    let mut opts = FetchRetryOptions::default();
    opts.timeout_ms = Some(NETWORK_TIMEOUT_MS);
    let response = fetch_with_retry(req, opts).await?;

    let location = if (300..400).contains(&response.status()) {
        response.headers().get("location")
    } else if response.url().contains("/releases/tag/") {
        Some(response.url().to_owned())
    } else {
        None
    };

    let Some(location) = location else {
        return Err(Error::msg(format!(
            "Failed to resolve latest {repo} release: HTTP {} without redirect",
            response.status()
        )));
    };

    let base = url::Url::parse("https://github.com").map_err(|err| Error::msg(err.to_string()))?;
    let parsed = base.join(&location).map_err(|err| Error::msg(err.to_string()))?;
    let tag = parsed.path().split('/').rfind(|s| !s.is_empty()).unwrap_or("");

    if tag.is_empty() || !location.contains("/releases/tag/") {
        return Err(Error::msg(format!(
            "Failed to resolve latest {repo} release: unexpected redirect to {location}"
        )));
    }

    let decoded = pi_js::uri::decode_uri_component(tag).unwrap_or_else(|_| tag.to_owned());
    Ok(decoded.strip_prefix('v').unwrap_or(&decoded).to_owned())
}

async fn download_file(url: &str, dest: &str) -> Result<()> {
    let req = Request::get(url);
    let mut opts = FetchRetryOptions::default();
    opts.timeout_ms = Some(DOWNLOAD_TIMEOUT_MS);
    let response = fetch_with_retry(req, opts).await?;

    if !response.ok() {
        return Err(Error::msg(format!(
            "Download failed with HTTP {}: {url}",
            response.status()
        )));
    }

    let bytes = response.bytes().await?;
    pi_js::fs::promises::write_file(dest, bytes, None).await
}

fn find_binary_recursively(root_dir: &str, binary_file_name: &str) -> Option<String> {
    let mut stack = vec![root_dir.to_owned()];
    while let Some(current_dir) = stack.pop() {
        let Ok(entries) = pi_js::fs::readdir_with_file_types(&current_dir) else {
            continue;
        };
        for entry in entries {
            let full_path = pi_js::path::join(&[&current_dir, &entry.name]);
            if entry.is_file() && entry.name == binary_file_name {
                return Some(full_path);
            }
            if entry.is_directory() {
                stack.push(full_path);
            }
        }
    }
    None
}

fn format_spawn_failure(res: &crate::utils::child_process::SyncProcessOutput) -> String {
    if let Some(ref err) = res.error
        && !err.is_empty()
    {
        return err.clone();
    }
    let stderr = res.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_owned();
    }
    let stdout = res.stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_owned();
    }
    match res.status {
        Some(code) => format!("exit status {code}"),
        None => "exit status unknown".to_owned(),
    }
}

fn run_extraction_command(command: &str, args: &[&str]) -> Option<String> {
    let res = spawn_process_sync(
        command,
        args,
        SpawnProcessOptions {
            stdout_piped: true,
            stderr_piped: true,
            ..Default::default()
        },
    );
    if res.error.is_none() && res.status == Some(0) {
        return None;
    }
    Some(format!("{command}: {}", format_spawn_failure(&res)))
}

fn extract_tar_gz_archive(archive_path: &str, extract_dir: &str, asset_name: &str) -> Result<()> {
    if let Some(failure) = run_extraction_command("tar", &["xzf", archive_path, "-C", extract_dir]) {
        return Err(Error::msg(format!("Failed to extract {asset_name}: {failure}")));
    }
    Ok(())
}

fn get_windows_tar_command() -> String {
    let sys_root = pi_js::env::var("SystemRoot").filter(|s| !s.is_empty());
    if let Some(root) = sys_root {
        let candidate = pi_js::path::join(&[&root, "System32", "tar.exe"]);
        if pi_js::fs::exists(&candidate) {
            return candidate;
        }
    }
    "tar.exe".to_owned()
}

fn extract_zip_archive(archive_path: &str, extract_dir: &str, asset_name: &str) -> Result<()> {
    let mut failures = Vec::new();
    if pi_js::env::platform() == "win32" {
        let win_tar = get_windows_tar_command();
        match run_extraction_command(&win_tar, &["xf", archive_path, "-C", extract_dir]) {
            None => return Ok(()),
            Some(f) => failures.push(f),
        }
        let script = "& { param($archive, $destination) $ErrorActionPreference = 'Stop'; Expand-Archive -LiteralPath $archive -DestinationPath $destination -Force }";
        match run_extraction_command(
            "powershell.exe",
            &[
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                script,
                archive_path,
                extract_dir,
            ],
        ) {
            None => return Ok(()),
            Some(f) => failures.push(f),
        }
    } else {
        match run_extraction_command("unzip", &["-q", archive_path, "-d", extract_dir]) {
            None => return Ok(()),
            Some(f) => failures.push(f),
        }
        match run_extraction_command("tar", &["xf", archive_path, "-C", extract_dir]) {
            None => return Ok(()),
            Some(f) => failures.push(f),
        }
    }

    Err(Error::msg(format!(
        "Failed to extract {asset_name}: {}",
        failures.join("; ")
    )))
}

async fn download_tool(tool: ManagedTool) -> Result<String> {
    let config = tool_config(tool);
    let plat = pi_js::env::platform();
    let arch = pi_js::env::arch();
    let tools_dir = get_bin_dir();

    let version = if tool == ManagedTool::Fd && plat == "darwin" && arch == "x64" {
        "10.3.0".to_owned()
    } else {
        get_latest_version(config.repo).await?
    };

    let Some(asset_name) = get_asset_name(tool, &version, plat, arch) else {
        return Err(Error::msg(format!("Unsupported platform: {plat}/{arch}")));
    };

    pi_js::fs::mkdir(&tools_dir, true, None)?;
    let download_url = format!(
        "https://github.com/{}/releases/download/{}{}/{}",
        config.repo, config.tag_prefix, version, asset_name
    );
    let archive_path = pi_js::path::join(&[&tools_dir, &asset_name]);
    let binary_ext = if plat == "win32" { ".exe" } else { "" };
    let binary_file_name = format!("{}{binary_ext}", config.binary_name);
    let binary_path = pi_js::path::join(&[&tools_dir, &binary_file_name]);

    download_file(&download_url, &archive_path).await?;

    let extract_dir = pi_js::path::join(&[
        &tools_dir,
        &format!(
            "extract_tmp_{}_{}_{}_{}",
            config.binary_name,
            pi_js::env::pid(),
            pi_js::time::now_ms(),
            pi_js::crypto::random_uuid()
        ),
    ]);
    pi_js::fs::mkdir(&extract_dir, true, None)?;

    let res = (|| -> Result<()> {
        if asset_name.ends_with(".tar.gz") {
            extract_tar_gz_archive(&archive_path, &extract_dir, &asset_name)?;
        } else if asset_name.ends_with(".zip") {
            extract_zip_archive(&archive_path, &extract_dir, &asset_name)?;
        } else {
            return Err(Error::msg(format!("Unsupported archive format: {asset_name}")));
        }

        let stem = asset_name
            .strip_suffix(".tar.gz")
            .or_else(|| asset_name.strip_suffix(".zip"))
            .unwrap_or(&asset_name);
        let extracted_dir = pi_js::path::join(&[&extract_dir, stem]);
        let candidates = [
            pi_js::path::join(&[&extracted_dir, &binary_file_name]),
            pi_js::path::join(&[&extract_dir, &binary_file_name]),
        ];
        let mut extracted_binary = candidates.into_iter().find(|c| pi_js::fs::exists(c));
        if extracted_binary.is_none() {
            extracted_binary = find_binary_recursively(&extract_dir, &binary_file_name);
        }

        let Some(found) = extracted_binary else {
            return Err(Error::msg(format!(
                "Binary not found in archive: expected {binary_file_name} under {extract_dir}"
            )));
        };

        pi_js::fs::rename(&found, &binary_path)?;
        if plat != "win32" {
            pi_js::fs::chmod(&binary_path, 0o755)?;
        }
        Ok(())
    })();

    let _ = pi_js::fs::rm(&archive_path, false, true);
    let _ = pi_js::fs::rm(&extract_dir, true, true);
    res?;

    Ok(binary_path)
}

static TERMUX_PACKAGES: LazyLock<IndexMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = IndexMap::new();
    m.insert("fd", "fd");
    m.insert("rg", "ripgrep");
    m
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatusKind {
    Info,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolStatus {
    pub r#type: ToolStatusKind,
    pub message: String,
}

pub async fn ensure_tool<F>(tool: ManagedTool, mut on_status: Option<F>) -> Option<String>
where
    F: FnMut(ToolStatus),
{
    if let Some(existing_path) = get_tool_path(tool) {
        return Some(existing_path);
    }

    let config = tool_config(tool);
    if is_offline_mode_enabled() {
        if let Some(cb) = on_status.as_mut() {
            cb(ToolStatus {
                r#type: ToolStatusKind::Warning,
                message: format!("{} not found. Offline mode enabled, skipping download.", config.name),
            });
        }
        return None;
    }

    if pi_js::env::platform() == "android" {
        let pkg_name = TERMUX_PACKAGES.get(tool.as_str()).copied().unwrap_or(tool.as_str());
        if let Some(cb) = on_status.as_mut() {
            cb(ToolStatus {
                r#type: ToolStatusKind::Warning,
                message: format!("{} not found. Install with: pkg install {pkg_name}", config.name),
            });
        }
        return None;
    }

    let plat = pi_js::env::platform();
    let arch = pi_js::env::arch();
    if get_release_target(&plat, &arch).is_none() {
        if let Some(cb) = on_status.as_mut() {
            cb(ToolStatus {
                r#type: ToolStatusKind::Warning,
                message: format!(
                    "{} not found. Automatic download is not supported on {plat}/{arch}. Install {} with your system package manager.",
                    config.name, config.name
                ),
            });
        }
        return None;
    }

    if let Some(cb) = on_status.as_mut() {
        cb(ToolStatus {
            r#type: ToolStatusKind::Info,
            message: format!("{} not found. Downloading...", config.name),
        });
    }

    match download_tool(tool).await {
        Ok(path) => {
            if let Some(cb) = on_status.as_mut() {
                cb(ToolStatus {
                    r#type: ToolStatusKind::Info,
                    message: format!("{} installed to {path}", config.name),
                });
            }
            Some(path)
        }
        Err(err) => {
            let messages = extract_error_cause_chain(&err);
            let joined = if messages.is_empty() {
                err.to_string()
            } else {
                messages.join(": ")
            };
            if let Some(cb) = on_status.as_mut() {
                cb(ToolStatus {
                    r#type: ToolStatusKind::Warning,
                    message: format!("Failed to download {}: {joined}", config.name),
                });
            }
            None
        }
    }
}

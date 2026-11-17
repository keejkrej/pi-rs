//! Port of packages/coding-agent/src/config.ts

use std::sync::{LazyLock, Mutex};

use indexmap::IndexSet;
use serde::Deserialize;

use crate::utils::child_process::{SpawnProcessOptions, spawn_process_sync};
use crate::utils::paths::normalize_path;
use crate::utils::text::strip_bom;

const DEFAULT_SHARE_VIEWER_URL: &str = "https://pi.dev/session/";
const FALLBACK_PACKAGE_JSON: &str = include_str!("../package/package.json");

static EMBEDDED_QUICKJS_WASM_PATH: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PackageJson {
    name: Option<String>,
    version: Option<String>,
    pi_config: Option<PiConfigSection>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PiConfigSection {
    name: Option<String>,
    config_dir: Option<String>,
}

#[derive(Clone, Debug)]
struct PackageMetadata {
    package_name: String,
    app_name: String,
    app_title: String,
    config_dir_name: String,
    version: String,
    env_agent_dir: String,
    env_session_dir: String,
}

fn load_package_metadata() -> PackageMetadata {
    let raw = pi_js::fs::read_to_string(&get_package_json_path())
        .ok()
        .unwrap_or_else(|| FALLBACK_PACKAGE_JSON.to_owned());
    let pkg: PackageJson = pi_js::json::parse_as(strip_bom(&raw)).unwrap_or_default();
    let pi_config_name = pkg.pi_config.as_ref().and_then(|c| c.name.clone());
    let package_name = pkg
        .name
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "@earendil-works/pi-coding-agent".to_owned());
    let app_name = pi_config_name
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "pi".to_owned());
    let app_title = if pi_config_name.as_ref().is_some_and(|s| !s.is_empty()) {
        app_name.clone()
    } else {
        "π".to_owned()
    };
    let config_dir_name = pkg
        .pi_config
        .and_then(|c| c.config_dir)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ".pi".to_owned());
    let version = pkg
        .version
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
    let upper = app_name.to_uppercase();
    PackageMetadata {
        package_name,
        app_name,
        app_title,
        config_dir_name,
        version,
        env_agent_dir: format!("{upper}_CODING_AGENT_DIR"),
        env_session_dir: format!("{upper}_CODING_AGENT_SESSION_DIR"),
    }
}

static META: LazyLock<PackageMetadata> = LazyLock::new(load_package_metadata);

pub static PACKAGE_NAME: LazyLock<&'static str> =
    LazyLock::new(|| Box::leak(META.package_name.clone().into_boxed_str()));
pub static APP_NAME: LazyLock<&'static str> = LazyLock::new(|| Box::leak(META.app_name.clone().into_boxed_str()));
pub static APP_TITLE: LazyLock<&'static str> = LazyLock::new(|| Box::leak(META.app_title.clone().into_boxed_str()));
pub static CONFIG_DIR_NAME: LazyLock<&'static str> =
    LazyLock::new(|| Box::leak(META.config_dir_name.clone().into_boxed_str()));
pub static VERSION: LazyLock<&'static str> = LazyLock::new(|| Box::leak(META.version.clone().into_boxed_str()));
pub static ENV_AGENT_DIR: LazyLock<&'static str> =
    LazyLock::new(|| Box::leak(META.env_agent_dir.clone().into_boxed_str()));
pub static ENV_SESSION_DIR: LazyLock<&'static str> =
    LazyLock::new(|| Box::leak(META.env_session_dir.clone().into_boxed_str()));

pub const IS_BUN_BINARY: bool = false;
pub const IS_BUN_RUNTIME: bool = false;
pub const IS_BUNDLED_NODE: bool = false;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallMethod {
    BunBinary,
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelfUpdateCommandStep {
    pub command: String,
    pub args: Vec<String>,
    pub display: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelfUpdateCommand {
    pub command: String,
    pub args: Vec<String>,
    pub display: String,
    pub steps: Option<Vec<SelfUpdateCommandStep>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelfUpdatePackageTarget {
    pub package_name: String,
    pub install_spec: String,
}

impl From<&str> for SelfUpdatePackageTarget {
    fn from(value: &str) -> Self {
        Self {
            package_name: value.to_owned(),
            install_spec: value.to_owned(),
        }
    }
}

impl From<String> for SelfUpdatePackageTarget {
    fn from(value: String) -> Self {
        Self {
            package_name: value.clone(),
            install_spec: value,
        }
    }
}

fn make_self_update_command_step(command: &str, args: Vec<String>) -> SelfUpdateCommandStep {
    let mut parts = Vec::with_capacity(1 + args.len());
    parts.push(if command.chars().any(char::is_whitespace) {
        format!("\"{command}\"")
    } else {
        command.to_owned()
    });
    for arg in &args {
        if arg.chars().any(char::is_whitespace) {
            parts.push(format!("\"{arg}\""));
        } else {
            parts.push(arg.clone());
        }
    }
    SelfUpdateCommandStep {
        command: command.to_owned(),
        args,
        display: parts.join(" "),
    }
}

fn make_self_update_command(
    install_step: SelfUpdateCommandStep,
    uninstall_step: Option<SelfUpdateCommandStep>,
) -> SelfUpdateCommand {
    match uninstall_step {
        None => SelfUpdateCommand {
            command: install_step.command,
            args: install_step.args,
            display: install_step.display,
            steps: None,
        },
        Some(uninst) => {
            let display = format!("{} && {}", uninst.display, install_step.display);
            SelfUpdateCommand {
                command: install_step.command.clone(),
                args: install_step.args.clone(),
                display,
                steps: Some(vec![uninst, install_step]),
            }
        }
    }
}

fn has_exe_dir_package_json() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let dir_str = dir.to_string_lossy().into_owned();
    if pi_js::fs::exists(&pi_js::path::join(&[&dir_str, "package.json"])) {
        Some(dir_str)
    } else {
        None
    }
}

pub fn detect_install_method() -> InstallMethod {
    if has_exe_dir_package_json().is_some() {
        return InstallMethod::BunBinary;
    }

    let exe_str = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let resolved = format!("{}\0{}", env!("CARGO_MANIFEST_DIR"), exe_str)
        .to_ascii_lowercase()
        .replace('\\', "/");

    if resolved.contains("/pnpm/") || resolved.contains("/.pnpm/") {
        return InstallMethod::Pnpm;
    }
    if resolved.contains("/yarn/") || resolved.contains("/.yarn/") {
        return InstallMethod::Yarn;
    }
    if resolved.contains("/install/global/node_modules/") {
        return InstallMethod::Bun;
    }
    if resolved.contains("/npm/") || resolved.contains("/node_modules/") {
        return InstallMethod::Npm;
    }

    InstallMethod::Unknown
}

fn read_command_output(command: &str, args: &[&str], require_success: bool) -> pi_js::Result<Option<String>> {
    let result = spawn_process_sync(command, args, SpawnProcessOptions::default());
    if result.status == Some(0) {
        let trimmed = pi_js::str16::trim(&result.stdout);
        return Ok(if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        });
    }
    if require_success {
        let reason = if let Some(err) = result.error.filter(|s| !s.is_empty()) {
            err
        } else if !pi_js::str16::trim(&result.stderr).is_empty() {
            pi_js::str16::trim(&result.stderr).to_owned()
        } else if let Some(code) = result.status {
            format!("exit code {code}")
        } else {
            "exit code unknown".to_owned()
        };
        let mut cmdline = Vec::with_capacity(1 + args.len());
        cmdline.push(command);
        cmdline.extend_from_slice(args);
        return Err(pi_js::Error::msg(format!(
            "Failed to run {}: {reason}",
            cmdline.join(" ")
        )));
    }
    Ok(None)
}

fn get_inferred_npm_install() -> Option<(String, String)> {
    let package_dir = get_package_dir();
    let use_win = pi_js::env::platform() == "win32" || package_dir.contains('\\');
    let parent = if use_win {
        pi_js::path::win32::dirname(&package_dir)
    } else {
        pi_js::path::dirname(&package_dir)
    };
    let parent_base = if use_win {
        pi_js::path::win32::basename(&parent, None)
    } else {
        pi_js::path::basename(&parent, None)
    };
    let parent_dir = if use_win {
        pi_js::path::win32::dirname(&parent)
    } else {
        pi_js::path::dirname(&parent)
    };
    let parent_dir_base = if use_win {
        pi_js::path::win32::basename(&parent_dir, None)
    } else {
        pi_js::path::basename(&parent_dir, None)
    };

    let root = if parent_base.starts_with('@') && parent_dir_base == "node_modules" {
        Some(parent_dir)
    } else if parent_base == "node_modules" {
        Some(parent)
    } else {
        None
    }?;

    let root_parent = if use_win {
        pi_js::path::win32::dirname(&root)
    } else {
        pi_js::path::dirname(&root)
    };
    let root_parent_base = if use_win {
        pi_js::path::win32::basename(&root_parent, None)
    } else {
        pi_js::path::basename(&root_parent, None)
    };
    if root_parent_base == "lib" {
        let prefix = if use_win {
            pi_js::path::win32::dirname(&root_parent)
        } else {
            pi_js::path::dirname(&root_parent)
        };
        Some((root, prefix))
    } else {
        None
    }
}

fn get_self_update_command_for_method(
    method: InstallMethod,
    installed_package_name: &str,
    target: &SelfUpdatePackageTarget,
    npm_command: Option<&[String]>,
) -> Option<SelfUpdateCommand> {
    match method {
        InstallMethod::BunBinary | InstallMethod::Unknown => None,
        InstallMethod::Pnpm => {
            let pnpm_ok = read_command_output("pnpm", &["root", "-g"], false)
                .ok()
                .flatten()
                .is_some();
            let mut bin_dir_args = Vec::new();
            if !pnpm_ok {
                let pkg = get_package_dir().replace('\\', "/");
                if let Some(idx) = pkg.find("/.pnpm/") {
                    let prefix = &pkg[..idx];
                    if let Some(gidx) = prefix.rfind("/global/")
                        && (prefix[..gidx].contains('/') || prefix.starts_with("/global/"))
                    {
                        let bin_dir = pi_js::env::var("PNPM_HOME")
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| pi_js::path::dirname(&pi_js::path::dirname(prefix)));
                        bin_dir_args.push(format!("--config.global-bin-dir={bin_dir}"));
                    }
                }
            }
            let mut install_args = vec![
                "install".to_owned(),
                "-g".to_owned(),
                "--ignore-scripts".to_owned(),
                "--config.minimumReleaseAge=0".to_owned(),
            ];
            install_args.extend(bin_dir_args.clone());
            install_args.push(target.install_spec.clone());
            let install = make_self_update_command_step("pnpm", install_args);
            let uninstall = if target.package_name == installed_package_name {
                None
            } else {
                let mut uargs = vec!["remove".to_owned(), "-g".to_owned()];
                uargs.extend(bin_dir_args);
                uargs.push(installed_package_name.to_owned());
                Some(make_self_update_command_step("pnpm", uargs))
            };
            Some(make_self_update_command(install, uninstall))
        }
        InstallMethod::Yarn => {
            let install = make_self_update_command_step(
                "yarn",
                vec![
                    "global".to_owned(),
                    "add".to_owned(),
                    "--ignore-scripts".to_owned(),
                    target.install_spec.clone(),
                ],
            );
            let uninstall = if target.package_name == installed_package_name {
                None
            } else {
                Some(make_self_update_command_step(
                    "yarn",
                    vec![
                        "global".to_owned(),
                        "remove".to_owned(),
                        installed_package_name.to_owned(),
                    ],
                ))
            };
            Some(make_self_update_command(install, uninstall))
        }
        InstallMethod::Bun => {
            let install = make_self_update_command_step(
                "bun",
                vec![
                    "install".to_owned(),
                    "-g".to_owned(),
                    "--ignore-scripts".to_owned(),
                    "--minimum-release-age=0".to_owned(),
                    target.install_spec.clone(),
                ],
            );
            let uninstall = if target.package_name == installed_package_name {
                None
            } else {
                Some(make_self_update_command_step(
                    "bun",
                    vec![
                        "uninstall".to_owned(),
                        "-g".to_owned(),
                        installed_package_name.to_owned(),
                    ],
                ))
            };
            Some(make_self_update_command(install, uninstall))
        }
        InstallMethod::Npm => {
            let mut command = "npm".to_owned();
            let mut npm_args: Vec<String> = Vec::new();
            if let Some(slice) = npm_command
                && let Some((first, rest)) = slice.split_first()
            {
                command = first.clone();
                npm_args = rest.to_vec();
            }
            let inferred = if npm_command.is_some_and(|s| !s.is_empty()) {
                None
            } else {
                get_inferred_npm_install()
            };
            let mut prefix_args = npm_args;
            if let Some((_, prefix)) = inferred {
                prefix_args.push("--prefix".to_owned());
                prefix_args.push(prefix);
            }
            let mut install_args = prefix_args.clone();
            install_args.extend([
                "install".to_owned(),
                "-g".to_owned(),
                "--ignore-scripts".to_owned(),
                "--min-release-age=0".to_owned(),
                target.install_spec.clone(),
            ]);
            let install = make_self_update_command_step(&command, install_args);
            let uninstall = if target.package_name == installed_package_name {
                None
            } else {
                let mut uargs = prefix_args;
                uargs.extend([
                    "uninstall".to_owned(),
                    "-g".to_owned(),
                    installed_package_name.to_owned(),
                ]);
                Some(make_self_update_command_step(&command, uargs))
            };
            Some(make_self_update_command(install, uninstall))
        }
    }
}

fn get_global_package_roots(method: InstallMethod, npm_command: Option<&[String]>) -> Vec<String> {
    match method {
        InstallMethod::Npm => {
            let configured = npm_command.is_some_and(|s| !s.is_empty());
            let (command, npm_args) = match npm_command.and_then(|s| s.split_first()) {
                Some((first, rest)) => (first.as_str(), rest),
                None => ("npm", &[][..]),
            };
            let arg_refs: Vec<&str> = npm_args.iter().map(String::as_str).collect();
            if configured && command == "bun" {
                let mut args = arg_refs.clone();
                args.extend(["pm", "bin", "-g"]);
                let Ok(bun_bin) = read_command_output(command, &args, true) else {
                    return Vec::new();
                };
                let mut roots = vec![pi_js::path::join(&[
                    &pi_js::env::home_dir(),
                    ".bun",
                    "install",
                    "global",
                    "node_modules",
                ])];
                if let Some(bin) = bun_bin {
                    let parent = pi_js::path::dirname(&bin);
                    roots.push(pi_js::path::join(&[&parent, "install", "global", "node_modules"]));
                }
                return roots;
            }
            let mut args = arg_refs;
            args.extend(["root", "-g"]);
            let root = read_command_output(command, &args, configured).ok().flatten();
            let inferred = if configured {
                None
            } else {
                get_inferred_npm_install().map(|(r, _)| r)
            };
            [root, inferred].into_iter().flatten().collect()
        }
        InstallMethod::Pnpm => {
            if let Some(root) = read_command_output("pnpm", &["root", "-g"], false).ok().flatten() {
                return vec![root.clone(), pi_js::path::dirname(&root)];
            }
            Vec::new()
        }
        InstallMethod::Yarn => {
            if let Some(dir) = read_command_output("yarn", &["global", "dir"], false).ok().flatten() {
                let nm = pi_js::path::join(&[&dir, "node_modules"]);
                return vec![dir, nm];
            }
            Vec::new()
        }
        InstallMethod::Bun => {
            let bun_bin = read_command_output("bun", &["pm", "bin", "-g"], false).ok().flatten();
            let mut roots = vec![pi_js::path::join(&[
                &pi_js::env::home_dir(),
                ".bun",
                "install",
                "global",
                "node_modules",
            ])];
            if let Some(bin) = bun_bin {
                let parent = pi_js::path::dirname(&bin);
                roots.push(pi_js::path::join(&[&parent, "install", "global", "node_modules"]));
            }
            roots
        }
        InstallMethod::BunBinary | InstallMethod::Unknown => Vec::new(),
    }
}

fn normalize_existing_path_for_comparison(path: &str, resolve_symlinks: bool) -> Option<String> {
    let resolved = pi_js::path::resolve(&[path]);
    if !pi_js::fs::exists(&resolved) {
        return None;
    }
    let mut normalized = if resolve_symlinks {
        pi_js::fs::realpath(&resolved).ok()?
    } else {
        resolved
    };
    if pi_js::env::platform() == "win32" {
        normalized = normalized.to_lowercase();
    }
    Some(normalized)
}

fn get_path_comparison_candidates(path: &str) -> Vec<String> {
    let mut seen = IndexSet::new();
    for resolve_symlinks in [false, true] {
        if let Some(candidate) = normalize_existing_path_for_comparison(path, resolve_symlinks) {
            seen.insert(candidate);
        }
    }
    seen.into_iter().collect()
}

fn is_self_update_path_writable() -> bool {
    let package_dir = get_package_dir();
    let parent = pi_js::path::dirname(&package_dir);
    // Node constants.W_OK == 2
    pi_js::fs::access(&package_dir, 2).is_ok() && pi_js::fs::access(&parent, 2).is_ok()
}

fn is_managed_by_global_package_manager(method: InstallMethod, npm_command: Option<&[String]>) -> bool {
    let package_dir_candidates = get_path_comparison_candidates(&get_package_dir());
    let sep = pi_js::path::sep();
    get_global_package_roots(method, npm_command).into_iter().any(|root| {
        get_path_comparison_candidates(&root).into_iter().any(|norm_root| {
            let prefix = if norm_root.ends_with(sep) {
                norm_root
            } else {
                format!("{norm_root}{sep}")
            };
            package_dir_candidates.iter().any(|dir| dir.starts_with(&prefix))
        })
    })
}

pub fn get_self_update_command(
    package_name: &str,
    npm_command: Option<&[String]>,
    update_target: Option<SelfUpdatePackageTarget>,
) -> Option<SelfUpdateCommand> {
    let target = update_target.unwrap_or_else(|| SelfUpdatePackageTarget::from(package_name));
    let method = detect_install_method();
    let cmd = get_self_update_command_for_method(method, package_name, &target, npm_command)?;
    if !is_managed_by_global_package_manager(method, npm_command) || !is_self_update_path_writable() {
        return None;
    }
    Some(cmd)
}

pub fn get_self_update_unavailable_instruction(
    package_name: &str,
    npm_command: Option<&[String]>,
    update_target: Option<SelfUpdatePackageTarget>,
) -> String {
    let target = update_target.unwrap_or_else(|| SelfUpdatePackageTarget::from(package_name));
    let method = detect_install_method();
    if method == InstallMethod::BunBinary {
        return "Download from: https://github.com/earendil-works/pi/releases/latest".to_owned();
    }
    if let Some(command) = get_self_update_command_for_method(method, package_name, &target, npm_command) {
        let m_str = match method {
            InstallMethod::BunBinary => "bun-binary",
            InstallMethod::Npm => "npm",
            InstallMethod::Pnpm => "pnpm",
            InstallMethod::Yarn => "yarn",
            InstallMethod::Bun => "bun",
            InstallMethod::Unknown => "unknown",
        };
        if is_managed_by_global_package_manager(method, npm_command) && !is_self_update_path_writable() {
            return format!(
                "This installation is managed by a global {m_str} install, but the install path is not writable. Update it yourself with: {}",
                command.display
            );
        }
        return format!(
            "This installation is not managed by a global {m_str} install. Update it with the package manager, wrapper, or source checkout that provides it."
        );
    }
    format!(
        "Update {} using the package manager, wrapper, or source checkout that provides this installation.",
        target.install_spec
    )
}

pub fn get_update_instruction(package_name: &str) -> String {
    let method = detect_install_method();
    let target = SelfUpdatePackageTarget::from(package_name);
    if let Some(command) = get_self_update_command_for_method(method, package_name, &target, None) {
        return format!("Run: {}", command.display);
    }
    get_self_update_unavailable_instruction(package_name, None, Some(target))
}

pub fn find_node_package_dir(start_dir: &str) -> String {
    let mut dir = start_dir.to_owned();
    loop {
        if pi_js::fs::exists(&pi_js::path::join(&[&dir, "package.json"])) {
            let parent = pi_js::path::dirname(&dir);
            if pi_js::path::basename(&dir, None) == "dist"
                && pi_js::fs::exists(&pi_js::path::join(&[&parent, "package.json"]))
            {
                return parent;
            }
            return dir;
        }
        let parent = pi_js::path::dirname(&dir);
        if parent == dir {
            break;
        }
        dir = parent;
    }
    start_dir.to_owned()
}

pub fn get_package_dir() -> String {
    if let Some(env_dir) = pi_js::env::var("PI_PACKAGE_DIR").filter(|s| !s.is_empty()) {
        return normalize_path(&env_dir, Default::default());
    }
    if let Some(exe_dir) = has_exe_dir_package_json() {
        return exe_dir;
    }
    pi_js::path::join(&[env!("CARGO_MANIFEST_DIR"), "package"])
}

fn is_binary_layout() -> bool {
    pi_js::env::var("PI_PACKAGE_DIR").filter(|s| !s.is_empty()).is_none() && has_exe_dir_package_json().is_some()
}

pub fn get_themes_dir() -> String {
    if is_binary_layout() {
        pi_js::path::join(&[&get_package_dir(), "theme"])
    } else {
        pi_js::path::join(&[env!("CARGO_MANIFEST_DIR"), "src", "modes", "interactive", "theme"])
    }
}

pub fn get_export_template_dir() -> String {
    if is_binary_layout() {
        pi_js::path::join(&[&get_package_dir(), "export-html"])
    } else {
        pi_js::path::join(&[env!("CARGO_MANIFEST_DIR"), "src", "core", "export_html"])
    }
}

pub fn get_package_json_path() -> String {
    pi_js::path::join(&[&get_package_dir(), "package.json"])
}

pub fn get_readme_path() -> String {
    pi_js::path::resolve(&[&get_package_dir(), "README.md"])
}

pub fn get_docs_path() -> String {
    pi_js::path::resolve(&[&get_package_dir(), "docs"])
}

pub fn get_examples_path() -> String {
    pi_js::path::resolve(&[&get_package_dir(), "examples"])
}

pub fn get_changelog_path() -> String {
    pi_js::path::resolve(&[&get_package_dir(), "CHANGELOG.md"])
}

pub fn get_interactive_assets_dir() -> String {
    if is_binary_layout() {
        pi_js::path::join(&[&get_package_dir(), "assets"])
    } else {
        pi_js::path::join(&[env!("CARGO_MANIFEST_DIR"), "src", "modes", "interactive", "assets"])
    }
}

pub fn get_bundled_interactive_asset_path(name: &str) -> String {
    pi_js::path::join(&[&get_interactive_assets_dir(), name])
}

pub fn set_embedded_quickjs_wasm_path(path: &str) {
    if let Ok(mut guard) = EMBEDDED_QUICKJS_WASM_PATH.lock() {
        *guard = Some(path.to_owned());
    }
}

pub fn get_quickjs_wasm_path() -> Option<String> {
    EMBEDDED_QUICKJS_WASM_PATH.lock().ok().and_then(|guard| guard.clone())
}

pub fn expand_tilde_path(path: &str) -> String {
    normalize_path(path, Default::default())
}

pub fn get_share_viewer_url(gist_id: &str) -> String {
    let base_url = pi_js::env::var("PI_SHARE_VIEWER_URL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SHARE_VIEWER_URL.to_owned());
    format!("{base_url}#{gist_id}")
}

pub fn get_agent_dir() -> String {
    if let Some(env_dir) = pi_js::env::var(&ENV_AGENT_DIR).filter(|s| !s.is_empty()) {
        return expand_tilde_path(&env_dir);
    }
    pi_js::path::join(&[&pi_js::env::home_dir(), &CONFIG_DIR_NAME, "agent"])
}

pub fn get_custom_themes_dir() -> String {
    pi_js::path::join(&[&get_agent_dir(), "themes"])
}

pub fn get_models_path() -> String {
    pi_js::path::join(&[&get_agent_dir(), "models.json"])
}

pub fn get_auth_path() -> String {
    pi_js::path::join(&[&get_agent_dir(), "auth.json"])
}

pub fn get_settings_path() -> String {
    pi_js::path::join(&[&get_agent_dir(), "settings.json"])
}

pub fn get_tools_dir() -> String {
    pi_js::path::join(&[&get_agent_dir(), "tools"])
}

pub fn get_bin_dir() -> String {
    pi_js::path::join(&[&get_agent_dir(), "bin"])
}

pub fn get_prompts_dir() -> String {
    pi_js::path::join(&[&get_agent_dir(), "prompts"])
}

pub fn get_sessions_dir() -> String {
    pi_js::path::join(&[&get_agent_dir(), "sessions"])
}

pub fn get_debug_log_path() -> String {
    pi_js::path::join(&[&get_agent_dir(), &format!("{}-debug.log", *APP_NAME)])
}

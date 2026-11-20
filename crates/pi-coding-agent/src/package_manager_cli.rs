//! Port of packages/coding-agent/src/package-manager-cli.ts

use std::io::IsTerminal;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use indexmap::IndexSet;
use pi_ai::models::{ModelsError, ModelsErrorCode, ModelsRefreshOptions};
use pi_js::abort::AbortController;
use pi_js::console::{error as console_error, log as console_log, stdout_write};
use pi_js::env::{cwd as env_cwd, pid as env_pid, platform as env_platform, var as env_var, vars as env_vars};
use pi_js::fetch::{Headers, Request, fetch};
use pi_js::fs::{exists, mkdir, mkdtemp, read_to_string, readdir, rename, rm, write_file};
use pi_js::path::{join, resolve};
use pi_js::time::now_ms;
use pi_js::uri::encode_uri_component;
use pi_js::{BoxFuture, Error, Result};
use pi_tui::components::markdown::{Markdown, MarkdownTheme};
use regex::Regex;
use serde_json::Value;

use crate::config::{
    APP_NAME, CONFIG_DIR_NAME, InstallChange, InstallMethod, PACKAGE_NAME, SelfUpdateCommand, SelfUpdatePackageTarget,
    VERSION, detect_install_change, detect_install_method, get_agent_dir, get_configured_self_update_argv,
    get_package_dir, get_self_update_command, get_self_update_unavailable_instruction,
    make_configured_self_update_command,
};
use crate::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use crate::core::package_manager::{
    DefaultPackageManager, PackageCommandRunner, PackageManagerOptions, ProgressAction, ProgressEventType,
    ResolvedPaths, is_offline_mode_enabled,
};
use crate::core::project_trust::{
    AppMode, ProjectTrustContext, ProjectTrustExtensionHost, ProjectTrustUi, ResolveProjectTrustedOptions,
    RunnerProjectTrustExtensionHost, resolve_project_trusted,
};
use crate::core::resource_loader::{
    CoreExtensionLoaderHost, DefaultResourceLoader, DefaultResourceLoaderOptions, InlineExtension,
    format_package_install_warning, is_builtin_extension,
};
use crate::core::settings_manager::{SettingsManager, SettingsManagerCreateOptions, SettingsScope};
use crate::core::trust_manager::{ProjectTrustStore, has_trust_requiring_project_resources};
use crate::utils::child_process::{
    SpawnProcessOptions, SyncProcessOutput, spawn_process, spawn_process_sync, wait_for_child_process,
};
use crate::utils::paths::{canonicalize_path, get_cwd_relative_path};
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::text::strip_bom;
use crate::utils::version_check::{
    VersionCheckOptions, format_version_check_error, get_latest_pi_release, is_newer_package_version,
};
use crate::utils::windows_self_update::{
    cleanup_windows_self_update_quarantine, quarantine_windows_native_dependencies,
};
use crate::vendor::chalk::{bold, chalk, cyan, dim, green, italic, red, underline, yellow};
use crate::vendor::proper_lockfile::{LockOptions, lock, lock_sync};

const DEFAULT_INSTALLER_API_BASE: &str = "https://pi.dev/api/installer/releases";
const MANAGED_INSTALL_MARKER: &str = "managed-install.json";

static MANAGED_RELEASE_VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$").expect("valid semver regex")
});

/// Exit code equivalent for `process.exitCode` (avoids calling `std::process::exit` in library code).
static PROCESS_EXIT_CODE: AtomicI32 = AtomicI32::new(0);

pub fn process_exit_code() -> i32 {
    PROCESS_EXIT_CODE.load(Ordering::SeqCst)
}

pub fn set_process_exit_code(code: i32) {
    PROCESS_EXIT_CODE.store(code, Ordering::SeqCst);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageCommand {
    Install,
    Remove,
    Update,
    List,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateTarget {
    All,
    SelfUpdate,
    Extensions { source: Option<String> },
    Models,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageCommandOptions {
    pub command: PackageCommand,
    pub source: Option<String>,
    pub update_target: Option<UpdateTarget>,
    pub show_extensions_skipped_note: bool,
    pub local: bool,
    pub force: bool,
    pub project_trust_override: Option<bool>,
    pub help: bool,
    pub invalid_option: Option<String>,
    pub invalid_argument: Option<String>,
    pub missing_option_value: Option<String>,
    pub conflicting_options: Option<String>,
}

/// Inputs forwarded to `select_config` when handling `pi config`.
#[derive(Clone)]
pub struct ConfigSelectorInput {
    pub resolved_paths_global: ResolvedPaths,
    pub resolved_paths_project: ResolvedPaths,
    pub settings_manager: SettingsManager,
    pub cwd: String,
    pub agent_dir: String,
    pub write_scope: SettingsScope,
    pub project_mode_available: bool,
}

pub type SelectConfigFn = Arc<dyn Fn(ConfigSelectorInput) -> BoxFuture<Result<()>> + Send + Sync>;
pub type CreateProjectTrustContextFn =
    Arc<dyn Fn(&str, AppMode, &SettingsManager, bool) -> Arc<dyn ProjectTrustContext> + Send + Sync>;

/// Optional hooks and injected extensions for CLI package & config commands.
#[derive(Clone, Default)]
pub struct PackageCommandRuntimeOptions {
    pub extension_factories: Vec<InlineExtension>,
    pub select_config: Option<SelectConfigFn>,
    pub create_project_trust_context: Option<CreateProjectTrustContextFn>,
    pub trust_extension_host: Option<Arc<dyn ProjectTrustExtensionHost>>,
    pub app_entrypoint: Option<String>,
    pub columns: Option<usize>,
}

struct NoopProjectTrustUi;

#[async_trait::async_trait]
impl ProjectTrustUi for NoopProjectTrustUi {
    async fn select(&self, _prompt: &str, _options: &[String]) -> Option<String> {
        None
    }
}

struct DefaultCommandProjectTrustContext {
    mode: AppMode,
    has_ui: bool,
    ui: Arc<NoopProjectTrustUi>,
}

#[async_trait::async_trait]
impl ProjectTrustContext for DefaultCommandProjectTrustContext {
    fn has_ui(&self) -> bool {
        self.has_ui
    }

    fn ui(&self) -> &dyn ProjectTrustUi {
        self.ui.as_ref()
    }

    fn ui_arc(&self) -> Option<Arc<dyn ProjectTrustUi>> {
        Some(self.ui.clone())
    }

    fn mode(&self) -> AppMode {
        self.mode
    }
}

fn default_create_project_trust_context(
    _cwd: &str,
    mode: AppMode,
    _settings_manager: &SettingsManager,
    has_ui: bool,
) -> Arc<dyn ProjectTrustContext> {
    Arc::new(DefaultCommandProjectTrustContext {
        mode,
        has_ui,
        ui: Arc::new(NoopProjectTrustUi),
    })
}

fn get_active_managed_install_root() -> Result<Option<String>> {
    let Some(configured_root) = env_var("PI_MANAGED_INSTALL_ROOT").map(|s| s.trim().to_owned()) else {
        return Ok(None);
    };
    if configured_root.is_empty() {
        return Ok(None);
    }

    let managed_root = resolve(&[&configured_root]);
    let releases_dir = canonicalize_path(&join(&[&managed_root, "releases"]));
    if get_cwd_relative_path(&canonicalize_path(&get_package_dir()), &releases_dir).is_none() {
        return Ok(None);
    }

    let marker_path = join(&[&managed_root, MANAGED_INSTALL_MARKER]);
    let raw = match read_to_string(&marker_path) {
        Ok(content) => content,
        Err(_) => {
            return Err(Error::msg(format!(
                "Managed install marker is missing or invalid: {marker_path}"
            )));
        }
    };
    let parsed: Value = match serde_json::from_str(strip_bom(&raw)) {
        Ok(v) => v,
        Err(_) => {
            return Err(Error::msg(format!(
                "Managed install marker is missing or invalid: {marker_path}"
            )));
        }
    };
    let valid = parsed.get("kind").and_then(Value::as_str) == Some("pi-managed-install")
        && parsed.get("schemaVersion").and_then(Value::as_i64) == Some(1)
        && parsed.get("layout").and_then(Value::as_str) == Some("releases-v1");
    if !valid {
        return Err(Error::msg(format!(
            "Managed install marker is missing or invalid: {marker_path}"
        )));
    }

    Ok(Some(managed_root))
}

async fn fetch_installer_artifact(url: &str, label: &str) -> Result<String> {
    let mut headers = Headers::new();
    headers.set("User-Agent", &get_pi_user_agent(*VERSION));
    let req = Request {
        method: "GET".to_owned(),
        url: url.to_owned(),
        headers,
        body: None,
        signal: None,
    };
    let response = fetch(req).await?;
    if !response.ok() {
        return Err(Error::msg(format!(
            "Could not download managed installer {label} from {url}: HTTP {}",
            response.status()
        )));
    }
    response.text().await
}

#[async_trait::async_trait]
pub trait CliProcessRunner: Send + Sync {
    async fn run_spawn(&self, command: &str, args: &[&str], cwd: Option<&str>) -> Result<Option<i32>>;
    fn run_spawn_sync(&self, command: &str, args: &[&str]) -> SyncProcessOutput;
    fn package_command_runner(&self) -> Option<Arc<dyn PackageCommandRunner>> {
        None
    }
}

async fn run_managed_npm_ci(stage_dir: &str, runner: Option<&dyn CliProcessRunner>) -> Result<()> {
    // No age-gate override: the release lockfile pins every package, so npm never
    // resolves versions and a configured min-release-age does not apply.
    let args = [
        "ci",
        "--ignore-scripts",
        "--omit=dev",
        "--include=optional",
        "--no-fund",
        "--no-audit",
        "--loglevel=error",
        "--progress=false",
    ];
    let code = if let Some(runner) = runner {
        runner.run_spawn("npm", &args, Some(stage_dir)).await?
    } else {
        let child = spawn_process(
            "npm",
            &args,
            SpawnProcessOptions {
                cwd: Some(stage_dir.to_owned()),
                env: None,
                stdin_piped: false,
                stdout_piped: false,
                stderr_piped: false,
                windows_hide: false,
                detached: false,
            },
        )?;
        wait_for_child_process(child, None, None).await?
    };
    if code != Some(0) {
        let code_str = code.map_or_else(|| "unknown".to_owned(), |c| c.to_string());
        return Err(Error::msg(format!(
            "npm {} exited with code {code_str}",
            args.join(" ")
        )));
    }
    Ok(())
}

fn verify_managed_release(
    release_dir: &str,
    expected_version: &str,
    runner: Option<&dyn CliProcessRunner>,
) -> Result<()> {
    let bin_name = if env_platform() == "win32" {
        format!("{}.cmd", *APP_NAME)
    } else {
        (*APP_NAME).to_owned()
    };
    let bin_path = join(&[release_dir, "node_modules", ".bin", &bin_name]);
    let result = if let Some(runner) = runner {
        runner.run_spawn_sync(&bin_path, &["--version"])
    } else {
        spawn_process_sync(
            &bin_path,
            &["--version"],
            SpawnProcessOptions {
                cwd: None,
                env: None,
                stdin_piped: false,
                stdout_piped: true,
                stderr_piped: true,
                windows_hide: false,
                detached: false,
            },
        )
    };
    if result.error.is_some() || result.status != Some(0) {
        let reason = result
            .error
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let s = result.stderr.trim();
                (!s.is_empty()).then(|| s.to_owned())
            })
            .unwrap_or_else(|| {
                let code_str = result.status.map_or_else(|| "unknown".to_owned(), |c| c.to_string());
                format!("exit code {code_str}")
            });
        return Err(Error::msg(format!(
            "Could not verify managed Pi {expected_version}: {reason}"
        )));
    }
    let installed_version = result.stdout.trim();
    if installed_version != expected_version {
        return Err(Error::msg(format!(
            "Managed Pi smoke test returned version {installed_version}; expected {expected_version}."
        )));
    }
    Ok(())
}

fn activate_managed_release(managed_root: &str, version: &str) -> Result<()> {
    let current_path = join(&[managed_root, "current-version"]);
    let temporary_path = join(&[managed_root, &format!("current-version.tmp.{}-{}", env_pid(), now_ms())]);
    let write_res = (|| {
        write_file(&temporary_path, format!("{version}\n"), None)?;
        rename(&temporary_path, &current_path)
    })();
    let _ = rm(&temporary_path, false, true);
    write_res
}

// Keep the active release and the one running this update, which other open
// sessions likely still use and which allows rolling back by editing current-version.
fn prune_managed_releases(managed_root: &str, active_version: &str) {
    let releases_root = join(&[managed_root, "releases"]);
    let Ok(entries) = readdir(&releases_root) else {
        return;
    };
    for entry in entries {
        if entry == active_version || entry == *VERSION || !MANAGED_RELEASE_VERSION_RE.is_match(&entry) {
            continue;
        }
        let _ = rm(&join(&[&releases_root, &entry]), true, true);
    }
}

fn cleanup_managed_staging(managed_root: &str) {
    let staging_root = join(&[managed_root, "staging"]);
    if let Ok(entries) = readdir(&staging_root) {
        for entry in entries {
            if entry.starts_with("update-") {
                let _ = rm(&join(&[&staging_root, &entry]), true, true);
            }
        }
    }
}

pub fn cleanup_managed_install() {
    let Ok(Some(managed_root)) = get_active_managed_install_root() else {
        return;
    };

    let Ok(release_lock) = lock_sync(
        &join(&[&managed_root, "update"]),
        LockOptions {
            realpath: Some(false),
            ..Default::default()
        },
    ) else {
        return;
    };

    cleanup_managed_staging(&managed_root);
    let _ = release_lock.release_sync();
}

async fn run_managed_self_update(
    managed_root: &str,
    version: &str,
    runner: Option<&dyn CliProcessRunner>,
) -> Result<()> {
    if !MANAGED_RELEASE_VERSION_RE.is_match(version) {
        return Err(Error::msg(format!("Invalid managed release version: {version}")));
    }

    let release_lock = match lock(
        &join(&[managed_root, "update"]),
        LockOptions {
            realpath: Some(false),
            ..Default::default()
        },
    )
    .await
    {
        Ok(guard) => guard,
        Err(err) => {
            if err.to_string().contains("ELOCKED") {
                return Err(Error::msg("Another managed Pi update is already running."));
            }
            return Err(err);
        }
    };

    let mut stage_dir: Option<String> = None;
    let body_res = run_managed_self_update_inner(managed_root, version, &mut stage_dir, runner).await;

    if let Some(dir) = stage_dir {
        let _ = rm(&dir, true, true);
    }
    let _ = release_lock.release().await;
    body_res
}

async fn run_managed_self_update_inner(
    managed_root: &str,
    version: &str,
    stage_dir: &mut Option<String>,
    runner: Option<&dyn CliProcessRunner>,
) -> Result<()> {
    cleanup_managed_staging(managed_root);
    let installer_base = env_var("PI_INSTALLER_API_BASE")
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_INSTALLER_API_BASE.to_owned());
    let installer_base = installer_base.trim_end_matches('/');
    let release_url = format!("{installer_base}/{}", encode_uri_component(version));
    let staging_root = join(&[managed_root, "staging"]);
    let releases_root = join(&[managed_root, "releases"]);
    mkdir(&releases_root, true, None)?;
    let release_dir = join(&[&releases_root, version]);
    if exists(&release_dir) {
        verify_managed_release(&release_dir, version, runner)?;
        activate_managed_release(managed_root, version)?;
        prune_managed_releases(managed_root, version);
        return Ok(());
    }

    mkdir(&staging_root, true, None)?;
    let created_stage = mkdtemp(&join(&[&staging_root, "update-"]))?;
    *stage_dir = Some(created_stage.clone());

    let pkg_json_url = format!("{release_url}/package.json");
    let pkg_lock_url = format!("{release_url}/package-lock.json");
    let (pkg_json, pkg_lock) = tokio::try_join!(
        fetch_installer_artifact(&pkg_json_url, "package.json"),
        fetch_installer_artifact(&pkg_lock_url, "package-lock.json"),
    )?;
    write_file(&join(&[&created_stage, "package.json"]), pkg_json, None)?;
    write_file(&join(&[&created_stage, "package-lock.json"]), pkg_lock, None)?;

    run_managed_npm_ci(&created_stage, runner).await?;
    verify_managed_release(&created_stage, version, runner)?;
    rename(&created_stage, &release_dir)?;
    activate_managed_release(managed_root, version)?;
    prune_managed_releases(managed_root, version);
    Ok(())
}

fn self_update_note_markdown_theme() -> MarkdownTheme {
    MarkdownTheme {
        heading: Arc::new(|text| bold(&yellow(text))),
        link: Arc::new(cyan),
        link_url: Arc::new(dim),
        code: Arc::new(yellow),
        code_block: Arc::new(dim),
        code_block_border: Arc::new(dim),
        quote: Arc::new(dim),
        quote_border: Arc::new(dim),
        hr: Arc::new(dim),
        list_bullet: Arc::new(yellow),
        bold: Arc::new(bold),
        italic: Arc::new(italic),
        strikethrough: Arc::new(|text| chalk().strikethrough().apply(text)),
        underline: Arc::new(underline),
        highlight_code: None,
        code_block_indent: None,
    }
}

fn report_settings_errors(settings_manager: &SettingsManager, context: &str) {
    let errors = settings_manager.drain_errors();
    for err in errors {
        let scope_str = match err.scope {
            SettingsScope::Global => "global",
            SettingsScope::Project => "project",
        };
        console_error(&yellow(&format!(
            "Warning ({context}, {scope_str} settings): {}",
            err.error
        )));
    }
}

fn get_package_command_usage(command: PackageCommand) -> String {
    let app = *APP_NAME;
    match command {
        PackageCommand::Install => format!("{app} install <source> [-l] [--approve|--no-approve]"),
        PackageCommand::Remove => format!("{app} remove <source> [-l] [--approve|--no-approve]"),
        PackageCommand::Update => format!(
            "{app} update [source|self|pi] [--self|--extensions|--models|--all] [--extension <source>] [--approve|--no-approve] [--force]"
        ),
        PackageCommand::List => format!("{app} list [--approve|--no-approve]"),
    }
}

fn config_command_usage() -> String {
    format!("{} config [-l] [--approve|--no-approve]", *APP_NAME)
}

fn print_config_command_help() {
    let usage = config_command_usage();
    let cfg = *CONFIG_DIR_NAME;
    console_log(&format!(
        "{}
  {usage}

Open the resource configuration TUI to enable or disable package resources.
Without -l, starts in global settings (~/{cfg}/agent/settings.json).
Press Tab in the TUI to switch between global and project-local modes.

Options:
  -l, --local       Edit project overrides ({cfg}/settings.json)
  -a, --approve     Trust project-local files for this command with -l
  -na, --no-approve Ignore project-local files for this command with -l
",
        bold("Usage:")
    ));
}

fn print_package_command_help(command: PackageCommand) {
    let app = *APP_NAME;
    let cfg = *CONFIG_DIR_NAME;
    let usage = get_package_command_usage(command);
    match command {
        PackageCommand::Install => console_log(&format!(
            "{}
  {usage}

Install a package and add it to settings.

Options:
  -l, --local       Install project-locally ({cfg}/settings.json)
  -a, --approve     Trust project-local files for this command
  -na, --no-approve Ignore project-local files for this command

Examples:
  {app} install npm:@foo/bar
  {app} install git:github.com/user/repo
  {app} install git:git@github.com:user/repo
  {app} install https://github.com/user/repo
  {app} install ssh://git@github.com/user/repo
  {app} install ./local/path
",
            bold("Usage:")
        )),
        PackageCommand::Remove => console_log(&format!(
            "{}
  {usage}

Remove a package and its source from settings.
Alias: {app} uninstall <source> [-l]

Options:
  -l, --local       Remove from project settings ({cfg}/settings.json)
  -a, --approve     Trust project-local files for this command
  -na, --no-approve Ignore project-local files for this command

Examples:
  {app} remove npm:@foo/bar
  {app} uninstall npm:@foo/bar
",
            bold("Usage:")
        )),
        PackageCommand::Update => console_log(&format!(
            "{}
  {usage}

Update pi, installed packages, or model catalogs.

Options:
  --self                  Update pi only (default when no target is given)
  --extensions            Update installed packages only
  --models                Refresh model catalogs only
  --all                   Update pi and installed packages
  --extension <source>    Update one package only
  -a, --approve           Trust project-local files for this command
  -na, --no-approve       Ignore project-local files for this command
  --force                 Reinstall pi even if the current version is latest

Short forms:
  {app} update                Update pi only
  {app} update --all          Update pi and all extensions
  {app} update --models       Refresh model catalogs only
  {app} update <source>       Update one package
  {app} update pi             Update pi only (self works as alias to pi)
",
            bold("Usage:")
        )),
        PackageCommand::List => console_log(&format!(
            "{}
  {usage}

List installed packages from user and project settings.

Options:
  -a, --approve      Trust project-local files for this command
  -na, --no-approve  Ignore project-local files for this command
",
            bold("Usage:")
        )),
    }
}

pub fn parse_package_command(args: &[String]) -> Option<PackageCommandOptions> {
    let raw_command = args.first()?.as_str();
    let command = match raw_command {
        "uninstall" => PackageCommand::Remove,
        "install" => PackageCommand::Install,
        "remove" => PackageCommand::Remove,
        "update" => PackageCommand::Update,
        "list" => PackageCommand::List,
        _ => return None,
    };

    let rest = &args[1..];
    let mut local = false;
    let mut force = false;
    let mut project_trust_override: Option<bool> = None;
    let mut help = false;
    let mut invalid_option: Option<String> = None;
    let mut invalid_argument: Option<String> = None;
    let mut missing_option_value: Option<String> = None;
    let mut conflicting_options: Option<String> = None;
    let mut source: Option<String> = None;
    let mut self_flag = false;
    let mut extensions_flag = false;
    let mut models_flag = false;
    let mut all_flag = false;
    let mut extension_flag_source: Option<String> = None;

    let mut index = 0usize;
    while index < rest.len() {
        let arg = &rest[index];
        if arg == "-h" || arg == "--help" {
            help = true;
            index += 1;
            continue;
        }
        if arg == "-l" || arg == "--local" {
            if command == PackageCommand::Install || command == PackageCommand::Remove {
                local = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg.clone());
            }
            index += 1;
            continue;
        }
        if arg == "--self" {
            if command == PackageCommand::Update {
                self_flag = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg.clone());
            }
            index += 1;
            continue;
        }
        if arg == "--extensions" {
            if command == PackageCommand::Update {
                extensions_flag = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg.clone());
            }
            index += 1;
            continue;
        }
        if arg == "--models" {
            if command == PackageCommand::Update {
                models_flag = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg.clone());
            }
            index += 1;
            continue;
        }
        if arg == "--all" {
            if command == PackageCommand::Update {
                all_flag = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg.clone());
            }
            index += 1;
            continue;
        }
        if arg == "--approve" || arg == "-a" {
            project_trust_override = Some(true);
            index += 1;
            continue;
        }
        if arg == "--no-approve" || arg == "-na" {
            project_trust_override = Some(false);
            index += 1;
            continue;
        }
        if arg == "--force" {
            if command == PackageCommand::Update {
                force = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg.clone());
            }
            index += 1;
            continue;
        }
        if arg == "--extension" {
            if command != PackageCommand::Update {
                if invalid_option.is_none() {
                    invalid_option = Some(arg.clone());
                }
                index += 1;
                continue;
            }
            let next_val = rest.get(index + 1);
            if next_val.is_none_or(|v| v.starts_with('-')) {
                if missing_option_value.is_none() {
                    missing_option_value = Some(arg.clone());
                }
                index += 1;
            } else if extension_flag_source.is_some() {
                if conflicting_options.is_none() {
                    conflicting_options = Some("--extension can only be provided once".to_owned());
                }
                index += 2;
            } else if let Some(val) = next_val {
                extension_flag_source = Some(val.clone());
                index += 2;
            }
            continue;
        }
        if arg.starts_with('-') {
            if invalid_option.is_none() {
                invalid_option = Some(arg.clone());
            }
            index += 1;
            continue;
        }
        if source.is_none() {
            source = Some(arg.clone());
        } else if invalid_argument.is_none() {
            invalid_argument = Some(arg.clone());
        }
        index += 1;
    }

    let mut update_target: Option<UpdateTarget> = None;
    let mut show_extensions_skipped_note = false;
    if command == PackageCommand::Update {
        if all_flag
            && (self_flag || extensions_flag || models_flag || extension_flag_source.is_some())
            && conflicting_options.is_none()
        {
            conflicting_options =
                Some("--all cannot be combined with --self, --extensions, --models, or --extension".to_owned());
        }
        if all_flag && source.is_some() && conflicting_options.is_none() {
            conflicting_options = Some("--all cannot be combined with a positional source".to_owned());
        }

        if models_flag {
            if (self_flag || extensions_flag || all_flag || extension_flag_source.is_some())
                && conflicting_options.is_none()
            {
                conflicting_options =
                    Some("--models cannot be combined with --self, --extensions, --all, or --extension".to_owned());
            }
            if source.is_some() && conflicting_options.is_none() {
                conflicting_options = Some("--models cannot be combined with a positional source".to_owned());
            }
            update_target = Some(UpdateTarget::Models);
        } else if let Some(ref ext_source) = extension_flag_source {
            if (self_flag || extensions_flag || all_flag) && conflicting_options.is_none() {
                conflicting_options =
                    Some("--extension cannot be combined with --self, --extensions, or --all".to_owned());
            }
            if source.is_some() && conflicting_options.is_none() {
                conflicting_options = Some("--extension cannot be combined with a positional source".to_owned());
            }
            update_target = Some(UpdateTarget::Extensions {
                source: Some(ext_source.clone()),
            });
        } else if let Some(ref pos_source) = source {
            let source_is_self = pos_source == "self" || pos_source == "pi";
            if source_is_self {
                update_target = Some(if extensions_flag {
                    UpdateTarget::All
                } else {
                    UpdateTarget::SelfUpdate
                });
            } else {
                if (extensions_flag || self_flag || all_flag) && conflicting_options.is_none() {
                    conflicting_options = Some(
                        "positional update targets cannot be combined with --self, --extensions, or --all".to_owned(),
                    );
                }
                update_target = Some(UpdateTarget::Extensions {
                    source: Some(pos_source.clone()),
                });
            }
        } else if all_flag || (self_flag && extensions_flag) {
            update_target = Some(UpdateTarget::All);
        } else if self_flag {
            update_target = Some(UpdateTarget::SelfUpdate);
        } else if extensions_flag {
            update_target = Some(UpdateTarget::Extensions { source: None });
        } else {
            update_target = Some(UpdateTarget::SelfUpdate);
            show_extensions_skipped_note = true;
        }
    }

    Some(PackageCommandOptions {
        command,
        source,
        update_target,
        show_extensions_skipped_note,
        local,
        force,
        project_trust_override,
        help,
        invalid_option,
        invalid_argument,
        missing_option_value,
        conflicting_options,
    })
}

fn update_target_includes_self(target: &UpdateTarget) -> bool {
    matches!(target, UpdateTarget::All | UpdateTarget::SelfUpdate)
}

fn update_target_includes_extensions(target: &UpdateTarget) -> bool {
    matches!(target, UpdateTarget::All | UpdateTarget::Extensions { .. })
}

async fn refresh_model_catalogs(agent_dir: &str) -> Result<()> {
    let controller = Arc::new(AbortController::new());
    let signal = controller.signal();
    let timer_ctrl = controller.clone();
    let timer_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(15_000)).await;
        timer_ctrl.abort(None);
    });

    let run_res = async {
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            auth_path: Some(join(&[agent_dir, "auth.json"])),
            models_path: Some(Some(join(&[agent_dir, "models.json"]))),
            allow_model_network: Some(false),
            signal: Some(signal.clone()),
            ..Default::default()
        })
        .await?;
        let result = model_runtime
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: None,
                force: Some(true),
                signal: Some(signal),
            }))
            .await?;
        if result.aborted {
            return Err(Error::msg("Model catalog refresh timed out."));
        }
        if !result.errors.is_empty() {
            let mut failures = Vec::new();
            for (provider, error) in &result.errors {
                // A stale login only affects that provider's catalog; the others are still worth refreshing.
                if let Some(merr) = error.downcast_ref::<ModelsError>()
                    && merr.code == ModelsErrorCode::OAuth
                {
                    console_error(&yellow(&format!(
                        "Warning: skipped model catalog for {provider}: {}\nLog in again with `{} auth login {provider}` to refresh it.",
                        merr.message, *APP_NAME
                    )));
                } else {
                    failures.push(format!("{provider}: {error}"));
                }
            }
            if !failures.is_empty() {
                return Err(Error::msg(format!(
                    "Could not refresh model catalogs: {}",
                    failures.join("; ")
                )));
            }
        }
        Ok(())
    }
    .await;

    timer_task.abort();
    run_res?;
    console_log(&green("Model catalogs refreshed"));
    Ok(())
}

fn print_self_update_unavailable(
    npm_command: Option<&[String]>,
    update_package_target: SelfUpdatePackageTarget,
    app_entrypoint: Option<&str>,
) {
    let app = *APP_NAME;
    console_error(&format!("error: {app} cannot self-update this installation."));
    console_error(&get_self_update_unavailable_instruction(
        *PACKAGE_NAME,
        npm_command,
        Some(update_package_target),
    ));
    console_error(&format!(
        "To make `{app} update` run your own update command, set PI_SELF_UPDATE_COMMAND or the selfUpdateCommand setting."
    ));

    let fallback_entry = std::env::args().nth(1);
    let entrypoint = app_entrypoint.or(fallback_entry.as_deref());
    if let Some(entry) = entrypoint
        && !entry.is_empty()
    {
        console_error("");
        console_error(&format!("Location of {app} executable: {entry}"));
    }
}

fn print_self_update_fallback(command: &SelfUpdateCommand) {
    console_error(&dim(&format!(
        "If this keeps failing, run this command yourself: {}",
        command.display
    )));
}

fn print_pnpm_self_update_metadata_hint() {
    console_error(&yellow(
        "If pnpm reports missing package versions, its cached registry metadata may be stale.",
    ));
    console_error(&yellow(&format!(
        "Run `pnpm store prune` and retry `{} update --self`.",
        *APP_NAME
    )));
}

fn print_self_update_note(note: &str, columns: Option<usize>) {
    let trimmed = note.trim();
    if trimmed.is_empty() {
        return;
    }

    console_log("");
    console_log(&bold(&yellow("Update note")));
    let width = columns
        .or_else(|| env_var("COLUMNS").and_then(|s| s.parse::<usize>().ok()))
        .unwrap_or(80)
        .max(20);
    let md = Markdown::new(trimmed, 0, 0, self_update_note_markdown_theme(), None, None);
    let lines: Vec<String> = md.render(width).into_iter().map(|l| l.trim_end().to_owned()).collect();
    console_log(&lines.join("\n"));
    console_log("");
}

struct SelfUpdatePlan {
    package_name: String,
    install_spec: String,
    version: String,
    should_run: bool,
    note: Option<String>,
}

async fn get_self_update_plan(force: bool) -> Result<SelfUpdatePlan> {
    let latest_release = match get_latest_pi_release(
        *VERSION,
        VersionCheckOptions {
            timeout_ms: None,
            retry: true,
        },
    )
    .await
    {
        Ok(rel) => rel,
        Err(err) => {
            return Err(Error::msg(format!(
                "Could not determine latest {} version: {}",
                *APP_NAME,
                format_version_check_error(&err)
            )));
        }
    };

    let Some(latest) = latest_release else {
        return Err(Error::msg(format!("Could not determine latest {} version.", *APP_NAME)));
    };

    let package_name = latest.package_name.unwrap_or_else(|| (*PACKAGE_NAME).to_owned());
    let install_spec = format!("{package_name}@{}", latest.version);
    if force || package_name != *PACKAGE_NAME || is_newer_package_version(&latest.version, *VERSION) {
        return Ok(SelfUpdatePlan {
            package_name,
            install_spec,
            version: latest.version,
            should_run: true,
            note: latest.note,
        });
    }

    console_log(&green(&format!("{} is already up to date (v{})", *APP_NAME, *VERSION)));
    Ok(SelfUpdatePlan {
        package_name,
        install_spec,
        version: latest.version,
        should_run: false,
        note: None,
    })
}

async fn run_self_update(command: &SelfUpdateCommand, runner: Option<&dyn CliProcessRunner>) -> Result<()> {
    console_log(&dim(&format!("Updating {} with {}...", *APP_NAME, command.display)));
    let steps: Vec<(&str, &[String], &str)> = match &command.steps {
        Some(steps) => steps
            .iter()
            .map(|s| (s.command.as_str(), s.args.as_slice(), s.display.as_str()))
            .collect(),
        None => vec![(
            command.command.as_str(),
            command.args.as_slice(),
            command.display.as_str(),
        )],
    };

    for (cmd, args, display) in steps {
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let code = if let Some(runner) = runner {
            runner.run_spawn(cmd, &arg_refs, None).await?
        } else {
            let child = spawn_process(
                cmd,
                &arg_refs,
                SpawnProcessOptions {
                    cwd: None,
                    env: None,
                    stdin_piped: false,
                    stdout_piped: false,
                    stderr_piped: false,
                    windows_hide: false,
                    detached: false,
                },
            )?;
            wait_for_child_process(child, None, None).await?
        };
        if code != Some(0) {
            let code_str = code.map_or_else(|| "unknown".to_owned(), |c| c.to_string());
            return Err(Error::msg(format!("{display} exited with code {code_str}")));
        }
    }
    Ok(())
}

/// Run the user-configured self-update command, then report the version installed on disk.
async fn run_configured_self_update(
    argv: &[String],
    plan: &SelfUpdatePlan,
    runner: Option<&dyn CliProcessRunner>,
    columns: Option<usize>,
) {
    let command = make_configured_self_update_command(argv, &plan.package_name, &plan.version);
    if let Some(ref note) = plan.note {
        print_self_update_note(note, columns);
    }
    if let Err(err) = run_self_update(&command, runner).await {
        console_error(&red(&format!("Error: {err}")));
        set_process_exit_code(1);
        return;
    }
    // Exit code 0 does not prove an update. Wrappers such as mise or Nix install each version into a
    // new directory and leave this installation untouched, so fall back to the pi on PATH.
    let change = detect_install_change(None);
    let installed_version = if let Some(InstallChange::Updated(ver)) = change
        && ver == plan.version
    {
        Some(ver)
    } else {
        read_version_on_path(runner)
    };
    if installed_version.as_deref() == Some(plan.version.as_str()) {
        console_log(&green(&format!(
            "Updated {} from {} to {}",
            *APP_NAME, *VERSION, plan.version
        )));
        return;
    }
    let reported = installed_version.as_deref().unwrap_or("no version");
    console_log(&yellow(&format!(
        "Ran {}, but `{} --version` reports {reported}, not {}.",
        command.display, *APP_NAME, plan.version
    )));
}

/// Version printed by `pi --version` as resolved from PATH, the pi the user runs next.
fn read_version_on_path(runner: Option<&dyn CliProcessRunner>) -> Option<String> {
    let result = if let Some(runner) = runner {
        runner.run_spawn_sync(*APP_NAME, &["--version"])
    } else {
        // PI_PACKAGE_DIR points at this installation and would make the new pi report the old version.
        let mut env = env_vars();
        env.shift_remove("PI_PACKAGE_DIR");
        spawn_process_sync(
            *APP_NAME,
            &["--version"],
            SpawnProcessOptions {
                cwd: None,
                env: Some(env),
                stdin_piped: false,
                stdout_piped: true,
                stderr_piped: true,
                windows_hide: false,
                detached: false,
            },
        )
    };
    if result.status == Some(0) {
        let trimmed = result.stdout.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    } else {
        None
    }
}

fn prepare_windows_npm_self_update() {
    if env_platform() != "win32" {
        return;
    }
    let package_dir = get_package_dir();
    cleanup_windows_self_update_quarantine(&package_dir);
    quarantine_windows_native_dependencies(&package_dir);
}

fn get_command_app_mode() -> AppMode {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        AppMode::Interactive
    } else {
        AppMode::Print
    }
}

fn report_project_trust_warnings(warnings: &[String]) {
    for warning in warnings {
        console_error(&yellow(&format!("Warning: {warning}")));
    }
}

struct CommandSettingsResult {
    settings_manager: SettingsManager,
    project_trust_warnings: Vec<String>,
}

async fn create_command_settings_manager(
    cwd: &str,
    agent_dir: &str,
    project_trust_override: Option<bool>,
    use_saved_project_trust_only: bool,
    runtime_options: &PackageCommandRuntimeOptions,
) -> CommandSettingsResult {
    let settings_manager = SettingsManager::create(
        cwd,
        Some(agent_dir),
        Some(SettingsManagerCreateOptions {
            project_trusted: Some(false),
        }),
    );
    let mut project_trust_warnings = Vec::new();
    let trust_store = ProjectTrustStore::new(agent_dir);
    if use_saved_project_trust_only {
        let saved_trusted = matches!(trust_store.get(cwd), Ok(Some(true)));
        settings_manager.set_project_trusted(project_trust_override.unwrap_or(saved_trusted));
        return CommandSettingsResult {
            settings_manager,
            project_trust_warnings,
        };
    }

    let app_mode = get_command_app_mode();
    let mut default_trust_host: Option<Arc<dyn ProjectTrustExtensionHost>> = None;
    if project_trust_override.is_none() && has_trust_requiring_project_resources(cwd) {
        let core_host = Arc::new(CoreExtensionLoaderHost::new());
        let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.to_owned(),
            agent_dir: agent_dir.to_owned(),
            settings_manager: Some(settings_manager.clone()),
            extension_factories: runtime_options.extension_factories.clone(),
            extension_host: Some(core_host.clone()),
            ..Default::default()
        });
        match loader.load_project_trust_extensions().await {
            Ok(ext_res) => {
                for err in ext_res.errors {
                    project_trust_warnings.push(format!("Failed to load extension \"{}\": {}", err.path, err.error));
                }
                if let Some(real_ext_res) = core_host.loaded_extensions_result() {
                    default_trust_host = Some(Arc::new(RunnerProjectTrustExtensionHost::new(real_ext_res)));
                }
            }
            Err(err) => {
                project_trust_warnings.push(format!("Failed to load extensions: {err}"));
            }
        }
    }

    let ctx = match &runtime_options.create_project_trust_context {
        Some(factory) => factory(cwd, app_mode, &settings_manager, app_mode == AppMode::Interactive),
        None => {
            default_create_project_trust_context(cwd, app_mode, &settings_manager, app_mode == AppMode::Interactive)
        }
    };
    let on_err = |msg: &str| {
        console_error(&yellow(&format!("Warning: {msg}")));
    };
    let project_trusted = resolve_project_trusted(ResolveProjectTrustedOptions {
        cwd,
        trust_store: &trust_store,
        trust_override: project_trust_override,
        default_project_trust: Some(settings_manager.get_default_project_trust()),
        extension_host: runtime_options
            .trust_extension_host
            .as_deref()
            .or(default_trust_host.as_deref()),
        project_trust_context: ctx.as_ref(),
        on_extension_error: Some(&on_err),
    })
    .await;
    settings_manager.set_project_trusted(project_trusted);
    CommandSettingsResult {
        settings_manager,
        project_trust_warnings,
    }
}

pub async fn handle_config_command(args: &[String], runtime_options: PackageCommandRuntimeOptions) -> bool {
    if args.first().map(String::as_str) != Some("config") {
        return false;
    }

    let rest = &args[1..];
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        print_config_command_help();
        return true;
    }

    let mut local = false;
    let mut project_trust_override: Option<bool> = None;
    for arg in rest {
        if arg == "-l" || arg == "--local" {
            local = true;
        } else if arg == "-a" || arg == "--approve" {
            project_trust_override = Some(true);
        } else if arg == "-na" || arg == "--no-approve" {
            project_trust_override = Some(false);
        } else if arg.starts_with('-') {
            console_error(&red(&format!("Unknown option {arg} for \"config\".")));
            console_error(&dim(&format!(
                "Use \"{} --help\" or \"{}\".",
                *APP_NAME,
                config_command_usage()
            )));
            set_process_exit_code(1);
            return true;
        } else {
            console_error(&red(&format!("Unexpected argument {arg}.")));
            console_error(&dim(&format!("Usage: {}", config_command_usage())));
            set_process_exit_code(1);
            return true;
        }
    }

    let cwd = env_cwd();
    let agent_dir = get_agent_dir();
    let CommandSettingsResult {
        settings_manager,
        project_trust_warnings,
    } = create_command_settings_manager(&cwd, &agent_dir, project_trust_override, false, &runtime_options).await;
    report_project_trust_warnings(&project_trust_warnings);
    if local && !settings_manager.is_project_trusted() {
        console_error(&red(
            "Project is not trusted. Use --approve to modify local resource config.",
        ));
        set_process_exit_code(1);
        return true;
    }
    report_settings_errors(&settings_manager, "config command");

    let builtin_extensions: Vec<String> = runtime_options
        .extension_factories
        .iter()
        .filter(|ext| is_builtin_extension(ext))
        .filter_map(|ext| match ext {
            InlineExtension::Named(named) => Some(named.name.clone()),
            InlineExtension::Core(crate::core::extensions::types::InlineExtension::Named(named)) => {
                Some(named.name.clone())
            }
            InlineExtension::Factory(_) | InlineExtension::Core(_) => None,
        })
        .collect();

    let global_settings_manager = SettingsManager::create(
        &cwd,
        Some(&agent_dir),
        Some(SettingsManagerCreateOptions {
            project_trusted: Some(false),
        }),
    );
    let global_resolved_paths = match DefaultPackageManager::new(PackageManagerOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        settings_manager: global_settings_manager,
        builtin_extensions: builtin_extensions.clone(),
    })
    .resolve(None)
    .await
    {
        Ok(res) => res,
        Err(err) => {
            console_error(&red(&format!("Error: {err}")));
            set_process_exit_code(1);
            return true;
        }
    };

    let project_resolved_paths = if settings_manager.is_project_trusted() {
        match DefaultPackageManager::new(PackageManagerOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            settings_manager: settings_manager.clone(),
            builtin_extensions,
        })
        .resolve(None)
        .await
        {
            Ok(res) => res,
            Err(err) => {
                console_error(&red(&format!("Error: {err}")));
                set_process_exit_code(1);
                return true;
            }
        }
    } else {
        global_resolved_paths.clone()
    };

    let mut install_warnings: IndexSet<String> = IndexSet::new();
    for failure in global_resolved_paths
        .install_failures
        .iter()
        .flatten()
        .chain(project_resolved_paths.install_failures.iter().flatten())
    {
        install_warnings.insert(format_package_install_warning(failure));
    }
    for warning in install_warnings {
        console_error(&yellow(&format!("Warning: {warning}")));
    }

    let flush_settings = settings_manager.clone();
    if let Some(select_config) = runtime_options.select_config {
        let input = ConfigSelectorInput {
            resolved_paths_global: global_resolved_paths,
            resolved_paths_project: project_resolved_paths,
            settings_manager,
            cwd,
            agent_dir,
            write_scope: if local {
                SettingsScope::Project
            } else {
                SettingsScope::Global
            },
            project_mode_available: flush_settings.is_project_trusted(),
        };
        if let Err(err) = select_config(input).await {
            flush_settings.flush().await;
            console_error(&red(&format!("Error: {err}")));
            set_process_exit_code(1);
            return true;
        }
    }

    flush_settings.flush().await;
    set_process_exit_code(0);
    true
}

pub async fn handle_package_command(args: &[String], runtime_options: PackageCommandRuntimeOptions) -> bool {
    handle_package_command_with_runner(args, runtime_options, None).await
}

pub async fn handle_package_command_with_runner(
    args: &[String],
    runtime_options: PackageCommandRuntimeOptions,
    process_runner: Option<Arc<dyn CliProcessRunner>>,
) -> bool {
    let Some(options) = parse_package_command(args) else {
        return false;
    };

    if options.help {
        print_package_command_help(options.command);
        return true;
    }

    if let Some(ref invalid_option) = options.invalid_option {
        let cmd_str = match options.command {
            PackageCommand::Install => "install",
            PackageCommand::Remove => "remove",
            PackageCommand::Update => "update",
            PackageCommand::List => "list",
        };
        console_error(&red(&format!("Unknown option {invalid_option} for \"{cmd_str}\".")));
        console_error(&dim(&format!(
            "Use \"{} --help\" or \"{}\".",
            *APP_NAME,
            get_package_command_usage(options.command)
        )));
        set_process_exit_code(1);
        return true;
    }

    if let Some(ref missing_option) = options.missing_option_value {
        console_error(&red(&format!("Missing value for {missing_option}.")));
        console_error(&dim(&format!("Usage: {}", get_package_command_usage(options.command))));
        set_process_exit_code(1);
        return true;
    }

    if let Some(ref invalid_arg) = options.invalid_argument {
        console_error(&red(&format!("Unexpected argument {invalid_arg}.")));
        console_error(&dim(&format!("Usage: {}", get_package_command_usage(options.command))));
        set_process_exit_code(1);
        return true;
    }

    if let Some(ref conflicting) = options.conflicting_options {
        console_error(&red(conflicting));
        console_error(&dim(&format!("Usage: {}", get_package_command_usage(options.command))));
        set_process_exit_code(1);
        return true;
    }

    if matches!(options.command, PackageCommand::Install | PackageCommand::Remove) && options.source.is_none() {
        let cmd_str = if options.command == PackageCommand::Install {
            "install"
        } else {
            "remove"
        };
        console_error(&red(&format!("Missing {cmd_str} source.")));
        console_error(&dim(&format!("Usage: {}", get_package_command_usage(options.command))));
        set_process_exit_code(1);
        return true;
    }

    if options.command == PackageCommand::Update && matches!(options.update_target, Some(UpdateTarget::Models)) {
        if let Err(err) = refresh_model_catalogs(&get_agent_dir()).await {
            console_error(&red(&format!("Error: {err}")));
            set_process_exit_code(1);
        }
        return true;
    }

    let cwd = env_cwd();
    let agent_dir = get_agent_dir();
    let writes_project_package_config =
        matches!(options.command, PackageCommand::Install | PackageCommand::Remove) && options.local;

    let CommandSettingsResult {
        settings_manager,
        project_trust_warnings,
    } = create_command_settings_manager(
        &cwd,
        &agent_dir,
        options.project_trust_override,
        options.command == PackageCommand::Update,
        &runtime_options,
    )
    .await;
    report_project_trust_warnings(&project_trust_warnings);

    if !settings_manager.is_project_trusted() && writes_project_package_config {
        console_error(&red(
            "Project is not trusted. Use --approve to modify local package config.",
        ));
        set_process_exit_code(1);
        return true;
    }
    report_settings_errors(&settings_manager, "package command");

    let self_update_npm_command = settings_manager.get_global_settings().npm_command;
    let configured_self_update_argv =
        get_configured_self_update_argv(settings_manager.get_global_settings().self_update_command.as_deref());
    let flush_settings = settings_manager.clone();

    let package_manager = DefaultPackageManager::new(PackageManagerOptions {
        cwd,
        agent_dir,
        settings_manager,
        builtin_extensions: Vec::new(),
    });
    if let Some(pkg_runner) = process_runner
        .as_deref()
        .and_then(|runner| runner.package_command_runner())
    {
        package_manager.set_command_runner(Some(pkg_runner));
    }
    // Update result events describe a changed package, e.g. "npm:foo 1.2.0 -> 1.3.0".
    let updated_packages = Arc::new(Mutex::new(Vec::<String>::new()));
    // Progress messages are warnings, e.g. npm reinstalled the old version from stale registry metadata.
    let warning_count = Arc::new(AtomicUsize::new(0));
    {
        let updated_packages = Arc::clone(&updated_packages);
        let warning_count = Arc::clone(&warning_count);
        package_manager.set_progress_callback(Some(Arc::new(move |event| {
            if event.event_type == ProgressEventType::Result
                && event.action == ProgressAction::Update
                && let Some(ref msg) = event.message
            {
                updated_packages
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(msg.clone());
            } else if event.event_type == ProgressEventType::Start {
                if let Some(ref msg) = event.message {
                    stdout_write(&dim(&format!("{msg}\n")));
                }
            } else if event.event_type == ProgressEventType::Progress
                && let Some(ref msg) = event.message
            {
                warning_count.fetch_add(1, Ordering::Relaxed);
                stdout_write(&yellow(&format!("{msg}\n")));
            }
        })));
    }

    let run_result: Result<()> = (|| async {
        match options.command {
            PackageCommand::Install => {
                let source = options.source.as_deref().unwrap_or_default();
                package_manager.install_and_persist(source, options.local).await?;
                console_log(&green(&format!("Installed {source}")));
                Ok(())
            }
            PackageCommand::Remove => {
                let source = options.source.as_deref().unwrap_or_default();
                let removed = package_manager.remove_and_persist(source, options.local).await?;
                if !removed {
                    console_error(&red(&format!("No matching package found for {source}")));
                    set_process_exit_code(1);
                    return Ok(());
                }
                console_log(&green(&format!("Removed {source}")));
                Ok(())
            }
            PackageCommand::List => {
                let configured = package_manager.list_configured_packages()?;
                let user_packages: Vec<_> = configured
                    .iter()
                    .filter(|p| {
                        matches!(
                            p.scope,
                            crate::core::package_manager::InstalledSourceScope::User
                        )
                    })
                    .collect();
                let project_packages: Vec<_> = configured
                    .iter()
                    .filter(|p| {
                        matches!(
                            p.scope,
                            crate::core::package_manager::InstalledSourceScope::Project
                        )
                    })
                    .collect();

                if configured.is_empty() {
                    console_log(&dim("No packages installed."));
                    return Ok(());
                }

                let print_pkg = |pkg: &crate::core::package_manager::ConfiguredPackage| {
                    let display = if pkg.filtered {
                        format!("{} (filtered)", pkg.source)
                    } else {
                        pkg.source.clone()
                    };
                    console_log(&format!("  {display}"));
                    if let Some(ref installed_path) = pkg.installed_path {
                        console_log(&dim(&format!("    {installed_path}")));
                    }
                };

                if !user_packages.is_empty() {
                    console_log(&bold("User packages:"));
                    for pkg in &user_packages {
                        print_pkg(pkg);
                    }
                }
                if !project_packages.is_empty() {
                    if !user_packages.is_empty() {
                        console_log("");
                    }
                    console_log(&bold("Project packages:"));
                    for pkg in &project_packages {
                        print_pkg(pkg);
                    }
                }
                Ok(())
            }
            PackageCommand::Update => {
                let target = options
                    .update_target
                    .as_ref()
                    .unwrap_or(&UpdateTarget::SelfUpdate);
                if options.show_extensions_skipped_note {
                    console_log(&dim(&format!(
                        "Extensions are skipped. Run {} update --extensions to update extensions.",
                        *APP_NAME
                    )));
                }
                if update_target_includes_extensions(target) {
                    let update_source = match target {
                        UpdateTarget::Extensions { source } => source.as_deref(),
                        _ => None,
                    };
                    package_manager.update(update_source).await?;
                    let updated_list = updated_packages.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let warn_cnt = warning_count.load(Ordering::Relaxed);
                    // Only claim "up to date" when the update actually checked and nothing warned.
                    if is_offline_mode_enabled() {
                        console_log(&yellow("Skipped package updates (offline mode)"));
                    } else if updated_list.is_empty() && warn_cnt > 0 {
                        console_log(&yellow(&if let Some(src) = update_source {
                            format!("{src} was not updated, see the warnings above")
                        } else {
                            "No packages were updated, see the warnings above".to_owned()
                        }));
                    } else if updated_list.is_empty() {
                        console_log(&green(&if let Some(src) = update_source {
                            format!("{src} is already up to date")
                        } else {
                            "Packages are already up to date".to_owned()
                        }));
                    } else {
                        let count = updated_list.len();
                        console_log(&green(&format!(
                            "Updated {count} package{}:",
                            if count == 1 { "" } else { "s" }
                        )));
                        for message in &updated_list {
                            console_log(&format!("  {message}"));
                        }
                    }
                }
                if update_target_includes_self(target) {
                    // A configured command is explicit user intent, so it replaces the managed install path too.
                    let managed_install_root = if configured_self_update_argv.is_some() {
                        None
                    } else {
                        get_active_managed_install_root()?
                    };
                    if managed_install_root.is_some() && options.force {
                        console_error(&red(&format!(
                            "Managed {} installations do not support --force; rerun the installer to repair this installation.",
                            *APP_NAME
                        )));
                        set_process_exit_code(1);
                        return Ok(());
                    }

                    let plan = get_self_update_plan(options.force).await?;
                    if !plan.should_run {
                        return Ok(());
                    }
                    if let Some(ref argv) = configured_self_update_argv {
                        run_configured_self_update(
                            argv,
                            &plan,
                            process_runner.as_deref(),
                            runtime_options.columns,
                        )
                        .await;
                        return Ok(());
                    }
                    if let Some(ref managed_root) = managed_install_root {
                        if let Some(ref note) = plan.note {
                            print_self_update_note(note, runtime_options.columns);
                        }
                        console_log(&dim(&format!(
                            "Updating managed {} installation...",
                            *APP_NAME
                        )));
                        if let Err(err) =
                            run_managed_self_update(managed_root, &plan.version, process_runner.as_deref()).await
                        {
                            console_error(&red(&format!("Error: {err}")));
                            set_process_exit_code(1);
                            return Ok(());
                        }
                        console_log(&green(&format!(
                            "Updated {} from {} to {}",
                            *APP_NAME, *VERSION, plan.version
                        )));
                        return Ok(());
                    }

                    let install_method = detect_install_method();
                    if env_platform() == "win32"
                        && install_method != InstallMethod::Npm
                        && install_method != InstallMethod::Pnpm
                    {
                        console_error(&red(&format!(
                            "{} self-update on Windows is only supported for npm and pnpm installs.",
                            *APP_NAME
                        )));
                        let method_str = match install_method {
                            InstallMethod::BunBinary => "bun-binary",
                            InstallMethod::Npm => "npm",
                            InstallMethod::Pnpm => "pnpm",
                            InstallMethod::Yarn => "yarn",
                            InstallMethod::Bun => "bun",
                            InstallMethod::Unknown => "unknown",
                        };
                        console_error(&dim(&format!(
                            "Detected install method: {method_str}. Update {} manually.",
                            *APP_NAME
                        )));
                        set_process_exit_code(1);
                        return Ok(());
                    }

                    let self_update_target = SelfUpdatePackageTarget {
                        package_name: plan.package_name.clone(),
                        install_spec: plan.install_spec.clone(),
                    };
                    let Some(self_update_command) = get_self_update_command(
                        *PACKAGE_NAME,
                        self_update_npm_command.as_deref(),
                        Some(self_update_target.clone()),
                    ) else {
                        print_self_update_unavailable(
                            self_update_npm_command.as_deref(),
                            self_update_target,
                            runtime_options.app_entrypoint.as_deref(),
                        );
                        set_process_exit_code(1);
                        return Ok(());
                    };
                    if let Some(ref note) = plan.note {
                        print_self_update_note(note, runtime_options.columns);
                    }
                    if install_method == InstallMethod::Npm {
                        prepare_windows_npm_self_update();
                    }
                    if let Err(err) =
                        run_self_update(&self_update_command, process_runner.as_deref()).await
                    {
                        console_error(&red(&format!("Error: {err}")));
                        if install_method == InstallMethod::Pnpm {
                            print_pnpm_self_update_metadata_hint();
                        }
                        print_self_update_fallback(&self_update_command);
                        set_process_exit_code(1);
                        return Ok(());
                    }
                    console_log(&green(&format!(
                        "Updated {} from {} to {}",
                        *APP_NAME, *VERSION, plan.version
                    )));
                    // The pi.dev installer migrates global npm installs to a managed install
                    // that pins all dependencies. It does not migrate pnpm, yarn, or bun installs.
                    if install_method == InstallMethod::Npm {
                        let installer_command = if env_platform() == "win32" {
                            "powershell -c \"irm https://pi.dev/install.ps1 | iex\""
                        } else {
                            "curl -fsSL https://pi.dev/install.sh | sh"
                        };
                        console_log("");
                        console_log(&yellow(&format!(
                            "This npm installation of {} does not pin its dependencies.",
                            *APP_NAME
                        )));
                        console_log(&yellow(
                            "Run the installer to migrate to a managed installation that does:",
                        ));
                        console_log("");
                        console_log(&format!("  {}", bold(installer_command)));
                    }
                }
                Ok(())
            }
        }
    })()
    .await;

    flush_settings.flush().await;

    if let Err(err) = run_result {
        console_error(&red(&format!("Error: {err}")));
        set_process_exit_code(1);
    }

    true
}

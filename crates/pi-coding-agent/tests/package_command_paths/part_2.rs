//! Port of packages/coding-agent/test/package-command-paths.test.ts (unit coding-agent-package-manager-cli-startup-ui)

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_coding_agent::config::VERSION;
use pi_coding_agent::core::package_manager::PackageCommandRunner;
use pi_coding_agent::core::settings_manager::SettingsScope;
use pi_coding_agent::package_manager_cli::{
    CliProcessRunner, ConfigSelectorInput, PackageCommand, PackageCommandRuntimeOptions, UpdateTarget,
    cleanup_managed_install, handle_config_command, handle_package_command, handle_package_command_with_runner,
    parse_package_command, process_exit_code, set_process_exit_code,
};
use pi_coding_agent::utils::child_process::SyncProcessOutput;
use pi_js::Error;
use pi_js::console::testing::capture;
use pi_js::env::testing::{CwdGuard, EnvGuard};
use pi_js::fetch::testing::mock_fetch;
use pi_js::fetch::{Headers, Response};
use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;

struct CliFixture {
    _temp_dir: TempDir,
    root: PathBuf,
    agent_dir: PathBuf,
    cwd: PathBuf,
}

impl CliFixture {
    fn new() -> Self {
        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let root = temp_dir.path().to_path_buf();
        let agent_dir = root.join("agent");
        let cwd = root.join("project");
        fs::create_dir_all(&agent_dir).expect("mkdir agent");
        fs::create_dir_all(&cwd).expect("mkdir cwd");
        Self {
            _temp_dir: temp_dir,
            root,
            agent_dir,
            cwd,
        }
    }
}

struct ExitCodeResetGuard;

impl ExitCodeResetGuard {
    fn new() -> Self {
        set_process_exit_code(0);
        Self
    }
}

impl Drop for ExitCodeResetGuard {
    fn drop(&mut self) {
        set_process_exit_code(0);
    }
}

struct MockCliPkgRunner {
    commands: Mutex<Vec<(String, Vec<String>, Option<String>)>>,
}

impl MockCliPkgRunner {
    fn new() -> Self {
        Self {
            commands: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl PackageCommandRunner for MockCliPkgRunner {
    async fn run_command(&self, command: &str, args: &[&str], cwd: Option<&str>) -> pi_js::Result<()> {
        self.commands.lock().unwrap_or_else(PoisonError::into_inner).push((
            command.to_owned(),
            args.iter().map(|s| (*s).to_owned()).collect(),
            cwd.map(str::to_owned),
        ));
        if command == "npm" && args.first() == Some(&"install") {
            if let Some(prefix_idx) = args.iter().position(|a| *a == "--prefix")
                && let Some(root) = args.get(prefix_idx + 1)
            {
                let pkg_dir = Path::new(root).join("node_modules/@foo/bar");
                fs::create_dir_all(pkg_dir.join("extensions")).expect("mkdir pkg ext");
                fs::write(pkg_dir.join("package.json"), r#"{"name":"@foo/bar","version":"1.0.0"}"#)
                    .expect("write pkg.json");
                fs::write(pkg_dir.join("extensions/index.ts"), "// ext").expect("write ext");
            }
        }
        Ok(())
    }

    async fn run_command_capture(
        &self,
        _command: &str,
        _args: &[&str],
        _cwd: Option<&str>,
        _timeout_ms: Option<u64>,
        _extra_env: Option<&IndexMap<String, String>>,
    ) -> pi_js::Result<String> {
        Ok(String::new())
    }

    fn run_command_sync(&self, _command: &str, _args: &[&str]) -> pi_js::Result<String> {
        Ok(String::new())
    }
}

struct MockCliRunner {
    pkg_runner: Arc<MockCliPkgRunner>,
    spawns: Mutex<Vec<(String, Vec<String>, Option<String>)>>,
    sync_version: String,
    fail_spawn_code: Option<i32>,
}

impl MockCliRunner {
    fn new(sync_version: &str) -> Self {
        Self {
            pkg_runner: Arc::new(MockCliPkgRunner::new()),
            spawns: Mutex::new(Vec::new()),
            sync_version: sync_version.to_owned(),
            fail_spawn_code: None,
        }
    }

    fn recorded_spawns(&self) -> Vec<(String, Vec<String>, Option<String>)> {
        self.spawns.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[async_trait::async_trait]
impl CliProcessRunner for MockCliRunner {
    async fn run_spawn(&self, command: &str, args: &[&str], cwd: Option<&str>) -> pi_js::Result<Option<i32>> {
        self.spawns.lock().unwrap_or_else(PoisonError::into_inner).push((
            command.to_owned(),
            args.iter().map(|s| (*s).to_owned()).collect(),
            cwd.map(str::to_owned),
        ));
        if let Some(code) = self.fail_spawn_code {
            return Ok(Some(code));
        }
        Ok(Some(0))
    }

    fn run_spawn_sync(&self, _command: &str, _args: &[&str]) -> SyncProcessOutput {
        SyncProcessOutput {
            status: Some(0),
            stdout: format!("{}\n", self.sync_version),
            stderr: String::new(),
            error: None,
            error_code: None,
        }
    }

    fn package_command_runner(&self) -> Option<Arc<dyn PackageCommandRunner>> {
        Some(self.pkg_runner.clone())
    }
}

#[test]
fn parses_package_commands_targets_and_validation_errors() {
    let args = |parts: &[&str]| -> Vec<String> { parts.iter().map(|s| (*s).to_owned()).collect() };

    assert!(parse_package_command(&args(&["config"])).is_none());

    let plain_update = parse_package_command(&args(&["update"])).unwrap();
    assert_eq!(plain_update.command, PackageCommand::Update);
    assert_eq!(plain_update.update_target, Some(UpdateTarget::SelfUpdate));
    assert!(plain_update.show_extensions_skipped_note);

    let self_update = parse_package_command(&args(&["update", "--self"])).unwrap();
    assert_eq!(self_update.update_target, Some(UpdateTarget::SelfUpdate));
    assert!(!self_update.show_extensions_skipped_note);

    let ext_all = parse_package_command(&args(&["update", "--extensions"])).unwrap();
    assert_eq!(ext_all.update_target, Some(UpdateTarget::Extensions { source: None }));

    let all = parse_package_command(&args(&["update", "--all"])).unwrap();
    assert_eq!(all.update_target, Some(UpdateTarget::All));

    let models = parse_package_command(&args(&["update", "--models"])).unwrap();
    assert_eq!(models.update_target, Some(UpdateTarget::Models));

    let single_ext = parse_package_command(&args(&["update", "--extension", "npm:foo"])).unwrap();
    assert_eq!(
        single_ext.update_target,
        Some(UpdateTarget::Extensions {
            source: Some("npm:foo".to_owned())
        })
    );

    let pos_pi = parse_package_command(&args(&["update", "pi"])).unwrap();
    assert_eq!(pos_pi.update_target, Some(UpdateTarget::SelfUpdate));

    let pos_pkg = parse_package_command(&args(&["update", "npm:bar"])).unwrap();
    assert_eq!(
        pos_pkg.update_target,
        Some(UpdateTarget::Extensions {
            source: Some("npm:bar".to_owned())
        })
    );

    let conflict = parse_package_command(&args(&["update", "--all", "--models"])).unwrap();
    assert!(conflict.conflicting_options.is_some());

    let missing_val = parse_package_command(&args(&["update", "--extension"])).unwrap();
    assert_eq!(missing_val.missing_option_value.as_deref(), Some("--extension"));

    let uninstall = parse_package_command(&args(&["uninstall", "npm:foo", "-l", "-a"])).unwrap();
    assert_eq!(uninstall.command, PackageCommand::Remove);
    assert!(uninstall.local);
    assert_eq!(uninstall.project_trust_override, Some(true));
    assert_eq!(uninstall.source.as_deref(), Some("npm:foo"));
}

#[tokio::test]
#[serial]
async fn handles_validation_help_trust_gate_install_list_remove_and_config() {
    let _exit_guard = ExitCodeResetGuard::new();
    let fx = CliFixture::new();
    let _agent_guard = EnvGuard::set("PI_CODING_AGENT_DIR", Some(fx.agent_dir.to_str().unwrap()));
    let _cwd_guard = CwdGuard::set(fx.cwd.to_string_lossy().into_owned());

    // Help output
    {
        let cap = capture();
        let handled = handle_package_command(
            &["install".to_owned(), "--help".to_owned()],
            PackageCommandRuntimeOptions::default(),
        )
        .await;
        assert!(handled);
        assert_eq!(process_exit_code(), 0);
        assert!(cap.stdout().contains("Install a package and add it to settings"));
    }

    // Missing source -> exit code 1
    {
        let cap = capture();
        set_process_exit_code(0);
        assert!(handle_package_command(&["install".to_owned()], PackageCommandRuntimeOptions::default(),).await);
        assert_eq!(process_exit_code(), 1);
        assert!(cap.stderr().contains("Missing install source."));
    }

    // Untrusted project refuses `install -l` without `--approve`
    {
        fs::create_dir_all(fx.cwd.join(".pi")).unwrap();
        fs::write(fx.cwd.join(".pi/settings.json"), "{}").unwrap();
        let cap = capture();
        set_process_exit_code(0);
        assert!(
            handle_package_command(
                &["install".to_owned(), "-l".to_owned(), "npm:@foo/bar".to_owned()],
                PackageCommandRuntimeOptions::default(),
            )
            .await
        );
        assert_eq!(process_exit_code(), 1);
        assert!(cap.stderr().contains("Project is not trusted. Use --approve"));
    }

    // With `--approve`, `install -l -a npm:@foo/bar` succeeds, then `list`, then `remove -l -a`
    {
        let runner = Arc::new(MockCliRunner::new(*VERSION));
        let cap = capture();
        set_process_exit_code(0);
        assert!(
            handle_package_command_with_runner(
                &[
                    "install".to_owned(),
                    "-l".to_owned(),
                    "-a".to_owned(),
                    "npm:@foo/bar".to_owned(),
                ],
                PackageCommandRuntimeOptions::default(),
                Some(runner.clone()),
            )
            .await
        );
        assert_eq!(process_exit_code(), 0);
        assert!(cap.stdout().contains("Installed npm:@foo/bar"));

        let list_cap = capture();
        assert!(
            handle_package_command_with_runner(
                &["list".to_owned(), "--approve".to_owned()],
                PackageCommandRuntimeOptions::default(),
                Some(runner.clone()),
            )
            .await
        );
        assert!(list_cap.stdout().contains("Project packages:"));
        assert!(list_cap.stdout().contains("npm:@foo/bar"));

        let rm_cap = capture();
        assert!(
            handle_package_command_with_runner(
                &[
                    "remove".to_owned(),
                    "-l".to_owned(),
                    "-a".to_owned(),
                    "npm:@foo/bar".to_owned(),
                ],
                PackageCommandRuntimeOptions::default(),
                Some(runner),
            )
            .await
        );
        assert_eq!(process_exit_code(), 0);
        assert!(rm_cap.stdout().contains("Removed npm:@foo/bar"));
    }

    // `handle_config_command` enforces trust gate and passes `ConfigSelectorInput` to hook
    // // WIRE(@tui-w3): tests `select_config` invocation without interactive `ConfigSelectorComponent`
    {
        let received = Arc::new(Mutex::new(Option::<(SettingsScope, bool)>::None));
        let received_ref = received.clone();
        let handled = handle_config_command(
            &["config".to_owned(), "-l".to_owned(), "--approve".to_owned()],
            PackageCommandRuntimeOptions {
                select_config: Some(Arc::new(move |input: ConfigSelectorInput| {
                    *received_ref.lock().unwrap() = Some((input.write_scope, input.project_mode_available));
                    Box::pin(async { Ok(()) })
                })),
                ..Default::default()
            },
        )
        .await;
        assert!(handled);
        assert_eq!(process_exit_code(), 0);
        assert_eq!(*received.lock().unwrap(), Some((SettingsScope::Project, true)));
    }
}

#[tokio::test]
#[serial]
async fn update_models_refreshes_catalogs_and_reports_errors() {
    let _exit_guard = ExitCodeResetGuard::new();
    let fx = CliFixture::new();
    let _agent_guard = EnvGuard::set("PI_CODING_AGENT_DIR", Some(fx.agent_dir.to_str().unwrap()));

    // Success path
    {
        let _mock = mock_fetch(|req| {
            let url = req.url.clone();
            Box::pin(async move {
                let mut headers = Headers::new();
                headers.set("content-type", "application/json");
                let body = if url.contains("openrouter.ai") {
                    pi_js::json::stringify(&json!({ "data": [] }))
                } else {
                    "{}".to_owned()
                };
                Ok(Response::new(200, headers, body))
            })
        });
        let cap = capture();
        set_process_exit_code(0);
        let handled = handle_package_command(
            &["update".to_owned(), "--models".to_owned()],
            PackageCommandRuntimeOptions::default(),
        )
        .await;
        assert!(handled);
        assert_eq!(process_exit_code(), 0);
        assert!(cap.stdout().contains("Model catalogs refreshed"));
    }

    // Failure path when remote fetch rejects with configured OpenRouter credential
    {
        let _or_key = EnvGuard::set("OPENROUTER_API_KEY", Some("test-or-key"));
        let _mock = mock_fetch(|req| {
            let url = req.url.clone();
            Box::pin(async move {
                if url.contains("openrouter") {
                    Err(Error::msg("simulated network failure"))
                } else {
                    let mut headers = Headers::new();
                    headers.set("content-type", "application/json");
                    Ok(Response::new(200, headers, "{}"))
                }
            })
        });
        let cap = capture();
        set_process_exit_code(0);
        let handled = handle_package_command(
            &["update".to_owned(), "--models".to_owned()],
            PackageCommandRuntimeOptions::default(),
        )
        .await;
        assert!(handled);
        assert_eq!(process_exit_code(), 1);
        assert!(
            cap.stderr().contains("Could not refresh model catalogs"),
            "stderr was: {}",
            cap.stderr()
        );
    }
}

#[tokio::test]
#[serial]
async fn handles_managed_and_standard_self_updates_and_cleanup() {
    let _exit_guard = ExitCodeResetGuard::new();
    let fx = CliFixture::new();
    let _agent_guard = EnvGuard::set("PI_CODING_AGENT_DIR", Some(fx.agent_dir.to_str().unwrap()));
    let _cwd_guard = CwdGuard::set(fx.cwd.to_string_lossy().into_owned());

    // Set up managed install layout
    let managed_root = fx.root.join("managed");
    let current_rel = managed_root
        .join("releases")
        .join(*VERSION)
        .join("node_modules/@earendil-works/pi-coding-agent");
    fs::create_dir_all(&current_rel).unwrap();
    fs::write(
        managed_root.join("managed-install.json"),
        pi_js::json::stringify(&json!({
            "kind": "pi-managed-install",
            "schemaVersion": 1,
            "layout": "releases-v1"
        })),
    )
    .unwrap();
    let stale_stage = managed_root.join("staging/update-stale");
    fs::create_dir_all(&stale_stage).unwrap();

    let _managed_env = EnvGuard::set("PI_MANAGED_INSTALL_ROOT", Some(managed_root.to_str().unwrap()));
    let _pkg_env = EnvGuard::set("PI_PACKAGE_DIR", Some(current_rel.to_str().unwrap()));
    let _api_env = EnvGuard::set("PI_INSTALLER_API_BASE", Some("https://installer.example.test/releases"));

    // cleanup_managed_install removes stale staging/update-* directories
    cleanup_managed_install();
    assert!(!stale_stage.exists());

    // Managed installs forbid --force
    {
        let cap = capture();
        set_process_exit_code(0);
        handle_package_command(
            &["update".to_owned(), "--force".to_owned()],
            PackageCommandRuntimeOptions::default(),
        )
        .await;
        assert_eq!(process_exit_code(), 1);
        assert!(cap.stderr().contains("do not support --force"));
    }

    // Managed self-update to newer version fetches package.json + lock, runs npm ci, verifies bin, activates
    let next_ver = "99.0.0";
    {
        let _mock = mock_fetch(move |req| {
            let url = req.url.clone();
            Box::pin(async move {
                let mut headers = Headers::new();
                headers.set("content-type", "application/json");
                if url == "https://pi.dev/api/latest-version" {
                    return Ok(Response::new(
                        200,
                        headers,
                        pi_js::json::stringify(&json!({
                            "version": next_ver,
                            "packageName": "@earendil-works/pi-coding-agent",
                            "note": "Release notes here"
                        })),
                    ));
                }
                if url == format!("https://installer.example.test/releases/{next_ver}/package.json") {
                    return Ok(Response::new(200, headers, r#"{"name":"managed-release"}"#));
                }
                if url == format!("https://installer.example.test/releases/{next_ver}/package-lock.json") {
                    return Ok(Response::new(200, headers, r#"{"lockfileVersion":3}"#));
                }
                Err(Error::msg(format!("unexpected fetch {url}")))
            })
        });

        let runner = Arc::new(MockCliRunner::new(next_ver));
        let cap = capture();
        set_process_exit_code(0);
        handle_package_command_with_runner(
            &["update".to_owned()],
            PackageCommandRuntimeOptions::default(),
            Some(runner.clone()),
        )
        .await;
        assert_eq!(process_exit_code(), 0, "stderr: {}", cap.stderr());
        let out = cap.stdout();
        assert!(out.contains("Extensions are skipped"));
        assert!(out.contains("Updating managed pi installation"));
        assert!(out.contains(&format!("Updated pi from {} to {next_ver}", *VERSION)));
        assert_eq!(
            fs::read_to_string(managed_root.join("current-version")).unwrap().trim(),
            next_ver
        );
        assert!(
            managed_root
                .join("releases")
                .join(next_ver)
                .join("package.json")
                .exists()
        );
        assert!(
            runner
                .recorded_spawns()
                .iter()
                .any(|(cmd, args, _)| { cmd == "npm" && args.first().map(String::as_str) == Some("ci") })
        );
    }

    let _: Value = json!({});
}
